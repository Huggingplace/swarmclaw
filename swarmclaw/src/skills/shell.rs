use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use serde_json::Value;
use anyhow::{Result, Context};
use tokio::process::Command;
use std::process::Stdio;

// --- Shell Execute Tool ---

#[derive(Clone)]
pub struct ShellExecuteTool;

#[async_trait]
impl Tool for ShellExecuteTool {
    fn name(&self) -> &str {
        "shell_execute"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Use with caution."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line to execute (e.g., 'ls -la', 'git status')."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let command_str = args.get("command")
            .and_then(|v| v.as_str())
            .context("Missing 'command' argument")?;

        let cwd = args.get("cwd")
            .and_then(|v| v.as_str());

        // Use 'sh -c' on Unix, 'cmd /C' on Windows
        let (shell, arg) = if cfg!(target_os = "windows") {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };

        let mut cmd = Command::new(shell);
        cmd.arg(arg).arg(command_str);
        
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = cmd.output().await
            .context(format!("Failed to execute command: {}", command_str))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&format!("STDOUT:
{}
", stdout));
        }
        if !stderr.is_empty() {
            result.push_str(&format!("STDERR:
{}
", stderr));
        }
        result.push_str(&format!("Exit Code: {}", output.status));

        Ok(result)
    }
}

// --- Shell Skill ---

pub struct ShellSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl ShellSkill {
    pub fn new() -> Self {
        Self {
            tools: vec![Arc::new(ShellExecuteTool)],
        }
    }
}

#[async_trait]
impl Skill for ShellSkill {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute system shell commands."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
