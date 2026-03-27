use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use uuid::Uuid;

/// An MCP Tool represents a remote capability hosted by a Model Context Protocol server.
pub struct McpTool {
    name: String,
    description: String,
    parameters: Value,
    server_url: String,
    client: Client,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let result = call_mcp(
            &self.client,
            &self.server_url,
            "tools/call",
            Some(json!({
                "name": self.name,
                "arguments": args,
            })),
        )
        .await
        .with_context(|| {
            format!(
                "Failed to execute MCP tool '{}' via {}",
                self.name, self.server_url
            )
        })?;

        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let message = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("MCP tool returned an error");
            bail!("{}", message);
        }

        if let Some(structured) = result.get("structuredContent") {
            return Ok(serde_json::to_string_pretty(structured)?);
        }

        if let Some(items) = result.get("content").and_then(Value::as_array) {
            let text = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Ok(text);
            }
        }

        Ok(serde_json::to_string_pretty(&result)?)
    }
}

/// A Skill representing an entire connected MCP Server.
pub struct McpSkill {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl McpSkill {
    /// Connects to a remote MCP server, performs initialize, and discovers its tools.
    pub async fn connect(name: &str, server_url: &str) -> Result<Self> {
        let client = Client::new();

        call_mcp_with_retry(
            &client,
            server_url,
            "initialize",
            Some(json!({
                "protocolVersion": "2025-03-26",
                "clientInfo": {
                    "name": "swarmclaw",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": {}
                }
            })),
        )
        .await
        .with_context(|| format!("Failed to initialize MCP server at {}", server_url))?;

        let tools_result = call_mcp_with_retry(&client, server_url, "tools/list", None)
            .await
            .with_context(|| format!("Failed to list MCP tools from {}", server_url))?;
        let tools = parse_tool_descriptors(&tools_result, server_url, &client)?;

        Ok(Self {
            name: format!("mcp_{}", name),
            tools,
        })
    }
}

#[async_trait]
impl Skill for McpSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "First-Class Model Context Protocol (MCP) Integration"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: String,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct McpToolDescriptor {
    name: String,
    description: String,
    #[serde(default, rename = "inputSchema")]
    input_schema: Value,
}

async fn call_mcp_with_retry(
    client: &Client,
    server_url: &str,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    let mut last_error = None;

    for attempt in 0..10 {
        match call_mcp(client, server_url, method, params.clone()).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_millis(150 * (attempt + 1) as u64)).await;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("MCP request failed without an error")))
}

async fn call_mcp(
    client: &Client,
    server_url: &str,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    let response = client
        .post(server_url)
        .json(&JsonRpcRequest {
            jsonrpc: "2.0",
            id: Uuid::new_v4().to_string(),
            method: method.to_string(),
            params,
        })
        .send()
        .await
        .with_context(|| format!("Failed to reach MCP server at {}", server_url))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read MCP server response body")?;
    if !status.is_success() {
        bail!(
            "MCP server {} returned HTTP {}: {}",
            server_url,
            status,
            body
        );
    }

    let rpc: JsonRpcResponse = serde_json::from_str(&body)
        .with_context(|| format!("Failed to decode MCP JSON-RPC response: {}", body))?;
    if let Some(error) = rpc.error {
        bail!("MCP server error: {}", error.message);
    }

    rpc.result
        .context("MCP response did not include a result payload")
}

fn parse_tool_descriptors(
    result: &Value,
    server_url: &str,
    client: &Client,
) -> Result<Vec<Arc<dyn Tool>>> {
    let descriptors: Vec<McpToolDescriptor> = serde_json::from_value(
        result
            .get("tools")
            .cloned()
            .context("MCP tools/list result did not include a tools array")?,
    )
    .context("Failed to parse MCP tool descriptors")?;

    Ok(descriptors
        .into_iter()
        .map(|tool| {
            Arc::new(McpTool {
                name: tool.name,
                description: tool.description,
                parameters: if tool.input_schema.is_null() {
                    json!({"type": "object", "properties": {}})
                } else {
                    tool.input_schema
                },
                server_url: server_url.to_string(),
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect())
}
