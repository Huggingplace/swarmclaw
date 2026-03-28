pub(crate) mod common;
pub mod discord;
pub mod slack;
pub mod telegram;
#[cfg(test)]
pub(crate) mod test_support;
pub mod webrtc_signaling;
pub mod whatsapp;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ChatGateway: Send + Sync {
    /// Start the gateway listener
    async fn start(&self) -> Result<()>;

    /// Send a message to a specific channel/user
    async fn send(&self, target_id: &str, content: &str) -> Result<()>;
}
