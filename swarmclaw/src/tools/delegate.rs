use async_trait::async_trait;
use serde_json::{json, Value};
use anyhow::{Result, Context};
use crate::tools::Tool;
use crate::outbox::{enqueue_message, OutboxMessage};
use uuid::Uuid;
use std::time::{SystemTime, UNIX_EPOCH};

/// DelegateTaskTool allows an agent to spawn sub-agents via Mothership Fleet
/// to handle complex or parallel tasks.
pub struct DelegateTaskTool;

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Delegates a complex sub-task to a specialized sub-agent. The current agent will pause until the sub-agent reports back."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_description": {
                    "type": "string",
                    "description": "A detailed description of the task for the sub-agent."
                },
                "agent_type": {
                    "type": "string",
                    "description": "The type of agent to spawn (e.g., 'researcher', 'coder', 'analyst')."
                }
            },
            "required": ["task_description", "agent_type"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let task = args["task_description"].as_str().context("Missing task_description")?;
        let agent_type = args["agent_type"].as_str().context("Missing agent_type")?;
        
        let sub_agent_id = Uuid::new_v4().to_string();
        
        // In a real implementation, this would call mothership-engine gRPC
        // For now, we simulate the delegation by enqueuing a "system" notification to the outbox
        // and returning a placeholder that indicates the agent is waiting.
        
        let payload = json!({
            "event": "sub_agent_spawned",
            "sub_agent_id": sub_agent_id,
            "task": task,
            "agent_type": agent_type,
        }).to_string();

        let msg = OutboxMessage {
            id: Uuid::new_v4().to_string(),
            platform: "internal".to_string(), // Internal event
            channel_id: "mothership-fleet".to_string(),
            token: "N/A".to_string(),
            app_id: None,
            payload,
            ui_components: None,
            created_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64,
            sync_status: "pending".to_string(),
        };

        enqueue_message(msg)?;

        Ok(format!("Successfully delegated task to sub-agent {}. I am now waiting for the result...", sub_agent_id))
    }
}
