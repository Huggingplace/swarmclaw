use crate::config::AgentConfig;
use crate::core::state::Message;
use crate::core::Agent;
use crate::llm::{
    ChatChunk, CompletionOptions, CompletionResponse, LLMProvider, ProviderCapabilities,
};
use crate::outbox::{self, OutboxMessageSummary};
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use futures::{stream, Stream};
use std::pin::Pin;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub(crate) fn test_agent_template() -> Arc<Agent> {
    Arc::new(Agent::new(
        "test-agent".to_string(),
        AgentConfig::default(),
        Arc::new(FakeProvider),
    ))
}

pub(crate) async fn wait_for_outbox_message(
    platform: &str,
    channel_id: &str,
) -> Result<OutboxMessageSummary> {
    for _ in 0..40 {
        let messages = outbox::list_outbox_messages(Some("pending"), 100)?;
        if let Some(message) = messages
            .into_iter()
            .find(|message| message.platform == platform && message.channel_id == channel_id)
        {
            return Ok(message);
        }
        sleep(Duration::from_millis(25)).await;
    }

    anyhow::bail!("timed out waiting for outbox message for {platform}:{channel_id}")
}

pub(crate) struct FakeProvider;

#[async_trait]
impl LLMProvider for FakeProvider {
    fn provider_name(&self) -> &str {
        "FakeProvider"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::streaming_text_only()
    }

    async fn complete_with_tools(
        &self,
        _messages: &[Message],
        _options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>],
    ) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            content: Some("gateway ok".to_string()),
            tool_calls: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: &CompletionOptions,
        _tools: &[Arc<dyn Tool>],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ChatChunk::Content("gateway ok".to_string())),
            Ok(ChatChunk::Done),
        ])))
    }
}
