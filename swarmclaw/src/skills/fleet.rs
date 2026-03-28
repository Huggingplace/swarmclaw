use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::sync::Arc;

// --- Spawn Fleet Tool ---

#[derive(Clone)]
pub struct SpawnFleetTool {
    client: Client,
    api_url: String,
    api_key: String,
}

impl SpawnFleetTool {
    pub fn new(api_url: String, api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_url,
            api_key,
        }
    }
}

#[async_trait]
impl Tool for SpawnFleetTool {
    fn name(&self) -> &str {
        "spawn_fleet"
    }

    fn description(&self) -> &str {
        "Spawn a fleet of SwarmClaw agents to perform a task in parallel."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "description": "Number of agents to spawn."
                },
                "command": {
                    "type": "string",
                    "description": "The command or task description for the fleet."
                },
                "cpu": {
                    "type": "number",
                    "description": "vCPU per agent (default 1.0)."
                },
                "memory_gb": {
                    "type": "number",
                    "description": "Memory per agent in GB (default 0.5)."
                }
            },
            "required": ["count", "command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let count = args
            .get("count")
            .and_then(|v| v.as_u64())
            .context("Missing count")? as u32;
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .context("Missing command")?;
        let cpu = args.get("cpu").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let memory_gb = args
            .get("memory_gb")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);

        let url = format!("{}/fleet", self.api_url);

        let payload = serde_json::json!({
            "count": count,
            "command": command,
            "image": "huggingplace/swarmclaw:latest", // Default image
            "cpu": cpu,
            "memory_gb": memory_gb
        });

        let res = self
            .client
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .context("Failed to send fleet request")?;

        let status = res.status();
        if !status.is_success() {
            let error_text = res.text().await.unwrap_or_default();
            anyhow::bail!("Fleet API Error {}: {}", status, error_text);
        }

        let body: Value = res.json().await?;
        let job_id = body["job_id"].as_str().unwrap_or("unknown");

        Ok(format!(
            "Successfully spawned fleet job '{}' with {} agents.",
            job_id, count
        ))
    }
}

// --- Fleet Skill ---

pub struct FleetSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl FleetSkill {
    pub fn new() -> Option<Self> {
        let api_key = env::var("MOTHERSHIP_API_KEY").ok()?;
        let api_url = env::var("MOTHERSHIP_API_URL")
            .unwrap_or_else(|_| "http://localhost:8000/api/v1".to_string());

        Some(Self {
            tools: vec![Arc::new(SpawnFleetTool::new(api_url, api_key))],
        })
    }
}

#[async_trait]
impl Skill for FleetSkill {
    fn name(&self) -> &str {
        "fleet"
    }

    fn description(&self) -> &str {
        "Tools for managing and spawning agent fleets."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
