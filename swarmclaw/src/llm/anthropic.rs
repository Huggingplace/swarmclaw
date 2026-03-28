use crate::core::state::{Message, Role};
use crate::llm::{
    ChatChunk, CompletionOptions, CompletionResponse, LLMProvider, ProviderCapabilities,
};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::json;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

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
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn provider_name(&self) -> &str {
        "Anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::streaming_text_only()
    }

    fn update_api_key(&self, key: String) {
        if let Ok(mut api_key) = self.api_key.lock() {
            *api_key = key;
        }
    }

    async fn complete_with_tools(
        &self,
        _messages: &[Message],
        _options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>],
    ) -> Result<CompletionResponse> {
        anyhow::bail!("Non-streaming complete_with_tools not implemented for Anthropic")
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let mut system = String::new();
        let mut anthropic_messages = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    system = msg.content.clone();
                }
                Role::User => {
                    anthropic_messages.push(json!({"role": "user", "content": msg.content}));
                }
                Role::Assistant => {
                    anthropic_messages.push(json!({"role": "assistant", "content": msg.content}));
                }
                Role::Tool => {
                    // Anthropic requires specific tool result mapping, ignoring for basic text parity
                }
            }
        }

        let request_body = json!({
            "model": options.model.as_deref().unwrap_or("claude-3-5-sonnet-20241022"),
            "max_tokens": options.max_tokens.unwrap_or(4096),
            "system": system,
            "messages": anthropic_messages,
            "stream": true,
        });

        let api_key = self.api_key.lock().unwrap().clone();
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Anthropic API error: {}", error_text);
        }

        let stream = response.bytes_stream().flat_map(|item| {
            let mut chunks = Vec::new();
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        if !line.starts_with("data: ") {
                            continue;
                        }

                        let data = &line["data: ".len()..];
                        if data == "[DONE]" {
                            chunks.push(Ok(ChatChunk::Done));
                            break;
                        }

                        match serde_json::from_str::<serde_json::Value>(data) {
                            Ok(v) => {
                                if let Some(type_str) = v.get("type").and_then(|t| t.as_str()) {
                                    if type_str == "content_block_delta" {
                                        if let Some(text) = v
                                            .get("delta")
                                            .and_then(|d| d.get("text"))
                                            .and_then(|t| t.as_str())
                                        {
                                            chunks.push(Ok(ChatChunk::Content(text.to_string())));
                                        }
                                    } else if type_str == "message_stop" {
                                        chunks.push(Ok(ChatChunk::Done));
                                    }
                                }
                            }
                            Err(_) => {
                                // Ignore parse errors for partial chunks in this simple impl
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
