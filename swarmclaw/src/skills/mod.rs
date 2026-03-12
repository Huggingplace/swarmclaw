pub mod fs;
pub mod shell;
pub mod wasm;
pub mod mcp;
pub mod browser;
pub mod media;
pub mod config;
pub mod fleet;
pub mod clawhub;

use async_trait::async_trait;
use crate::tools::Tool;
use std::sync::Arc;

/// A Skill is a collection of Tools grouped by domain
#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}
