pub mod browser;
pub mod memory;
pub mod model_fetcher;
pub mod cron;

use async_trait::async_trait;
use anyhow::Result;

#[async_trait]
pub trait Service: Send + Sync {
    async fn init(&self) -> Result<()>;
}
