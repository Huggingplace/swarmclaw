use async_trait::async_trait;
use crate::llm::{LLMProvider, CompletionOptions, CompletionResponse, ToolCall, ChatChunk};
use crate::core::state::{Message, Role};
use crate::tools::Tool;
use serde::Serialize;
use serde_json::json;
use reqwest::Client;
use anyhow::{Context, Result};
use std::sync::Arc;
use futures::{Stream, StreamExt};
use std::pin::Pin;

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

#[derive(Serialize)]
struct OpenAIRequestMessage<'a> {
    role: &'a Role,
    content: &'a str,
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    async fn complete_with_tools(
        &self, 
        messages: &[Message], 
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>]
    ) -> Result<CompletionResponse> {
        let model = options.model.as_deref().unwrap_or("gpt-3.5-turbo");
        let temperature = options.temperature.unwrap_or(0.7);

        let api_messages: Vec<OpenAIRequestMessage> = messages.iter().map(|m| OpenAIRequestMessage {
            role: &m.role,
            content: &m.content,
        }).collect();

        let mut request_body = json!({
            "model": model,
            "messages": api_messages,
            "temperature": temperature,
            "max_tokens": options.max_tokens,
        });

        if !tools.is_empty() {
            let api_tools: Vec<serde_json::Value> = tools.iter().map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            }).collect();
            request_body["tools"] = json!(api_tools);
        }

        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to OpenAI API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("OpenAI API error: {}", error_text);
        }

        let response_json: serde_json::Value = response.json().await?;
        
        let choice = response_json["choices"][0].as_object()
            .context("No choices in response")?;
            
        let message = choice["message"].as_object()
            .context("No message in choice")?;
            
        let content = message.get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let tool_calls = message.get("tool_calls")
            .and_then(|v| v.as_array())
            .map(|calls| {
                calls.iter().filter_map(|c| {
                    let function = c.get("function")?;
                    Some(ToolCall {
                        id: c.get("id")?.as_str()?.to_string(),
                        name: function.get("name")?.as_str()?.to_string(),
                        arguments: function.get("arguments")?.as_str()?.to_string(),
                    })
                }).collect()
            });

        let finish_reason = choice["finish_reason"].as_str()
            .map(|s| s.to_string());

        Ok(CompletionResponse {
            content,
            tool_calls,
            finish_reason,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>]
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let model = options.model.as_deref().unwrap_or("gpt-3.5-turbo");
        let temperature = options.temperature.unwrap_or(0.7);

        let api_messages: Vec<OpenAIRequestMessage> = messages.iter().map(|m| OpenAIRequestMessage {
            role: &m.role,
            content: &m.content,
        }).collect();

        let mut request_body = json!({
            "model": model,
            "messages": api_messages,
            "temperature": temperature,
            "max_tokens": options.max_tokens,
            "stream": true,
        });

        if !tools.is_empty() {
            let api_tools: Vec<serde_json::Value> = tools.iter().map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            }).collect();
            request_body["tools"] = json!(api_tools);
        }

        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to OpenAI API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("OpenAI API error: {}", error_text);
        }

        let stream = response.bytes_stream().flat_map(|item| {
            let mut chunks = Vec::new();
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.is_empty() { continue; }
                        if !line.starts_with("data: ") { continue; }
                        
                        let data = &line["data: ".len()..];
                        if data == "[DONE]" {
                            chunks.push(Ok(ChatChunk::Done));
                            break;
                        }

                        match serde_json::from_str::<serde_json::Value>(data) {
                            Ok(v) => {
                                let choice = &v["choices"][0];
                                let delta = &choice["delta"];

                                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                    chunks.push(Ok(ChatChunk::Content(content.to_string())));
                                }

                                if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                                    for tc in tool_calls {
                                        if let (Some(id), Some(name)) = (tc.get("id").and_then(|v| v.as_str()), tc.get("function").and_then(|v| v.get("name")).and_then(|v| v.as_str())) {
                                            chunks.push(Ok(ChatChunk::ToolCallStart { id: id.to_string(), name: name.to_string() }));
                                        }
                                        if let Some(args) = tc.get("function").and_then(|v| v.get("arguments")).and_then(|v| v.as_str()) {
                                            chunks.push(Ok(ChatChunk::ToolCallDelta { arguments: args.to_string() }));
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                chunks.push(Err(anyhow::anyhow!("Failed to parse SSE data: {}", e)));
                            }
                        }
                    }
                }
                Err(e) => {
                    chunks.push(Err(anyhow::anyhow!("Failed to read from stream: {}", e)));
                }
            }
            futures::stream::iter(chunks)
        });

        Ok(Box::pin(stream))
    }
}
