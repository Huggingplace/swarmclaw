use crate::tools::Tool;
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Default maximum wall-clock time a single tool execution may take before it is
/// aborted and reported back to the agent loop as a timeout.
const TOOL_TIMEOUT_SECS: u64 = 300;

/// The WorkerPool isolates the execution of potentially unsafe or unstable tools
/// from the main Orchestrator (SwarmClaw) loop.
pub struct WorkerPool;

impl WorkerPool {
    /// Executes a tool in an isolated tokio task.
    /// If the tool panics (e.g. WASM fatal error or segfault), it is caught here
    /// and returned as a safe Error, preventing the main agent loop from crashing.
    /// If the tool exceeds `TOOL_TIMEOUT_SECS`, the task is aborted and a timeout
    /// error is returned so the agent can continue without hanging.
    pub async fn execute_tool(tool: Arc<dyn Tool>, args: Value) -> Result<String> {
        // Spawn the tool execution in a separate async task to catch panics
        let tool_clone = tool.clone();

        let handle = tokio::spawn(async move { tool_clone.execute(args).await });
        // Keep an abort handle so that, on timeout, we can stop the still-running
        // task instead of leaking it as a detached background task.
        let abort_handle = handle.abort_handle();

        // Wrap the join handle in a timeout so a hung tool cannot stall the loop.
        match tokio::time::timeout(Duration::from_secs(TOOL_TIMEOUT_SECS), handle).await {
            Ok(res) => match res {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(e)) => Err(e), // Tool returned an error gracefully
                Err(e) => {
                    if e.is_panic() {
                        anyhow::bail!("WORKER CRASH: Tool panicked during execution. Orchestrator caught the failure and survived.");
                    } else {
                        anyhow::bail!("WORKER CANCELLED: Tool execution task was cancelled.");
                    }
                }
            },
            Err(_elapsed) => {
                // The timeout fired; abort the still-running task and report back.
                abort_handle.abort();
                anyhow::bail!(
                    "TOOL TIMEOUT: '{}'s elapsed; tool aborted.",
                    TOOL_TIMEOUT_SECS
                );
            }
        }
    }
}
