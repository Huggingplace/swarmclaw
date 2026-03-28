use crate::config::AgentConfig;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc; // Assuming this struct exists and derives Serialize/Deserialize

// --- Update Config Tool ---

#[derive(Clone)]
pub struct UpdateConfigTool {
    workspace_path: PathBuf,
}

impl UpdateConfigTool {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }
}

#[async_trait]
impl Tool for UpdateConfigTool {
    fn name(&self) -> &str {
        "update_config"
    }

    fn description(&self) -> &str {
        "Update the agent's configuration (e.g., switch models, change name). modifying AGENTS.md."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The configuration key to update (e.g., 'model', 'name')."
                },
                "value": {
                    "type": "string",
                    "description": "The new value for the key."
                }
            },
            "required": ["key", "value"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .context("Missing key")?;
        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .context("Missing value")?;

        let agents_path = self.workspace_path.join("AGENTS.md");
        if !agents_path.exists() {
            anyhow::bail!("AGENTS.md not found at {:?}", agents_path);
        }

        let content = fs::read_to_string(&agents_path)?;

        // Simple Frontmatter Parsing
        let parts: Vec<&str> = content.splitn(3, "---").collect();

        let (frontmatter, body) = if parts.len() >= 3 {
            (parts[1], parts[2])
        } else {
            ("", content.as_str()) // Handle case with no frontmatter or just yaml
        };

        // Parse existing YAML
        let mut config_yaml: serde_yaml::Value = serde_yaml::from_str(frontmatter)
            .unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        // We assume the structure is { agents: [ { ... } ] } based on loader.rs
        // For this MVP tool, we'll try to find the first agent or a specific one.
        // To keep it robust, let's just try to update the field if we find it in the "agents" list.

        if let Some(agents) = config_yaml.get_mut("agents") {
            if let Some(agents_seq) = agents.as_sequence_mut() {
                if let Some(first_agent) = agents_seq.get_mut(0) {
                    first_agent[key] = serde_yaml::Value::String(value.to_string());
                }
            }
        } else {
            // If structure is flat (fallback)
            config_yaml[key] = serde_yaml::Value::String(value.to_string());
        }

        let new_frontmatter = serde_yaml::to_string(&config_yaml)?;

        // Reconstruct file
        let new_content = if parts.len() >= 3 {
            format!(
                "---
{}---{}",
                new_frontmatter, body
            )
        } else {
            new_frontmatter // Just YAML
        };

        fs::write(&agents_path, new_content)?;

        Ok(format!("Successfully updated '{}' to '{}'. The agent may need to restart to apply changes fully.", key, value))
    }
}

// --- Get Config Tool ---

#[derive(Clone)]
pub struct GetConfigTool {
    workspace_path: PathBuf,
}

impl GetConfigTool {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }
}

#[async_trait]
impl Tool for GetConfigTool {
    fn name(&self) -> &str {
        "get_config"
    }

    fn description(&self) -> &str {
        "Read the current agent configuration."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> Result<String> {
        let agents_path = self.workspace_path.join("AGENTS.md");
        if !agents_path.exists() {
            return Ok("No AGENTS.md configuration found.".to_string());
        }
        let content = fs::read_to_string(agents_path)?;
        Ok(content)
    }
}

// --- Config Skill ---

pub struct ConfigSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl ConfigSkill {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            tools: vec![
                Arc::new(UpdateConfigTool::new(workspace_path.clone())),
                Arc::new(GetConfigTool::new(workspace_path)),
            ],
        }
    }
}

#[async_trait]
impl Skill for ConfigSkill {
    fn name(&self) -> &str {
        "config"
    }

    fn description(&self) -> &str {
        "Tools for reading and updating agent configuration."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
