pub mod loader;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentConfig {
    pub name: Option<String>,
    pub model: Option<String>,
    pub workspace: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: Some("default".to_string()),
            model: Some("gpt-4o".to_string()),
            workspace: None,
        }
    }
}
