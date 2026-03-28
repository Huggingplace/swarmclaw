pub mod delegate;

use anyhow::Result;
use async_trait::async_trait;
pub use delegate::DelegateTaskTool;
use serde_json::Value;

#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name of the tool (e.g., "read_file")
    fn name(&self) -> &str;

    /// Description for the LLM
    fn description(&self) -> &str;

    /// JSON Schema for parameters
    fn parameters(&self) -> Value;

    /// Execution logic
    async fn execute(&self, args: Value) -> Result<String>;
}
