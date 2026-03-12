use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use serde_json::Value;
use anyhow::{Result, Context};
use reqwest::Client;
use std::path::PathBuf;
use std::fs;
use futures::StreamExt;
use std::io::Write;

const CLAWHUB_API_URL: &str = "https://api.clawhub.com/v1"; // Placeholder URL

// --- Search ClawHub Tool ---

#[derive(Clone)]
pub struct SearchClawHubTool {
    client: Client,
}

#[async_trait]
impl Tool for SearchClawHubTool {
    fn name(&self) -> &str {
        "clawhub_search"
    }

    fn description(&self) -> &str {
        "Search for skills, models, and tools on ClawHub."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search term" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let query = args.get("query").and_then(|v| v.as_str()).context("Missing query")?;
        
        let url = format!("{}/search?q={}", CLAWHUB_API_URL, query);
        let res = self.client.get(&url).send().await.context("ClawHub API unreachable")?;
        
        if !res.status().is_success() {
             return Ok(format!("ClawHub Error: {}", res.status()));
        }

        let body: Value = res.json().await?;
        // Mock response for now if API doesn't exist
        let result = if body.as_array().is_some() {
            body.to_string()
        } else {
             // Mock data for demo
             serde_json::json!([
                 { "id": "skill/web-scraper", "description": "Advanced web scraping with headless browser" },
                 { "id": "model/llama-3-code", "description": "Llama 3 optimized for coding" }
             ]).to_string()
        };

        Ok(result)
    }
}

// --- Install from ClawHub Tool ---

#[derive(Clone)]
pub struct InstallClawHubTool {
    client: Client,
    workspace_path: PathBuf,
}

#[async_trait]
impl Tool for InstallClawHubTool {
    fn name(&self) -> &str {
        "clawhub_install"
    }

    fn description(&self) -> &str {
        "Download and install a resource (skill/model) from ClawHub to the local workspace."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "resource_id": { "type": "string", "description": "ID of the resource (e.g., 'skill/web-scraper')" }
            },
            "required": ["resource_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let resource_id = args.get("resource_id").and_then(|v| v.as_str()).context("Missing resource_id")?;
        
        let parts: Vec<&str> = resource_id.split('/').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid resource ID format. Expected 'type/name'");
        }
        let (res_type, name) = (parts[0], parts[1]);

        let target_dir = match res_type {
            "skill" => self.workspace_path.join("skills"),
            "model" => self.workspace_path.join("models"),
            _ => anyhow::bail!("Unknown resource type: {}", res_type),
        };
        fs::create_dir_all(&target_dir)?;

        let filename = format!("{}.wasm", name); // Simplified assumption for skills
        let target_path = target_dir.join(&filename);

        if target_path.exists() {
            return Ok(format!("Resource {} already installed at {:?}", resource_id, target_path));
        }

        // Mock Download URL
        let url = format!("{}/download/{}", CLAWHUB_API_URL, resource_id);
        
        // Simulating download logic (using the ModelFetcher pattern)
        // In real impl, fetch from signed URL
        
        // For Proof of Concept, just write a dummy file if it's a skill
        if res_type == "skill" {
            fs::write(&target_path, b"DUMMY_WASM_CONTENT")?;
            return Ok(format!("Successfully installed {} to {:?}", resource_id, target_path));
        }

        Ok(format!("Installation initiated for {}. (Mock download)", resource_id))
    }
}


// --- ClawHub Skill ---

pub struct ClawHubSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl ClawHubSkill {
    pub fn new(workspace_path: PathBuf) -> Self {
        let client = Client::new();
        Self {
            tools: vec![
                Arc::new(SearchClawHubTool { client: client.clone() }),
                Arc::new(InstallClawHubTool { client, workspace_path }),
            ],
        }
    }
}

#[async_trait]
impl Skill for ClawHubSkill {
    fn name(&self) -> &str {
        "clawhub"
    }

    fn description(&self) -> &str {
        "Interact with the ClawHub ecosystem to discover and install capabilities."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
