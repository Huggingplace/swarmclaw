pub mod admin_api;
pub mod browser;
pub mod control_plane_store;
pub mod cron;
pub mod google_workspace;
pub(crate) mod google_workspace_store;
pub mod memory;
pub mod model_fetcher;
pub mod web_tools;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Service: Send + Sync {
    async fn init(&self) -> Result<()>;
}
