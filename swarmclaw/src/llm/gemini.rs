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

const DEFAULT_MODEL: &str = "gemini-3.1-pro-preview";

pub struct GeminiProvider {
    client: Client,
    api_key: Mutex<String>,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key: Mutex::new(api_key),
            base_url: "https://generativelanguage.googleapis.com/v1beta/models".to_string(),
        }
    }

    /// Build the Gemini request body (`contents` + `tools`) shared by both the
    /// streaming and non-streaming paths.
    ///
    /// SwarmClaw stores assistant tool calls in the OpenAI JSON shape and tool
    /// results as `Role::Tool` messages keyed by `tool_call_id`; these are
    /// rebuilt into Gemini's `functionCall` / `functionResponse` parts.
    fn build_request_body(messages: &[Message], tools: &[Arc<dyn Tool>]) -> Value {
        let mut contents = Vec::new();
        let mut system = None;

        let mut i = 0;
        while i < messages.len() {
            let msg = &messages[i];
            match msg.role {
                Role::System => {
                    system = Some(json!({
                        "role": "user",
                        "parts": [{"text": format!("SYSTEM INSTRUCTION: {}", msg.content)}]
                    }));
                    i += 1;
                }
                Role::User => {
                    contents.push(json!({"role": "user", "parts": [{"text": msg.content}]}));
                    i += 1;
                }
                Role::Assistant => {
                    let mut parts = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({"text": msg.content}));
                    }
                    if let Some(calls) = &msg.tool_calls {
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

                                let mut function_call = json!({
                                    "name": name,
                                    "args": args
                                });
                                if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                                    function_call["id"] = json!(id);
                                }

                                let mut part = json!({ "functionCall": function_call });
                                if let Some(sig) =
                                    func.get("thought_signature").and_then(|s| s.as_str())
                                {
                                    part["thoughtSignature"] = json!(sig);
                                }
                                parts.push(part);
                            }
                        }
                    }
                    if !parts.is_empty() {
                        contents.push(json!({"role": "model", "parts": parts}));
                    } else {
                        // Empty assistant message? Push empty text to keep alternating structure
                        contents.push(json!({"role": "model", "parts": [{"text": ""}]}));
                    }
                    i += 1;
                }
                Role::Tool => {
                    // Group consecutive tool responses into a single user message
                    let mut parts = Vec::new();
                    while i < messages.len() && messages[i].role == Role::Tool {
                        let tmsg = &messages[i];
                        let mut func_name = "unknown_function".to_string();

                        if let Some(target_id) = &tmsg.tool_call_id {
                            // Look back for the matching tool call
                            for prev in messages.iter().rev() {
                                if prev.role == Role::Assistant {
                                    if let Some(calls) = &prev.tool_calls {
                                        for call in calls {
                                            if call.get("id").and_then(|i| i.as_str())
                                                == Some(target_id.as_str())
                                            {
                                                if let Some(n) = call
                                                    .get("function")
                                                    .and_then(|f| f.get("name"))
                                                    .and_then(|n| n.as_str())
                                                {
                                                    func_name = n.to_string();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let mut response_obj = json!({"output": tmsg.content});
                        if let Ok(parsed) = serde_json::from_str::<Value>(&tmsg.content) {
                            if parsed.is_object() {
                                response_obj = parsed;
                            }
                        }

                        let mut function_response = json!({
                            "name": func_name,
                            "response": response_obj
                        });
                        if let Some(id) = &tmsg.tool_call_id {
                            function_response["id"] = json!(id);
                        }

                        parts.push(json!({
                            "functionResponse": function_response
                        }));
                        i += 1;
                    }
                    contents.push(json!({
                        "role": "user",
                        "parts": parts
                    }));
                }
            }
        }

        if let Some(sys) = system {
            contents.insert(0, sys);
        }

        let mut request_body = json!({
            "contents": contents,
        });

        if !tools.is_empty() {
            let mut declarations = Vec::new();
            for t in tools {
                declarations.push(json!({
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters()
                }));
            }
            request_body["tools"] = json!([{
                "functionDeclarations": declarations
            }]);
        }

        request_body
    }

    /// Parse a non-streaming `generateContent` response into a CompletionResponse.
    fn parse_response(v: &Value) -> Result<CompletionResponse> {
        let candidate = v
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());

        let mut content_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(candidate) = candidate {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        content_text.push_str(text);
                    }
                    if let Some(call) = part.get("functionCall") {
                        let name = call
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let args = call.get("args").cloned().unwrap_or(json!({}));
                        // Gemini omits a call id in many responses; synthesize a
                        // stable one so tool results can be matched back.
                        let id = call
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
                        let sig = part
                            .get("thoughtSignature")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments: args.to_string(),
                            thought_signature: sig,
                        });
                    }
                }
            }
        }

        let finish_reason = candidate
            .and_then(|c| c.get("finishReason"))
            .and_then(|r| r.as_str())
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
}

#[async_trait]
impl LLMProvider for GeminiProvider {
    fn provider_name(&self) -> &str {
        "Gemini"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::openai_compatible() // Allows tools
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
        let request_body = Self::build_request_body(messages, tools);
        let model = options.model.as_deref().unwrap_or(DEFAULT_MODEL);

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self
            .client
            .post(format!(
                "{}/{}:generateContent?key={}",
                self.base_url, model, api_key
            ))
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Gemini API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Gemini API error: {}", error_text);
        }

        let v: Value = response.json().await?;
        Self::parse_response(&v)
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let request_body = Self::build_request_body(messages, tools);
        let model = options.model.as_deref().unwrap_or(DEFAULT_MODEL);

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self
            .client
            .post(format!(
                "{}/{}:streamGenerateContent?alt=sse&key={}",
                self.base_url, model, api_key
            ))
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Gemini API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Gemini API error: {}", error_text);
        }

        let stream = response.bytes_stream().eventsource().flat_map(|event_res| {
            let mut chunks = Vec::new();
            match event_res {
                Ok(event) => {
                    let data = event.data;
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
                        if let Some(candidates) =
                            parsed.get("candidates").and_then(|c| c.as_array())
                        {
                            if let Some(candidate) = candidates.get(0) {
                                if let Some(parts) = candidate
                                    .get("content")
                                    .and_then(|c| c.get("parts"))
                                    .and_then(|p| p.as_array())
                                {
                                    for part in parts {
                                        if let Some(text) =
                                            part.get("text").and_then(|t| t.as_str())
                                        {
                                            chunks.push(Ok(ChatChunk::Content(text.to_string())));
                                        }
                                        if let Some(call) = part.get("functionCall") {
                                            let name = call
                                                .get("name")
                                                .and_then(|n| n.as_str())
                                                .unwrap_or_default()
                                                .to_string();
                                            let args =
                                                call.get("args").cloned().unwrap_or(json!({}));
                                            let args_str = serde_json::to_string(&args)
                                                .unwrap_or_else(|_| "{}".to_string());
                                            let id = call
                                                .get("id")
                                                .and_then(|i| i.as_str())
                                                .unwrap_or_default()
                                                .to_string();
                                            let sig = part
                                                .get("thoughtSignature")
                                                .and_then(|s| s.as_str())
                                                .map(|s| s.to_string());

                                            chunks.push(Ok(ChatChunk::ToolCallStart {
                                                id,
                                                name,
                                                thought_signature: sig,
                                            }));
                                            chunks.push(Ok(ChatChunk::ToolCallDelta {
                                                arguments: args_str,
                                            }));
                                        }
                                    }
                                }
                                if let Some(finish_reason) =
                                    candidate.get("finishReason").and_then(|r| r.as_str())
                                {
                                    if finish_reason == "STOP" {
                                        chunks.push(Ok(ChatChunk::Done));
                                    }
                                }
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
    fn system_message_becomes_leading_instruction() {
        let history = vec![msg(Role::System, "Be terse."), msg(Role::User, "Hi")];
        let body = GeminiProvider::build_request_body(&history, &[]);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert!(contents[0]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("SYSTEM INSTRUCTION: Be terse."));
        assert_eq!(contents[1]["parts"][0]["text"], "Hi");
    }

    #[test]
    fn assistant_tool_calls_become_function_call_parts() {
        let mut assistant = msg(Role::Assistant, "");
        assistant.tool_calls = Some(vec![json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
        })]);
        let body = GeminiProvider::build_request_body(&[assistant], &[]);
        let part = &body["contents"][0]["parts"][0]["functionCall"];
        assert_eq!(part["name"], "search");
        assert_eq!(part["args"]["q"], "rust");
        assert_eq!(part["id"], "call_1");
    }

    #[test]
    fn parse_response_extracts_text_and_function_call() {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "Sure."},
                    {"functionCall": {"name": "list_files", "args": {"path": "/"}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let resp = GeminiProvider::parse_response(&body).unwrap();
        assert_eq!(resp.content.as_deref(), Some("Sure."));
        assert_eq!(resp.finish_reason.as_deref(), Some("STOP"));
        let calls = resp.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_files");
        // A synthetic id is generated when Gemini omits one.
        assert!(!calls[0].id.is_empty());
        let parsed: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(parsed["path"], "/");
    }
}
