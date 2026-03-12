pub mod delegate;

use async_trait::async_trait;
use serde_json::Value;
use anyhow::Result;
pub use delegate::DelegateTaskTool;

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
