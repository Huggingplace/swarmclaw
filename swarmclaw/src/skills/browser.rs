use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use serde_json::Value;
use anyhow::{Result, Context};
#[cfg(feature = "headless_chrome")]
use headless_chrome::{Browser, LaunchOptions};

// --- Browser Tool ---

#[derive(Clone)]
pub struct ReadWebsiteTool;

#[async_trait]
impl Tool for ReadWebsiteTool {
    fn name(&self) -> &str {
        "read_website"
    }

    fn description(&self) -> &str {
        "Read the content of a website by rendering it in a headless browser."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to visit."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        #[cfg(feature = "headless_chrome")]
        {
            let url = args.get("url")
                .and_then(|v| v.as_str())
                .context("Missing 'url' argument")?;

            let browser = Browser::new(LaunchOptions {
                headless: true,
                ..Default::default()
            })?;

            let tab = browser.new_tab()?;
            tab.navigate_to(url)?;
            tab.wait_for_element("body")?;
            
            let content = tab.find_element("body")?.get_description()?;
            
            Ok(format!("Page Content (Description): {:?}", content))
        }
        #[cfg(not(feature = "headless_chrome"))]
        {
            anyhow::bail!("Browser feature not enabled. Compile with --features headless_chrome");
        }
    }
}

// --- Browser Skill ---

pub struct BrowserSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl BrowserSkill {
    pub fn new() -> Self {
        Self {
            tools: vec![Arc::new(ReadWebsiteTool)],
        }
    }
}

#[async_trait]
impl Skill for BrowserSkill {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Tools for browsing the web."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
