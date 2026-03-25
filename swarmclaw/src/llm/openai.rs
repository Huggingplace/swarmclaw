use async_trait::async_trait;
use crate::llm::{LLMProvider, CompletionOptions, CompletionResponse, ToolCall, ChatChunk};
use crate::core::state::{Message, Role};
use crate::tools::Tool;
use serde::Serialize;
use serde_json::json;
use reqwest::Client;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use futures::{Stream, StreamExt};
use std::pin::Pin;
use eventsource_stream::Eventsource;

pub struct OpenAIProvider {
    client: Client,
    api_key: Mutex<String>,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key: Mutex::new(api_key),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

impl OpenAIProvider {
    fn convert_messages(messages: &[Message]) -> Vec<OpenAIRequestMessage> {
        let mut api_messages = Vec::new();
        for msg in messages {
            if msg.role == Role::Tool {
                // Tools need special handling in OpenAI (tool_call_id)
                api_messages.push(OpenAIRequestMessage {
                    role: &Role::Tool,
                    content: Some(&msg.content),
                    tool_calls: None,
                    tool_call_id: msg.tool_call_id.as_deref(), // Assume we add this to Message
                });
            } else if msg.role == Role::Assistant && msg.tool_calls.is_some() {
                 api_messages.push(OpenAIRequestMessage {
                    role: &Role::Assistant,
                    content: if msg.content.is_empty() { None } else { Some(&msg.content) },
                    tool_calls: msg.tool_calls.clone(),
                    tool_call_id: None,
                });
            } else {
                api_messages.push(OpenAIRequestMessage {
                    role: &msg.role,
                    content: Some(&msg.content),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }
        api_messages
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn update_api_key(&self, key: String) {
        if let Ok(mut api_key) = self.api_key.lock() {
            *api_key = key;
        }
    }

    async fn complete_with_tools(
        &self, 
        messages: &[Message], 
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>]
    ) -> Result<CompletionResponse> {
        let model = options.model.as_deref().unwrap_or("gpt-3.5-turbo");
        let temperature = options.temperature.unwrap_or(0.7);

        let api_messages = Self::convert_messages(messages);

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

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", api_key))
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

        let api_messages = Self::convert_messages(messages);

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

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to OpenAI API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("OpenAI API error: {}", error_text);
        }

        let stream = response.bytes_stream().eventsource().flat_map(|event_res| {
            let mut chunks = Vec::new();
            match event_res {
                Ok(event) => {
                    let data = event.data;
                    if data == "[DONE]" {
                        chunks.push(Ok(ChatChunk::Done));
                    } else if !data.is_empty() {
                        match serde_json::from_str::<serde_json::Value>(&data) {
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
                                chunks.push(Err(anyhow::anyhow!("Failed to parse SSE data: {} (raw: {})", e, data)));
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
