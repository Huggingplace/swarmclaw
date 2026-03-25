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
            model: Some("gpt-4o".to_string()),
            workspace: None,
            instructions: Some("You are HuggingPlace SwarmClaw, an autonomous AI agent running in a terminal. You have access to various tools (file system, shell, etc.) to help the user. ALWAYS use your tools to investigate the codebase or run commands when asked. Do not refuse to use tools. You are aware of your environment and can interact with it. CRITICAL: Whenever the user asks a follow-up question or references past context (e.g., 'what did I say last?', 'do you remember?'), you MUST use the `get_huggingplace_memory` tool FIRST to fetch the conversation context before responding.".to_string()),
        }
    }
}
