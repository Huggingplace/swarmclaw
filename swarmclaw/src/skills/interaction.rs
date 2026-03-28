use crate::outbox::{enqueue_message, OutboxMessage};
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use uuid::Uuid;

pub static PENDING_APPROVALS: Lazy<Mutex<HashMap<String, oneshot::Sender<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// --- Request Approval Tool ---

#[derive(Clone)]
pub struct RequestApprovalTool {
    platform: String,
    channel_id: String,
}

impl RequestApprovalTool {
    pub fn new(platform: String, channel_id: String) -> Self {
        Self {
            platform,
            channel_id,
        }
    }
}

#[async_trait]
impl Tool for RequestApprovalTool {
    fn name(&self) -> &str {
        "request_approval"
    }

    fn description(&self) -> &str {
        "Request explicit user approval before performing a sensitive or costly action. Returns 'approved' or 'denied'."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message explaining why approval is needed and what the action costs."
                },
                "action_id": {
                    "type": "string",
                    "description": "A unique identifier for the action (e.g., 'spawn_gpu_vision')."
                }
            },
            "required": ["message", "action_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .context("Missing message")?;
        let action_id = args
            .get("action_id")
            .and_then(|v| v.as_str())
            .context("Missing action_id")?;

        // Construct the interactive UI components (A2UI style)
        let ui_components = serde_json::json!([
            {
                "label": "Approve",
                "action": action_id,
                "style": "primary"
            },
            {
                "label": "Deny",
                "action": format!("deny_{}", action_id),
                "style": "secondary"
            }
        ]);

        let outbox_msg = OutboxMessage {
            id: Uuid::new_v4().to_string(),
            platform: self.platform.clone(),
            channel_id: self.channel_id.clone(),
            token: "internal".to_string(),
            app_id: None,
            payload: message.to_string(),
            ui_components: Some(ui_components),
            created_at: Utc::now().timestamp(),
            sync_status: "pending".to_string(),
            attempt_count: 0,
            last_error: None,
            last_attempt_at: None,
            next_attempt_at: None,
        };

        enqueue_message(outbox_msg)?;

        let (tx, rx) = oneshot::channel();
        {
            let mut guard = PENDING_APPROVALS.lock().unwrap();
            guard.insert(action_id.to_string(), tx);
        }

        // Wait for user to reply via ClawNet gateway
        tracing::info!(
            "Approval request '{}' sent to user. Waiting for response...",
            action_id
        );

        let result = rx.await.unwrap_or_else(|_| "denied".to_string());

        tracing::info!("Received approval response: {}", result);

        if result == action_id {
            Ok("approved".to_string())
        } else {
            Ok("denied".to_string())
        }
    }
}

// --- Interaction Skill ---

pub struct InteractionSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl InteractionSkill {
    pub fn new(platform: String, channel_id: String) -> Self {
        Self {
            tools: vec![Arc::new(RequestApprovalTool::new(platform, channel_id))],
        }
    }
}

#[async_trait]
impl Skill for InteractionSkill {
    fn name(&self) -> &str {
        "interaction"
    }

    fn description(&self) -> &str {
        "Tools for human-in-the-loop interactions and declarative GUI generation."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
