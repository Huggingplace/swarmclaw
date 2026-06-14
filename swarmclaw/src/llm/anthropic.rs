use crate::core::state::{Message, Role};
use crate::llm::{
    ChatChunk, CompletionOptions, CompletionResponse, LLMProvider, ProviderCapabilities, ToolCall,
};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

const DEFAULT_MODEL: &str = "claude-3-5-sonnet-20241022";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: Client,
    api_key: Mutex<String>,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key: Mutex::new(api_key),
            base_url: "https://api.anthropic.com/v1".to_string(),
        }
    }

    /// Translate SwarmClaw's internal message history into Anthropic's
    /// `(system, messages)` shape.
    ///
    /// SwarmClaw stores assistant tool calls in the OpenAI JSON shape
    /// (`{id, type, function:{name, arguments}}`) and tool results as a
    /// dedicated `Role::Tool` message carrying a `tool_call_id`. Anthropic
    /// instead expects `tool_use` content blocks inside the assistant turn and
    /// `tool_result` blocks inside the following user turn, so we rebuild both.
    fn convert_messages(messages: &[Message]) -> (String, Vec<Value>) {
        let mut system = String::new();
        let mut out: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&msg.content);
                }
                Role::User => {
                    out.push(json!({ "role": "user", "content": msg.content }));
                }
                Role::Assistant => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": msg.content }));
                    }
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                            let function = tc.get("function");
                            let name = function
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let args_str = function
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            // Anthropic expects `input` as a JSON object, not a string.
                            let input: Value =
                                serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            }));
                        }
                    }
                    // Anthropic rejects empty content arrays / empty text blocks.
                    // The agent only records assistant turns that have content or
                    // tool calls, but guard anyway.
                    if blocks.is_empty() {
                        continue;
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
                Role::Tool => {
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                        "content": msg.content,
                    });
                    // Group consecutive tool results into a single user turn so
                    // they immediately follow the assistant `tool_use` turn, as
                    // Anthropic requires. Only merge into an existing
                    // block-array user turn (created by a prior tool result),
                    // never into a plain text user message.
                    if let Some(last) = out.last_mut() {
                        if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                            if let Some(arr) =
                                last.get_mut("content").and_then(|c| c.as_array_mut())
                            {
                                arr.push(block);
                                continue;
                            }
                        }
                    }
                    out.push(json!({ "role": "user", "content": [block] }));
                }
            }
        }

        (system, out)
    }

    fn build_tools(tools: &[Arc<dyn Tool>]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name(),
                    "description": t.description(),
                    "input_schema": t.parameters(),
                })
            })
            .collect()
    }

    fn parse_response(v: Value) -> Result<CompletionResponse> {
        let mut content_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            content_text.push_str(text);
                        }
                    }
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            // Normalize back to the JSON-string arguments shape
                            // the rest of the runtime expects.
                            arguments: input.to_string(),
                            thought_signature: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        let finish_reason = v
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());

        Ok(CompletionResponse {
            content: if content_text.is_empty() {
                None
            } else {
                Some(content_text)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            finish_reason,
        })
    }

    fn build_request_body(
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
        stream: bool,
    ) -> Value {
        let (system, anthropic_messages) = Self::convert_messages(messages);

        let mut body = json!({
            "model": options.model.as_deref().unwrap_or(DEFAULT_MODEL),
            "max_tokens": options.max_tokens.unwrap_or(4096),
            "messages": anthropic_messages,
            "stream": stream,
        });

        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(temperature) = options.temperature {
            body["temperature"] = json!(temperature);
        }
        if !tools.is_empty() {
            body["tools"] = json!(Self::build_tools(tools));
        }

        body
    }
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn provider_name(&self) -> &str {
        "Anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_streaming: true,
            supports_non_streaming: true,
            supports_tool_calls: true,
            supports_streaming_tool_calls: true,
            supports_parallel_tool_calls: true,
        }
    }

    fn update_api_key(&self, key: String) {
        if let Ok(mut api_key) = self.api_key.lock() {
            *api_key = key;
        }
    }

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<CompletionResponse> {
        let body = Self::build_request_body(messages, options, tools, false);

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Anthropic API error: {}", error_text);
        }

        Self::parse_response(response.json().await?)
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let body = Self::build_request_body(messages, options, tools, true);

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Anthropic API error: {}", error_text);
        }

        let stream = response.bytes_stream().eventsource().flat_map(|event_res| {
            let mut chunks = Vec::new();
            match event_res {
                Ok(event) => {
                    let data = event.data;
                    if data.is_empty() {
                        return futures::stream::iter(chunks);
                    }
                    match serde_json::from_str::<Value>(&data) {
                        Ok(v) => match v.get("type").and_then(|t| t.as_str()) {
                            Some("content_block_start") => {
                                // A `tool_use` block opens with its id and name;
                                // its arguments arrive later via input_json_delta.
                                if let Some(cb) = v.get("content_block") {
                                    if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                        let id = cb
                                            .get("id")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or_default()
                                            .to_string();
                                        let name = cb
                                            .get("name")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or_default()
                                            .to_string();
                                        chunks.push(Ok(ChatChunk::ToolCallStart {
                                            id,
                                            name,
                                            thought_signature: None,
                                        }));
                                    }
                                }
                            }
                            Some("content_block_delta") => {
                                if let Some(delta) = v.get("delta") {
                                    match delta.get("type").and_then(|t| t.as_str()) {
                                        Some("text_delta") => {
                                            if let Some(text) =
                                                delta.get("text").and_then(|x| x.as_str())
                                            {
                                                chunks.push(Ok(ChatChunk::Content(
                                                    text.to_string(),
                                                )));
                                            }
                                        }
                                        Some("input_json_delta") => {
                                            if let Some(partial) = delta
                                                .get("partial_json")
                                                .and_then(|x| x.as_str())
                                            {
                                                chunks.push(Ok(ChatChunk::ToolCallDelta {
                                                    arguments: partial.to_string(),
                                                }));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some("message_stop") => {
                                chunks.push(Ok(ChatChunk::Done));
                            }
                            // message_start, content_block_stop, message_delta,
                            // ping, etc. carry no user-visible content here.
                            _ => {}
                        },
                        Err(_) => {
                            // Ignore unparseable partial frames.
                        }
                    }
                }
                Err(e) => chunks.push(Err(anyhow::anyhow!("Stream read error: {}", e))),
            }
            futures::stream::iter(chunks)
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            timestamp: 0,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn system_messages_are_concatenated_and_stripped_from_turns() {
        let history = vec![
            msg(Role::System, "You are SwarmClaw."),
            msg(Role::System, "Be concise."),
            msg(Role::User, "Hello"),
        ];

        let (system, messages) = AnthropicProvider::convert_messages(&history);

        assert_eq!(system, "You are SwarmClaw.\n\nBe concise.");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks_with_parsed_input() {
        let mut assistant = msg(Role::Assistant, "Let me check.");
        assistant.tool_calls = Some(vec![json!({
            "id": "toolu_1",
            "type": "function",
            "function": {
                "name": "read_file",
                "arguments": "{\"path\":\"/tmp/x\"}",
            }
        })]);

        let (_, messages) = AnthropicProvider::convert_messages(&[assistant]);

        assert_eq!(messages.len(), 1);
        let blocks = messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Let me check.");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "toolu_1");
        assert_eq!(blocks[1]["name"], "read_file");
        // `input` must be a JSON object, not a string.
        assert_eq!(blocks[1]["input"]["path"], "/tmp/x");
    }

    #[test]
    fn consecutive_tool_results_group_into_one_user_turn() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = Some(vec![
            json!({"id": "a", "type": "function", "function": {"name": "f", "arguments": "{}"}}),
            json!({"id": "b", "type": "function", "function": {"name": "g", "arguments": "{}"}}),
        ]);
        let mut result_a = msg(Role::Tool, "result a");
        result_a.tool_call_id = Some("a".to_string());
        let mut result_b = msg(Role::Tool, "result b");
        result_b.tool_call_id = Some("b".to_string());

        let (_, messages) = AnthropicProvider::convert_messages(&[assistant, result_a, result_b]);

        // One assistant turn (with two tool_use blocks) + one grouped user turn.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);

        assert_eq!(messages[1]["role"], "user");
        let results = messages[1]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "a");
        assert_eq!(results[0]["content"], "result a");
        assert_eq!(results[1]["tool_use_id"], "b");
    }

    #[test]
    fn tool_result_does_not_merge_into_plain_text_user_turn() {
        let mut result = msg(Role::Tool, "orphan result");
        result.tool_call_id = Some("z".to_string());
        let history = vec![msg(Role::User, "hi"), result];

        let (_, messages) = AnthropicProvider::convert_messages(&history);

        // The plain-text user turn stays a string; the tool result opens a new turn.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "hi");
        assert!(messages[1]["content"].is_array());
    }

    #[test]
    fn parse_response_extracts_text_and_tool_calls() {
        let body = json!({
            "content": [
                {"type": "text", "text": "Working on it."},
                {"type": "tool_use", "id": "toolu_9", "name": "shell", "input": {"cmd": "ls"}},
            ],
            "stop_reason": "tool_use",
        });

        let resp = AnthropicProvider::parse_response(body).unwrap();

        assert_eq!(resp.content.as_deref(), Some("Working on it."));
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_use"));
        let calls = resp.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_9");
        assert_eq!(calls[0].name, "shell");
        // Arguments are serialized back to a JSON string for the runtime.
        let parsed: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(parsed["cmd"], "ls");
    }

    #[test]
    fn build_request_body_omits_empty_system_and_tools() {
        let history = vec![msg(Role::User, "hi")];
        let options = CompletionOptions::default();
        let body = AnthropicProvider::build_request_body(&history, &options, &[], false);

        assert!(body.get("system").is_none());
        assert!(body.get("tools").is_none());
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], 4096);
    }
}
