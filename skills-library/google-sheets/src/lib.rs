use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use swarmclaw_sdk::{
    export_execute, export_manifest, host_http, HttpRequest, SwarmClawSkill, ToolDefinition,
};

const DEFAULT_MCP_ENDPOINT: &str = "http://127.0.0.1:4418/mcp";

pub struct GoogleSheetsSkill;

impl SwarmClawSkill for GoogleSheetsSkill {
    fn name(&self) -> &str {
        "google_sheets"
    }

    fn description(&self) -> &str {
        "Write to user-selected Google Sheets through SwarmClaw's host-owned local integration."
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "list_bound_google_sheets",
                "List every spreadsheet alias currently bound in the local SwarmClaw Google Workspace store.",
                json!({ "type": "object", "properties": {} }),
            ),
            ToolDefinition::new(
                "get_google_sheet_metadata",
                "Fetch fresh spreadsheet metadata for a previously bound alias.",
                json!({
                    "type": "object",
                    "required": ["alias"],
                    "properties": {
                        "alias": { "type": "string" }
                    }
                }),
            ),
            ToolDefinition::new(
                "append_google_sheet_rows",
                "Append rows to a logical table inside a previously bound spreadsheet alias.",
                json!({
                    "type": "object",
                    "required": ["alias", "range", "values"],
                    "properties": {
                        "alias": { "type": "string" },
                        "range": { "type": "string" },
                        "values": {
                            "type": "array",
                            "items": { "type": "array", "items": {} }
                        },
                        "valueInputOption": {
                            "type": "string",
                            "enum": ["RAW", "USER_ENTERED"]
                        },
                        "insertDataOption": {
                            "type": "string",
                            "enum": ["OVERWRITE", "INSERT_ROWS"]
                        }
                    }
                }),
            ),
            ToolDefinition::new(
                "update_google_sheet_values",
                "Overwrite a concrete A1 range inside a previously bound spreadsheet alias.",
                json!({
                    "type": "object",
                    "required": ["alias", "range", "values"],
                    "properties": {
                        "alias": { "type": "string" },
                        "range": { "type": "string" },
                        "values": {
                            "type": "array",
                            "items": { "type": "array", "items": {} }
                        },
                        "valueInputOption": {
                            "type": "string",
                            "enum": ["RAW", "USER_ENTERED"]
                        }
                    }
                }),
            ),
            ToolDefinition::new(
                "batch_update_google_sheet_values",
                "Write multiple A1 ranges in a previously bound spreadsheet alias with one request.",
                json!({
                    "type": "object",
                    "required": ["alias", "data"],
                    "properties": {
                        "alias": { "type": "string" },
                        "valueInputOption": {
                            "type": "string",
                            "enum": ["RAW", "USER_ENTERED"]
                        },
                        "data": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["range", "values"],
                                "properties": {
                                    "range": { "type": "string" },
                                    "values": {
                                        "type": "array",
                                        "items": { "type": "array", "items": {} }
                                    }
                                }
                            }
                        }
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
        self.execute_tool("list_bound_google_sheets", args)
    }

    fn execute_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let endpoint = resolve_endpoint(&args);
        let arguments = strip_transport_fields(args);
        let request_body = json!({
            "jsonrpc": "2.0",
            "id": format!("wasm-google-sheets-{}", tool_name),
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
                .unwrap_or("Google Sheets tool failed");
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
    export_manifest(&GoogleSheetsSkill)
}

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> i64 {
    export_execute(&GoogleSheetsSkill, ptr, len)
}
