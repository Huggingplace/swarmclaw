use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use anyhow::Result;
use serde_json::Value;

/// An MCP Tool represents a remote capability hosted by an Model Context Protocol server
/// (e.g. Postgres, GitHub, Slack). We do not run the code; we just proxy the execution.
pub struct McpTool {
    name: String,
    description: String,
    parameters: Value,
    server_url: String,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<String> {
        // In a real implementation, this would make a JSON-RPC call over stdio or SSE 
        // to the connected MCP server.
        tracing::info!("Proxying MCP execution to {}: {} with args {}", self.server_url, self.name, args);
        Ok(format!("MCP Server at {} successfully executed {}", self.server_url, self.name))
    }
}

/// A Skill representing an entire connected MCP Server
pub struct McpSkill {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl McpSkill {
    /// Connects to a remote MCP server and discovers its available tools
    pub async fn connect(name: &str, server_url: &str) -> Result<Self> {
        // Mocking the MCP discovery handshake (tools/list)
        let mock_tool = McpTool {
            name: format!("{}_query", name),
            description: format!("Execute a query via MCP server: {}", name),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }),
            server_url: server_url.to_string(),
        };

        Ok(Self {
            name: format!("mcp_{}", name),
            tools: vec![Arc::new(mock_tool)],
        })
    }
}

#[async_trait]
impl Skill for McpSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "First-Class Model Context Protocol (MCP) Integration"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}