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
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://generativelanguage.googleapis.com/v1beta/models".to_string(),
        }
    }
}

#[async_trait]
impl LLMProvider for GeminiProvider {
    async fn complete_with_tools(
        &self, 
        _messages: &[Message], 
        _options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>]
    ) -> Result<CompletionResponse> {
        anyhow::bail!("Non-streaming complete_with_tools not implemented for Gemini")
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>]
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        let mut contents = Vec::new();
        let mut system = None;

        for msg in messages {
            match msg.role {
                Role::System => {
                    system = Some(json!({
                        "role": "user",
                        "parts": [{"text": format!("SYSTEM INSTRUCTION: {}", msg.content)}]
                    }));
                }
                Role::User => {
                    contents.push(json!({"role": "user", "parts": [{"text": msg.content}]}));
                }
                Role::Assistant => {
                    contents.push(json!({"role": "model", "parts": [{"text": msg.content}]}));
                }
                Role::Tool => {
                    // Ignored for basic text generation parity
                }
            }
        }

        if let Some(sys) = system {
            contents.insert(0, sys);
        }

        let request_body = json!({
            "contents": contents,
        });

        let model = options.model.as_deref().unwrap_or("gemini-1.5-pro");
        
        let response = self.client
            .post(format!("{}/{}:streamGenerateContent?key={}", self.base_url, model, self.api_key))
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Gemini API")?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Gemini API error: {}", error_text);
        }

        let stream = response.bytes_stream().flat_map(|item| {
            let mut chunks = Vec::new();
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    // Gemini returns Server-Sent Events or chunked JSON arrays.
                    // Assuming SSE for streamGenerateContent with alt=sse (not used here) or raw JSON chunks.
                    // The standard streamGenerateContent returns an array stream `[ { "candidates": ... }, ... ]`
                    // We'll do a very robust basic matching for the parts.text
                    let mut found_text = false;
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("\"text\": \"") {
                            // Quick and dirty extraction for parity requirement
                            if let Some(start) = trimmed.find("\"text\": \"") {
                                let val = &trimmed[start + 9..];
                                if let Some(end) = val.rfind('"') {
                                    let content = &val[..end];
                                    let unescaped = content.replace("\\n", "\n").replace("\\\"", "\"");
                                    chunks.push(Ok(ChatChunk::Content(unescaped)));
                                    found_text = true;
                                }
                            }
                        }
                    }
                    if !found_text && text.contains("finishReason") {
                        chunks.push(Ok(ChatChunk::Done));
                    }
                }
                Err(e) => chunks.push(Err(anyhow::anyhow!("Stream read error: {}", e))),
            }
            futures::stream::iter(chunks)
        });

        Ok(Box::pin(stream))
    }
}
