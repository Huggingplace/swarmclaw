use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct MemorySkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl MemorySkill {
    pub fn new(id: String, org_id: Option<String>, api_key: Option<String>, workspace_path: PathBuf) -> Self {
        Self {
            tools: vec![
                Arc::new(SavePathwayTool::new(id.clone(), org_id.clone(), workspace_path)),
                Arc::new(GetPathwayTool::new(id, org_id, api_key)),
            ],
        }
    }
}

#[async_trait]
impl Skill for MemorySkill {
    fn name(&self) -> &str {
        "huggingplace_memory"
    }

    fn description(&self) -> &str {
        "Long-term memory and execution pathway tools powered by HuggingPlace."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

// --- Save Pathway Tool ---

pub struct SavePathwayTool {
    id: String,
    org_id: Option<String>,
    workspace_path: PathBuf,
}

impl SavePathwayTool {
    pub fn new(id: String, org_id: Option<String>, workspace_path: PathBuf) -> Self {
        Self { id, org_id, workspace_path }
    }
}

#[async_trait]
impl Tool for SavePathwayTool {
    fn name(&self) -> &str {
        "save_huggingplace_pathway"
    }

    fn description(&self) -> &str {
        "Saves the current successful execution steps (the 'pathway') to a local recipe pipeline log for HuggingPlace ingestion. Call this when a complex task is successfully completed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_description": {
                    "type": "string",
                    "description": "A concise summary of the task that was completed (e.g. 'Add a new MCP tool to google-sheets skill')."
                },
                "recipe_steps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The sequence of successful steps taken (e.g. ['read_file src/lib.rs', 'replace definition', 'cargo build'])."
                }
            },
            "required": ["task_description", "recipe_steps"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let task_description = args.get("task_description").and_then(|v| v.as_str()).unwrap_or("Untitled Task");
        let recipe_steps = args.get("recipe_steps").cloned().unwrap_or(json!([]));

        let log_dir = self.workspace_path.join(".swarmclaw");
        if !log_dir.exists() {
            tokio::fs::create_dir_all(&log_dir).await?;
        }
        let log_file = log_dir.join("recipes.jsonl");

        let log_entry = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "session_id": self.id,
            "org_id": self.org_id,
            "task_description": task_description,
            "recipe_steps": recipe_steps,
        });

        let mut line = log_entry.to_string();
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .await?;
        
        tokio::io::AsyncWriteExt::write_all(&mut file, line.as_bytes()).await?;

        Ok(format!("Successfully wrote pathway for '{}' to local recipe pipeline ({}).", task_description, log_file.display()))
    }
}

// --- Get Pathway Tool ---

pub struct GetPathwayTool {
    id: String,
    org_id: Option<String>,
    api_key: Option<String>,
}

impl GetPathwayTool {
    pub fn new(id: String, org_id: Option<String>, api_key: Option<String>) -> Self {
        Self { id, org_id, api_key }
    }
}

#[async_trait]
impl Tool for GetPathwayTool {
    fn name(&self) -> &str {
        "get_huggingplace_memory"
    }

    fn description(&self) -> &str {
        "Explicitly searches HuggingPlace memory for past execution pathways or facts related to the current task."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (e.g. 'how did I fix the google sheets formula bug last time?')."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let org_id = self.org_id.as_ref().context("HuggingPlace Memory is not configured (missing org_id)")?;
        let api_key = self.api_key.as_ref().context("HuggingPlace Memory is not configured (missing api_key)")?;
        
        let query = args.get("query").and_then(|v| v.as_str()).context("Missing query")?;

        let client = reqwest::Client::new();
        let payload = json!({
            "session_id": self.id,
            "user_question": query,
            "org_id": org_id,
            "should_use_memory": "YES"
        });

        let res = client
            .post("http://localhost:8001/get-memory-context")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&payload)
            .send()
            .await
            .context("Failed to connect to HuggingPlace backend")?;

        if !res.status().is_success() {
            let err_txt = res.text().await.unwrap_or_default();
            anyhow::bail!("HuggingPlace backend error: {}", err_txt);
        }

        let body: Value = res.json().await?;
        let memory_context = body.get("memory_context_used")
            .and_then(|v| v.as_str())
            .unwrap_or("No relevant memory found.");

        Ok(memory_context.to_string())
    }
}
