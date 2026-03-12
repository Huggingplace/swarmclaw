use swarmclaw_sdk::SwarmClawSkill;
use serde_json::Value;
use anyhow::{Result, Context};
use std::process::Command;

pub struct ShellSkill;

impl SwarmClawSkill for ShellSkill {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute system shell commands."
    }

    fn execute(&self, args: Value) -> Result<String> {
        let command_str = args.get("command")
            .and_then(|v| v.as_str())
            .context("Missing 'command' argument")?;

        let cwd = args.get("cwd")
            .and_then(|v| v.as_str());

        // Note: In WASI, spawning processes requires special capabilities.
        // If the host (Wasmtime) hasn't granted `wasi:cli/run`, this will fail securely!
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

        let output = cmd.output()
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

// --- The WASM ABI Boilerplate ---

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> *const u8 {
    // 1. Decode FlatBuffer `PluginRequest`
    // 2. Map to ShellSkill::execute
    // 3. Encode `PluginResponse`
    std::ptr::null()
}
