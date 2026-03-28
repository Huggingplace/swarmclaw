use super::AgentConfig;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
struct AgentConfigWrapper {
    #[serde(default)]
    agents: Vec<AgentConfig>,
}

pub fn load_from_workspace(workspace_path: &Path) -> Result<Vec<AgentConfig>> {
    let agents_path = workspace_path.join("AGENTS.md");
    if !agents_path.exists() {
        return Ok(vec![AgentConfig::default()]);
    }

    let content = fs::read_to_string(&agents_path)
        .context(format!("Failed to read AGENTS.md at {:?}", agents_path))?;

    if content.starts_with("---") {
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() >= 3 {
            let yaml_str = parts[1];
            let wrapper: AgentConfigWrapper = serde_yaml::from_str(yaml_str)
                .context("Failed to parse YAML frontmatter in AGENTS.md")?;
            return Ok(wrapper.agents);
        }
    }

    // Try parsing entire file as YAML
    match serde_yaml::from_str::<AgentConfigWrapper>(&content) {
        Ok(wrapper) => Ok(wrapper.agents),
        Err(_) => anyhow::bail!("Could not parse AGENTS.md as YAML or Frontmatter"),
    }
}
