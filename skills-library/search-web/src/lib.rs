use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use swarmclaw_sdk::{
    export_execute, export_manifest, host_http, HttpRequest, SwarmClawSkill, ToolDefinition,
};

const DEFAULT_SEARCH_MCP_ENDPOINT: &str = "http://127.0.0.1:4419/mcp/search";

pub struct SearchWebSkill;

impl SwarmClawSkill for SearchWebSkill {
    fn name(&self) -> &str {
        "search_web"
    }

    fn description(&self) -> &str {
        "Search the web through host-owned providers, including Google API and other configured backends."
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "list_search_providers",
                "List every search provider configured on this SwarmClaw host.",
                json!({ "type": "object", "properties": {} }),
            ),
            ToolDefinition::new(
                "search_web",
                "Search the web using the default or requested provider on the host.",
                json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "provider": {
                            "type": "string",
                            "enum": ["google", "brave", "searxng"]
                        },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10 },
                        "safe_search": { "type": "boolean" }
                    }
                }),
            ),
            ToolDefinition::new(
                "search_google_web",
                "Search the web through the host's Google Programmable Search JSON API integration.",
                json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10 },
                        "safe_search": { "type": "boolean" }
                    }
                }),
            ),
        ]
    }

    fn capabilities(&self) -> Vec<String> {
        vec![
            "http:http://127.0.0.1".to_string(),
            "http:http://localhost".to_string(),
        ]
    }

    fn execute(&self, args: Value) -> Result<String> {
        self.execute_tool("search_web", args)
    }

    fn execute_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let endpoint = resolve_endpoint(&args);
        let arguments = strip_transport_fields(args);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": format!("wasm-search-web-{}", tool_name),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });

        let response = host_http(&HttpRequest::post_json(&endpoint, &request_body)?)
            .with_context(|| format!("Failed to reach local search MCP endpoint at {}", endpoint))?;
        let rpc: JsonRpcResponse = response
            .json()
            .context("Failed to decode search MCP JSON-RPC response")?;

        if let Some(error) = rpc.error {
            bail!("{}", error.message);
        }

        let result = rpc
            .result
            .context("Search MCP response did not include a result payload")?;
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
                .unwrap_or("search_web failed");
            bail!("{}", message);
        }

        if let Some(structured) = result.get("structuredContent") {
            return Ok(serde_json::to_string_pretty(structured)?);
        }

        Ok(serde_json::to_string_pretty(&result)?)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    message: String,
}

fn resolve_endpoint(args: &Value) -> String {
    args.get("__mcp_url")
        .or_else(|| args.get("mcp_url"))
        .or_else(|| args.get("__service_url"))
        .or_else(|| args.get("service_url"))
        .and_then(Value::as_str)
        .map(normalize_endpoint)
        .unwrap_or_else(|| DEFAULT_SEARCH_MCP_ENDPOINT.to_string())
}

fn normalize_endpoint(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    if trimmed.ends_with("/mcp/search") {
        trimmed.to_string()
    } else if trimmed.ends_with("/mcp") {
        format!("{}/search", trimmed)
    } else {
        format!("{}/mcp/search", trimmed)
    }
}

fn strip_transport_fields(args: Value) -> Value {
    let mut args = args;
    if let Some(object) = args.as_object_mut() {
        object.remove("__mcp_url");
        object.remove("mcp_url");
        object.remove("__service_url");
        object.remove("service_url");
    }
    args
}

#[no_mangle]
pub extern "C" fn claw_get_manifest() -> i64 {
    export_manifest(&SearchWebSkill)
}

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> i64 {
    export_execute(&SearchWebSkill, ptr, len)
}
