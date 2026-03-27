pub mod browser;
pub mod cron;
pub mod google_workspace;
pub(crate) mod google_workspace_store;
pub mod memory;
pub mod model_fetcher;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Service: Send + Sync {
    async fn init(&self) -> Result<()>;
}
