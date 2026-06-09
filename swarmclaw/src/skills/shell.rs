use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Hard wall-clock limit for any spawned command. Prevents runaway commands
/// (e.g. `sleep infinity`, `yes`) from hanging the agent forever.
const SHELL_TIMEOUT_SECS: u64 = 120;

/// Maximum number of bytes captured per stream (stdout / stderr). Anything
/// beyond this is dropped with a truncation notice so a chatty command (e.g.
/// `yes`) cannot exhaust memory.
const MAX_STREAM_BYTES: usize = 100_000;

/// Truncate a captured stream to `MAX_STREAM_BYTES`, appending a notice if it
/// was clipped. Truncation happens on a UTF-8 char boundary so the lossy
/// conversion never splits a multi-byte sequence awkwardly.
fn cap_stream(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_STREAM_BYTES {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut end = MAX_STREAM_BYTES;
        while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
            // Step back off a UTF-8 continuation byte.
            end -= 1;
        }
        let mut s = String::from_utf8_lossy(&bytes[..end]).into_owned();
        s.push_str("\n...[truncated]");
        s
    }
}

/// Wait for `child` to finish, but never longer than `SHELL_TIMEOUT_SECS`.
///
/// stdout/stderr (which must have been configured as piped) are drained
/// concurrently so the child cannot deadlock on a full pipe buffer. On timeout
/// the child is force-killed and an error is returned. The collected stdout /
/// stderr are returned via [`std::process::Output`].
async fn run_with_timeout(
    mut child: tokio::process::Child,
    command_str: &str,
) -> Result<std::process::Output> {
    use tokio::io::AsyncReadExt;

    // Take the pipe handles out so we can read them while waiting on the child.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();

    let read_out = async {
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut stdout_buf).await;
        }
    };
    let read_err = async {
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut stderr_buf).await;
        }
    };

    // Drive the wait and both readers together under a single timeout.
    let combined = async {
        let (status, _, _) = tokio::join!(child.wait(), read_out, read_err);
        status
    };

    match tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), combined).await {
        Ok(status_res) => {
            let status = status_res
                .context(format!("Failed to execute command: {}", command_str))?;
            Ok(std::process::Output {
                status,
                stdout: stdout_buf,
                stderr: stderr_buf,
            })
        }
        Err(_elapsed) => {
            // Force-kill the runaway child. `start_kill` signals; `kill().await`
            // would also reap, but since `child.wait()` was being polled inside
            // the timed-out future it is no longer borrowed, so we can kill+reap
            // here directly.
            let _ = child.start_kill();
            let _ = child.wait().await;
            anyhow::bail!(
                "Command timed out after {}s and was killed.",
                SHELL_TIMEOUT_SECS
            );
        }
    }
}

// --- Shell Execute Tool ---

#[derive(Clone)]
pub struct ShellExecuteTool;

#[async_trait]
impl Tool for ShellExecuteTool {
    fn name(&self) -> &str {
        "shell_execute"
    }

    fn description(&self) -> &str {
        "Execute a shell command directly on the host with NO isolation. Use with extreme caution. \
         The command is killed after 120s and its output is capped. For untrusted code or complex \
         scripts, prefer 'sandboxed_shell_execute'."
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

        // Validate the working directory up front so a bad path produces a clear
        // error instead of an opaque spawn failure.
        if let Some(dir) = cwd {
            let p = std::path::Path::new(dir);
            if !p.is_dir() {
                anyhow::bail!(
                    "working directory '{}' does not exist or is not a directory",
                    dir
                );
            }
        }

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

        let child = cmd
            .spawn()
            .context(format!("Failed to execute command: {}", command_str))?;

        let output = run_with_timeout(child, command_str).await?;

        let stdout = cap_stream(&output.stdout);
        let stderr = cap_stream(&output.stderr);

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
        "Execute a shell command in a more isolated environment than the raw host. Prefer this when writing and testing new or untrusted scripts (Python, JS, Shell). The command is killed after 120s and its output is capped. \
         Isolation depends on 'sandbox_type': 'docker' runs in an ephemeral container with the working directory mounted READ-ONLY, no network, dropped capabilities, no new privileges, and CPU/memory/pid limits (strong but not an absolute jail; a container escape could still affect the host). 'mothership' offloads to a remote carrier. 'macos_vm' is NOT implemented and will refuse to run."
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
                    "description": "The isolation environment. 'docker' uses an ephemeral local container with a READ-ONLY workspace mount, no network, and dropped privileges (fastest; strong but not absolute isolation). 'mothership' offloads to the cloud carrier (good for heavy builds). 'macos_vm' is NOT implemented and will return an error."
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

        let cwd = std::env::current_dir().context("could not determine current dir")?;
        let cwd_str = cwd.to_string_lossy();

        let (bin, final_args) = match sandbox_type {
            "docker" => {
                // Mount the current directory into the container at /workspace
                // READ-ONLY and run the command. The hardening flags below drop
                // capabilities, block networking, and cap process/resource usage
                // so a hostile command in the container has limited reach.
                //
                // NOTE: this is defense-in-depth, NOT a perfect jail. A container
                // escape or a misconfigured Docker daemon can still affect the
                // host; treat the isolation as "strong but not absolute".
                (
                    "docker".to_string(),
                    vec![
                        "run".to_string(),
                        "--rm".to_string(),
                        "-v".to_string(),
                        // Read-only bind mount: the command can read the workspace
                        // but cannot mutate host files through it.
                        format!("{}:/workspace:ro", cwd_str),
                        "-w".to_string(),
                        "/workspace".to_string(),
                        // Hardening: no network, no extra Linux capabilities, no
                        // privilege escalation, and a bounded process count.
                        "--network=none".to_string(),
                        "--cap-drop=ALL".to_string(),
                        "--security-opt=no-new-privileges".to_string(),
                        "--pids-limit=512".to_string(),
                        // Resource limits to prevent exhaustion.
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
                // The previous 'tart' invocation was malformed and would not have
                // produced a real isolated VM, yet it was advertised to the model
                // as a safe sandbox. Refuse loudly rather than silently running
                // something that gives a false isolation guarantee.
                anyhow::bail!(
                    "macos_vm sandbox is not implemented yet; refusing to run to avoid a false isolation guarantee."
                );
            }
            _ => anyhow::bail!("Unsupported sandbox_type: {}", sandbox_type),
        };

        let mut cmd = Command::new(&bin);
        for arg in &final_args {
            cmd.arg(arg);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::bail!("Sandbox provider binary '{}' is not installed or not in PATH. Please install it or use a different sandbox_type.", bin);
                }
                anyhow::bail!("Failed to execute sandbox: {}", e);
            }
        };

        // Apply the same wall-clock timeout and output cap as the host path so a
        // runaway container cannot hang the agent or exhaust memory.
        let output = run_with_timeout(child, command_str).await?;

        let stdout = cap_stream(&output.stdout);
        let stderr = cap_stream(&output.stderr);

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
