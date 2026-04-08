use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

// --- Shell Execute Tool ---

#[derive(Clone)]
pub struct ShellExecuteTool;

#[async_trait]
impl Tool for ShellExecuteTool {
    fn name(&self) -> &str {
        "shell_execute"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Use with extreme caution. For executing untrusted code or complex scripts, prefer 'sandboxed_shell_execute'."
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
        let command_str = args
            .get("command")
            .and_then(|v| v.as_str())
            .context("Missing 'command' argument")?;

        let cwd = args.get("cwd").and_then(|v| v.as_str());

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

        let output = cmd
            .output()
            .await
            .context(format!("Failed to execute command: {}", command_str))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&format!(
                "STDOUT:
{}
",
                stdout
            ));
        }
        if !stderr.is_empty() {
            result.push_str(&format!(
                "STDERR:
{}
",
                stderr
            ));
        }
        result.push_str(&format!("Exit Code: {}", output.status));

        Ok(result)
    }
}


// --- Sandboxed Shell Execute Tool ---

#[derive(Clone)]
pub struct SandboxedShellExecuteTool;

#[async_trait]
impl Tool for SandboxedShellExecuteTool {
    fn name(&self) -> &str {
        "sandboxed_shell_execute"
    }

    fn description(&self) -> &str {
        "Execute a shell command inside an isolated Sandbox. This is REQUIRED when writing and testing new scripts (Python, JS, Shell) to prevent system corruption, infinite loops, or resource leaks."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line to execute."
                },
                "sandbox_type": {
                    "type": "string",
                    "enum": ["docker", "mothership", "macos_vm"],
                    "description": "The isolation environment. 'docker' uses an ephemeral local container (fastest). 'mothership' offloads to the cloud carrier (good for heavy builds). 'macos_vm' uses a local macOS virtual machine (if available)."
                },
                "docker_image": {
                    "type": "string",
                    "description": "Optional: Only applies if sandbox_type is 'docker'. Default is 'ubuntu:latest'. Use 'python:3.11' or 'node:20' if specific runtimes are needed."
                }
            },
            "required": ["command", "sandbox_type"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let command_str = args
            .get("command")
            .and_then(|v| v.as_str())
            .context("Missing 'command' argument")?;

        let sandbox_type = args
            .get("sandbox_type")
            .and_then(|v| v.as_str())
            .unwrap_or("docker");

        let docker_image = args
            .get("docker_image")
            .and_then(|v| v.as_str())
            .unwrap_or("ubuntu:latest");

        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        let cwd_str = cwd.to_string_lossy();

        let (bin, final_args) = match sandbox_type {
            "docker" => {
                // Mount the current directory into the container at /workspace and run the command
                (
                    "docker".to_string(),
                    vec![
                        "run".to_string(),
                        "--rm".to_string(),
                        "-v".to_string(),
                        format!("{}:/workspace", cwd_str),
                        "-w".to_string(),
                        "/workspace".to_string(),
                        // Add limits to prevent resource exhaustion
                        "--cpus=2".to_string(),
                        "--memory=2g".to_string(),
                        docker_image.to_string(),
                        "sh".to_string(),
                        "-c".to_string(),
                        command_str.to_string(),
                    ],
                )
            }
            "mothership" => {
                // Use mothership CLI to offload execution
                (
                    "mothership".to_string(),
                    vec![
                        "fleet".to_string(),
                        "run".to_string(),
                        "--".to_string(),
                        command_str.to_string(),
                    ],
                )
            }
            "macos_vm" => {
                // Using 'tart' as the theoretical macos VM orchestrator (or similar)
                // For now, if Tart isn't available, we warn them.
                (
                    "tart".to_string(),
                    vec![
                        "run".to_string(),
                        "sequoia-base".to_string(), // Assumed VM name
                        "--dir".to_string(),
                        format!("{}:/workspace", cwd_str),
                        "/workspace".to_string(),
                        "sh".to_string(),
                        "-c".to_string(),
                        command_str.to_string(),
                    ],
                )
            }
            _ => anyhow::bail!("Unsupported sandbox_type: {}", sandbox_type),
        };

        let mut cmd = Command::new(&bin);
        for arg in &final_args {
            cmd.arg(arg);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match cmd.output().await {
            Ok(out) => out,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::bail!("Sandbox provider binary '{}' is not installed or not in PATH. Please install it or use a different sandbox_type.", bin);
                }
                anyhow::bail!("Failed to execute sandbox: {}", e);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = format!("[Sandbox Engine: {}]\n", sandbox_type);
        if !stdout.is_empty() {
            result.push_str(&format!("STDOUT:\n{}\n", stdout));
        }
        if !stderr.is_empty() {
            result.push_str(&format!("STDERR:\n{}\n", stderr));
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
            tools: vec![Arc::new(ShellExecuteTool), Arc::new(SandboxedShellExecuteTool)],
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
