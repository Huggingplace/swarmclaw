use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use swarmclaw_sdk::{
    export_execute, export_manifest, host_http, HttpRequest, SwarmClawSkill, ToolDefinition,
};

const DEFAULT_FETCH_MCP_ENDPOINT: &str = "http://127.0.0.1:4419/mcp/fetch";

pub struct FetchConvertSkill;

impl SwarmClawSkill for FetchConvertSkill {
    fn name(&self) -> &str {
        "fetch_convert"
    }

    fn description(&self) -> &str {
        "Fetch a URL, extract readable text, and optionally use the host browser fallback for JS-heavy pages."
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            "fetch_convert_page",
            "Fetch a URL, convert it into readable text, and optionally fall back to the host browser service.",
            json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string" },
                    "render_js": { "type": "boolean" },
                    "auto_render_js": { "type": "boolean" },
                    "max_chars": { "type": "integer", "minimum": 500, "maximum": 200000 }
                }
            }),
        )]
    }

    fn capabilities(&self) -> Vec<String> {
        vec![
            "http:http://127.0.0.1".to_string(),
            "http:http://localhost".to_string(),
        ]
    }

    fn execute(&self, args: Value) -> Result<String> {
        self.execute_tool("fetch_convert_page", args)
    }

    fn execute_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let endpoint = resolve_endpoint(&args);
        let arguments = strip_transport_fields(args);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": format!("wasm-fetch-convert-{}", tool_name),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            }
        });

        let response = host_http(&HttpRequest::post_json(&endpoint, &request_body)?)
            .with_context(|| format!("Failed to reach local fetch MCP endpoint at {}", endpoint))?;
        let rpc: JsonRpcResponse = response
            .json()
            .context("Failed to decode fetch MCP JSON-RPC response")?;

        if let Some(error) = rpc.error {
            bail!("{}", error.message);
        }

        let result = rpc
            .result
            .context("Fetch MCP response did not include a result payload")?;
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
                .unwrap_or("fetch_convert_page failed");
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
        .unwrap_or_else(|| DEFAULT_FETCH_MCP_ENDPOINT.to_string())
}

fn normalize_endpoint(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    if trimmed.ends_with("/mcp/fetch") {
        trimmed.to_string()
    } else if trimmed.ends_with("/mcp") {
        format!("{}/fetch", trimmed)
    } else {
        format!("{}/mcp/fetch", trimmed)
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
    export_manifest(&FetchConvertSkill)
}

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> i64 {
    export_execute(&FetchConvertSkill, ptr, len)
}
