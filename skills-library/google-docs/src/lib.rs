use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use swarmclaw_sdk::{
    export_execute, export_manifest, host_http, HttpRequest, SwarmClawSkill, ToolDefinition,
};

const DEFAULT_MCP_ENDPOINT: &str = "http://127.0.0.1:4418/mcp";

pub struct GoogleDocsSkill;

impl SwarmClawSkill for GoogleDocsSkill {
    fn name(&self) -> &str {
        "google_docs"
    }

    fn description(&self) -> &str {
        "Create and edit Google Docs through SwarmClaw's host-owned local Google Workspace integration."
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "create_google_doc",
                "Create a new Google Doc with an optional initial body of text.",
                json!({
                    "type": "object",
                    "required": ["title"],
                    "properties": {
                        "title": { "type": "string" },
                        "initialText": { "type": "string" },
                        "folderId": { "type": "string" }
                    }
                }),
            ),
            ToolDefinition::new(
                "get_google_doc_content",
                "Fetch the content of a Google Doc by document id.",
                json!({
                    "type": "object",
                    "required": ["documentId"],
                    "properties": {
                        "documentId": { "type": "string" },
                        "format": {
                            "type": "string",
                            "enum": ["plain_text", "markdown", "json"]
                        }
                    }
                }),
            ),
            ToolDefinition::new(
                "append_google_doc_text",
                "Append text to the end of an existing Google Doc.",
                json!({
                    "type": "object",
                    "required": ["documentId", "text"],
                    "properties": {
                        "documentId": { "type": "string" },
                        "text": { "type": "string" }
                    }
                }),
            ),
            ToolDefinition::new(
                "insert_google_doc_image",
                "Insert a publicly accessible image into an existing Google Doc.",
                json!({
                    "type": "object",
                    "required": ["documentId", "imageUrl"],
                    "properties": {
                        "documentId": { "type": "string" },
                        "imageUrl": { "type": "string" },
                        "widthPt": { "type": "number" },
                        "heightPt": { "type": "number" },
                        "locationIndex": { "type": "integer" }
                    }
                }),
            ),
            ToolDefinition::new(
                "share_google_doc",
                "Share an existing Google Doc with one or more recipients by email.",
                json!({
                    "type": "object",
                    "required": ["documentId"],
                    "properties": {
                        "documentId": { "type": "string" },
                        "email": { "type": "string" },
                        "emails": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "role": {
                            "type": "string",
                            "enum": ["reader", "commenter", "writer"]
                        },
                        "sendNotificationEmail": { "type": "boolean" },
                        "emailMessage": { "type": "string" }
                    }
                }),
            ),
            ToolDefinition::new(
                "replace_google_doc_text",
                "Replace the body content of an existing Google Doc with new text.",
                json!({
                    "type": "object",
                    "required": ["documentId", "text"],
                    "properties": {
                        "documentId": { "type": "string" },
                        "text": { "type": "string" }
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
        self.execute_tool("create_google_doc", args)
    }

    fn execute_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let endpoint = resolve_endpoint(&args);
        let arguments = strip_transport_fields(args);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": format!("wasm-google-docs-{}", tool_name),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });

        let response =
            host_http(&HttpRequest::post_json(&endpoint, &request_body)?).with_context(|| {
                format!(
                    "Failed to reach local Google Workspace MCP endpoint at {}",
                    endpoint
                )
            })?;
        let rpc: JsonRpcResponse = response
            .json()
            .context("Failed to decode Google Workspace MCP JSON-RPC response")?;

        if let Some(error) = rpc.error {
            bail!("{}", error.message);
        }

        let result = rpc
            .result
            .context("Google Workspace MCP response did not include a result payload")?;
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
                .unwrap_or("Google Docs tool failed");
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
        .unwrap_or_else(|| DEFAULT_MCP_ENDPOINT.to_string())
}

fn normalize_endpoint(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    if trimmed.ends_with("/mcp") {
        trimmed.to_string()
    } else {
        format!("{}/mcp", trimmed)
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
    export_manifest(&GoogleDocsSkill)
}

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> i64 {
    export_execute(&GoogleDocsSkill, ptr, len)
}
