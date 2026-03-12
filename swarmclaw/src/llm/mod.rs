pub mod openai;

use async_trait::async_trait;
use crate::core::state::Message;
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use futures::Stream;
use std::pin::Pin;

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
    ToolCallStart { id: String, name: String },
    ToolCallDelta { arguments: String },
    Done,
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn complete(&self, messages: &[Message], options: &CompletionOptions) -> Result<CompletionResponse> {
        self.complete_with_tools(messages, options, &[]).await
    }

    async fn complete_with_tools(
        &self, 
        messages: &[Message], 
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>]
    ) -> Result<CompletionResponse>;

    async fn stream(
        &self,
        messages: &[Message],
        options: &CompletionOptions,
        tools: &[Arc<dyn Tool>]
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>>;
}
