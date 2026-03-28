use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

// --- Read File Tool ---

#[derive(Clone)]
pub struct ReadFileTool {
    base_dir: PathBuf,
}

impl ReadFileTool {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file within the workspace."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file to read."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .context("Missing 'path' argument")?;

        let path = self.base_dir.join(path_str);

        // Basic sandboxing check
        if !path.starts_with(&self.base_dir) {
            anyhow::bail!("Access denied: Path is outside workspace.");
        }

        if !path.exists() {
            anyhow::bail!("File not found: {}", path_str);
        }

        fs::read_to_string(&path).context(format!("Failed to read file: {}", path_str))
    }
}

// --- Write File Tool ---

#[derive(Clone)]
pub struct WriteFileTool {
    base_dir: PathBuf,
}

impl WriteFileTool {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write text content to a file. Overwrites existing files."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "Text content to write."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .context("Missing 'path' argument")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .context("Missing 'content' argument")?;

        let path = self.base_dir.join(path_str);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&path, content).context(format!("Failed to write file: {}", path_str))?;

        Ok(format!("Successfully wrote to {}", path_str))
    }
}

// --- FileSystem Skill ---

pub struct FileSystemSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl FileSystemSkill {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            tools: vec![
                Arc::new(ReadFileTool::new(workspace_root.clone())),
                Arc::new(WriteFileTool::new(workspace_root)),
            ],
        }
    }
}

#[async_trait]
impl Skill for FileSystemSkill {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn description(&self) -> &str {
        "Tools for reading and writing files in the workspace."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
