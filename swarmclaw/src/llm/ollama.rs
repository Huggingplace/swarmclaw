use crate::core::state::{Message, Role};
use crate::llm::{
    ChatChunk, CompletionOptions, CompletionResponse, LLMProvider, ProviderCapabilities, ToolCall,
};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::pin::Pin;
use std::sync::Arc;

const DEFAULT_MODEL: &str = "llama3";

#[derive(Clone)]
pub struct OllamaProvider {
    client: Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new(base_url: String) -> Self {
        let url = if base_url.is_empty() {
            "http://localhost:11434".to_string()
        } else {
            base_url
        };
        Self {
            client: Client::new(),
            base_url: url,
        }
    }

    /// Translate SwarmClaw's internal history into Ollama `/api/chat` messages.
    ///
    /// Ollama expects assistant tool calls as `tool_calls[].function.arguments`
    /// (a JSON *object*) and tool results as `role: "tool"` messages. SwarmClaw
    /// stores arguments as a JSON string and results as `Role::Tool` messages
    /// keyed by `tool_call_id`, so both are converted here.
    fn convert_messages(messages: &[Message]) -> Vec<Value> {
        let mut out = Vec::new();
        for msg in messages {
            match msg.role {
                Role::System => {
                    out.push(json!({"role": "system", "content": msg.content}));
                }
                Role::User => {
                    out.push(json!({"role": "user", "content": msg.content}));
                }
                Role::Assistant => {
                    let mut m = json!({"role": "assistant", "content": msg.content});
                    if let Some(calls) = &msg.tool_calls {
                        let mut tc = Vec::new();
                        for call in calls {
                            if let Some(func) = call.get("function") {
                                let name = func
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default();
                                let args_str = func
                                    .get("arguments")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("{}");
                                let args: Value =
                                    serde_json::from_str(args_str).unwrap_or(json!({}));
                                tc.push(json!({"function": {"name": name, "arguments": args}}));
                            }
                        }
                        if !tc.is_empty() {
                            m["tool_calls"] = json!(tc);
                        }
                    }
                    out.push(m);
                }
                Role::Tool => {
                    let mut m = json!({"role": "tool", "content": msg.content});
                    // Newer Ollama versions use `tool_name` to associate a
                    // result with its call; fill it in when we can resolve it.
                    if let Some(id) = &msg.tool_call_id {
                        if let Some(name) = Self::lookup_tool_name(messages, id) {
                            m["tool_name"] = json!(name);
                        }
                    }
                    out.push(m);
                }
            }
        }
        out
    }

    fn lookup_tool_name(messages: &[Message], target_id: &str) -> Option<String> {
        for prev in messages.iter().rev() {
            if prev.role == Role::Assistant {
                if let Some(calls) = &prev.tool_calls {
                    for call in calls {
                        if call.get("id").and_then(|i| i.as_str()) == Some(target_id) {
                            return call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn build_tools(tools: &[Arc<dyn Tool>]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Parse an Ollama `message` object into optional text + tool calls.
    /// Ollama returns tool-call arguments as a JSON object (not a string), so
    /// we re-serialize them into the string form the runtime expects.
    fn parse_message(message: &Value) -> (Option<String>, Vec<ToolCall>) {
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let mut tool_calls = Vec::new();
        if let Some(calls) = message.get("tool_calls").and_then(|c| c.as_array()) {
            for call in calls {
                if let Some(func) = call.get("function") {
                    let name = func
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let args = func.get("arguments").cloned().unwrap_or(json!({}));
                    let args_str = match args.as_str() {
                        Some(s) => s.to_string(),
                        None => args.to_string(),
                    };
                    // Ollama does not return a tool-call id; synthesize one so
                    // results can be matched back.
                    let id = call
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: args_str,
                        thought_signature: None,
                    });
                }
            }
        }
        (content, tool_calls)
    }

    fn build_request_body(
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
        stream: bool,
    ) -> Value {
        let mut body = json!({
            "model": options.model.as_deref().unwrap_or(DEFAULT_MODEL),
            "messages": Self::convert_messages(messages),
            "stream": stream,
        });
        if !tools.is_empty() {
            body["tools"] = json!(Self::build_tools(tools));
        }
        body
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    fn provider_name(&self) -> &str {
        "Ollama"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Tool support depends on the loaded model; capable models (llama3.1,
        // qwen2.5, mistral-nemo, ...) support tools in both modes.
        ProviderCapabilities {
            supports_streaming: true,
            supports_non_streaming: true,
            supports_tool_calls: true,
            supports_streaming_tool_calls: true,
            supports_parallel_tool_calls: false,
        }
    }

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<CompletionResponse> {
        let body = Self::build_request_body(messages, options, tools, false);

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Ollama API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Ollama API error: {}", error_text);
        }

        let v: Value = response.json().await?;
        let (content, tool_calls) = Self::parse_message(v.get("message").unwrap_or(&Value::Null));
        let finish_reason = v
            .get("done_reason")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string());

        Ok(CompletionResponse {
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            finish_reason,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let body = Self::build_request_body(messages, options, tools, true);

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Ollama API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Ollama API error: {}", error_text);
        }

        // Ollama streams newline-delimited JSON objects. Tool calls arrive
        // complete within a single chunk's `message.tool_calls`, so we emit a
        // start+delta pair for each.
        let stream = response.bytes_stream().flat_map(|item| {
            let mut chunks = Vec::new();
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(line) {
                            Ok(v) => {
                                if let Some(message) = v.get("message") {
                                    if let Some(content) =
                                        message.get("content").and_then(|c| c.as_str())
                                    {
                                        if !content.is_empty() {
                                            chunks.push(Ok(ChatChunk::Content(
                                                content.to_string(),
                                            )));
                                        }
                                    }
                                    let (_, tool_calls) = OllamaProvider::parse_message(message);
                                    for tc in tool_calls {
                                        chunks.push(Ok(ChatChunk::ToolCallStart {
                                            id: tc.id,
                                            name: tc.name,
                                            thought_signature: None,
                                        }));
                                        chunks.push(Ok(ChatChunk::ToolCallDelta {
                                            arguments: tc.arguments,
                                        }));
                                    }
                                }
                                if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                                    chunks.push(Ok(ChatChunk::Done));
                                }
                            }
                            Err(_) => {
                                // Ignore partial / non-JSON frames.
                            }
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
    fn assistant_tool_calls_serialize_arguments_as_object() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = Some(vec![json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "weather", "arguments": "{\"city\":\"NYC\"}"}
        })]);
        let out = OllamaProvider::convert_messages(&[assistant]);
        let tc = &out[0]["tool_calls"][0]["function"];
        assert_eq!(tc["name"], "weather");
        // arguments must be an object for Ollama, not a string.
        assert_eq!(tc["arguments"]["city"], "NYC");
    }

    #[test]
    fn tool_result_gets_tool_name_from_prior_call() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = Some(vec![json!({
            "id": "abc",
            "type": "function",
            "function": {"name": "weather", "arguments": "{}"}
        })]);
        let mut result = msg(Role::Tool, "sunny");
        result.tool_call_id = Some("abc".to_string());

        let out = OllamaProvider::convert_messages(&[assistant, result]);
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["content"], "sunny");
        assert_eq!(out[1]["tool_name"], "weather");
    }

    #[test]
    fn parse_message_extracts_object_arguments() {
        let message = json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{"function": {"name": "add", "arguments": {"a": 1, "b": 2}}}]
        });
        let (content, calls) = OllamaProvider::parse_message(&message);
        assert!(content.is_none());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "add");
        assert!(!calls[0].id.is_empty());
        let parsed: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn build_request_body_includes_tools_flag_and_stream() {
        let history = vec![msg(Role::User, "hi")];
        let options = CompletionOptions::default();
        let body = OllamaProvider::build_request_body(&history, &options, &[], false);
        assert_eq!(body["stream"], false);
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert!(body.get("tools").is_none());
    }
}
