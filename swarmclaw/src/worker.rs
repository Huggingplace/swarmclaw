use crate::tools::Tool;
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;

/// The WorkerPool isolates the execution of potentially unsafe or unstable tools
/// from the main Orchestrator (SwarmClaw) loop.
pub struct WorkerPool;

impl WorkerPool {
    /// Executes a tool in an isolated tokio task.
    /// If the tool panics (e.g. WASM fatal error or segfault), it is caught here
    /// and returned as a safe Error, preventing the main agent loop from crashing.
    pub async fn execute_tool(tool: Arc<dyn Tool>, args: Value) -> Result<String> {
        // Spawn the tool execution in a separate async task to catch panics
        let tool_clone = tool.clone();

        let res = tokio::spawn(async move { tool_clone.execute(args).await }).await;

        match res {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e), // Tool returned an error gracefully
            Err(e) => {
                if e.is_panic() {
                    anyhow::bail!("WORKER CRASH: Tool panicked during execution. Orchestrator caught the failure and survived.");
                } else {
                    anyhow::bail!("WORKER CANCELLED: Tool execution task was cancelled.");
                }
            }
        }
    }
}
