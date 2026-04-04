pub mod anthropic;
pub mod gemini;
pub mod ollama;
pub mod openai;

use crate::core::state::Message;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct CompletionOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ChatChunk {
    Content(String),
    ToolCallStart {
        id: String,
        name: String,
        thought_signature: Option<String>,
    },
    ToolCallDelta {
        arguments: String,
    },
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub supports_streaming: bool,
    pub supports_non_streaming: bool,
    pub supports_tool_calls: bool,
    pub supports_streaming_tool_calls: bool,
    pub supports_parallel_tool_calls: bool,
}

impl ProviderCapabilities {
    pub const fn openai_compatible() -> Self {
        Self {
            supports_streaming: true,
            supports_non_streaming: true,
            supports_tool_calls: true,
            supports_streaming_tool_calls: true,
            supports_parallel_tool_calls: false,
        }
    }

    pub const fn streaming_text_only() -> Self {
        Self {
            supports_streaming: true,
            supports_non_streaming: false,
            supports_tool_calls: false,
            supports_streaming_tool_calls: false,
            supports_parallel_tool_calls: false,
        }
    }
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn complete(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
    ) -> Result<CompletionResponse> {
        self.complete_with_tools(messages, options, &[]).await
    }

    fn provider_name(&self) -> &str;

    fn capabilities(&self) -> ProviderCapabilities;

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<CompletionResponse>;

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>>;

    fn update_api_key(&self, _key: String) {}

    fn is_auth_error(&self, error: &anyhow::Error) -> bool {
        let err_str = error.to_string().to_lowercase();
        err_str.contains("api_key")
            || err_str.contains("api key")
            || err_str.contains("401")
            || err_str.contains("unauthorized")
    }
}
