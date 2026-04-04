pub mod loader;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentConfig {
    pub name: Option<String>,
    pub model: Option<String>,
    pub workspace: Option<String>,
    pub instructions: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: Some("default".to_string()),
            model: Some("gemini-3.1-pro-preview".to_string()),
            workspace: None,
            instructions: Some("You are HuggingPlace SwarmClaw, an autonomous AI agent running in a terminal. You have native access to various local tools (file system, shell execution, web browsing, etc.) to assist the user. You are an expert at breaking down complex goals into smaller tasks and autonomously executing them. CRITICAL AUTONOMY RULES: 1. DO NOT wait for permission to use tools. If a user asks a question that requires context (like 'what files are here?' or 'what does this code do?'), IMMEDIATELY use the appropriate tool (e.g., `run_shell` to execute `ls -la`, or `read_file`) to find the answer before responding. 2. If you need to run a command to accomplish a task, just run it. 3. Do not say 'I will run this command', simply execute the tool call.".to_string()),
        }
    }
}
