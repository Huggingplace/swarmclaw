use crate::llm::{LLMProvider, CompletionOptions, CompletionResponse, ToolCall, ChatChunk};
use crate::core::state::{Message, Role};
use crate::tools::Tool;
use anyhow::{Result, Context};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use futures::{Stream, StreamExt};
use std::pin::Pin;

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
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    async fn complete_with_tools(
        &self, 
        _messages: &[Message], 
        _options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>]
    ) -> Result<CompletionResponse> {
        anyhow::bail!("Non-streaming complete_with_tools not implemented for Ollama")
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>]
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let mut ollama_messages = Vec::new();

        for msg in messages {
            let role = match msg.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool", // Basic fallback mapping
            };
            ollama_messages.push(json!({
                "role": role,
                "content": msg.content
            }));
        }

        let request_body = json!({
            "model": options.model.as_deref().unwrap_or("llama3"),
            "messages": ollama_messages,
            "stream": true,
        });

        let response = self.client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Ollama API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Ollama API error: {}", error_text);
        }

        let stream = response.bytes_stream().flat_map(|item| {
            let mut chunks = Vec::new();
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.is_empty() { continue; }
                        
                        match serde_json::from_str::<serde_json::Value>(line) {
                            Ok(v) => {
                                if let Some(msg) = v.get("message") {
                                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                                        chunks.push(Ok(ChatChunk::Content(content.to_string())));
                                    }
                                }
                                if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                                    chunks.push(Ok(ChatChunk::Done));
                                }
                            }
                            Err(_) => {
                                // Ignored for simplicity
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
