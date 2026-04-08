pub mod analytics;
pub mod browser;
pub mod clawhub;
pub mod config;
pub mod firefox;
pub mod fleet;
pub mod fs;
pub mod mcp;
pub mod media;
pub mod memory;
pub mod shell;
pub mod wasm;

use crate::tools::Tool;
use async_trait::async_trait;
use std::sync::Arc;

/// A Skill is a collection of Tools grouped by domain
#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}
pub mod desktop;
