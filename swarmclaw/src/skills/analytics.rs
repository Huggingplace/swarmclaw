use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::info;

// --- Log Analytics Tool ---

#[derive(Clone)]
pub struct LogAnalyticsTool {
    workspace_dir: PathBuf,
}

impl LogAnalyticsTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LogAnalyticsTool {
    fn name(&self) -> &str {
        "log_analytics"
    }

    fn description(&self) -> &str {
        "Write structured analytics, context summaries, or interaction outcomes to a local telemetry log file. Use this to securely share or save important analysis for offline review."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "event_name": {
                    "type": "string",
                    "description": "A short, snake_case identifier for the event (e.g., 'task_completed', 'context_summary', 'error_encountered')."
                },
                "event_data": {
                    "type": "string",
                    "description": "Detailed payload, summary, or stringified JSON of the interaction/context."
                }
            },
            "required": ["event_name", "event_data"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let event_name = args.get("event_name").and_then(|v| v.as_str()).unwrap_or("unknown_event");
        let event_data = args.get("event_data").and_then(|v| v.as_str()).unwrap_or("");

        // Also emit via tracing for immediate stdout/file logging
        info!(target: "swarmclaw::analytics", event_name = %event_name, "{}", event_data);

        let log_dir = self.workspace_dir.join(".swarmclaw");
        if !log_dir.exists() {
            tokio::fs::create_dir_all(&log_dir).await?;
        }

        let log_file = log_dir.join("analytics.jsonl");
        let timestamp = chrono::Utc::now().to_rfc3339();

        let log_entry = serde_json::json!({
            "timestamp": timestamp,
            "event": event_name,
            "data": event_data,
        });

        let mut line = log_entry.to_string();
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .await?;

        file.write_all(line.as_bytes()).await?;

        Ok(format!(
            "Analytics event '{}' successfully recorded to {}.",
            event_name,
            log_file.display()
        ))
    }
}

// --- Skill Definition ---

pub struct AnalyticsSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl AnalyticsSkill {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            tools: vec![Arc::new(LogAnalyticsTool::new(workspace_dir))],
        }
    }
}

#[async_trait]
impl Skill for AnalyticsSkill {
    fn name(&self) -> &str {
        "analytics"
    }

    fn description(&self) -> &str {
        "Tools for recording structured analytics, interactions, and telemetry data to a local log."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
