pub mod discord;
pub mod telegram;
pub mod slack;
pub mod clawnet;

use async_trait::async_trait;
use anyhow::Result;

#[async_trait]
pub trait ChatGateway: Send + Sync {
    /// Start the gateway listener
    async fn start(&self) -> Result<()>;
    
    /// Send a message to a specific channel/user
    async fn send(&self, target_id: &str, content: &str) -> Result<()>;
}
