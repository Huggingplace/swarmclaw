use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;
use std::sync::Arc;
use thirtyfour::prelude::*;
use tokio::sync::Mutex;

// --- Firefox WebDriver Tool ---

pub struct FirefoxDriverTool {
    driver: Arc<Mutex<Option<WebDriver>>>,
}

impl FirefoxDriverTool {
    pub fn new() -> Self {
        Self {
            driver: Arc::new(Mutex::new(None)),
        }
    }

    async fn get_or_create_driver(&self) -> Result<WebDriver> {
        let mut lock = self.driver.lock().await;
        if let Some(driver) = lock.as_ref() {
            // Check if still alive (simple ping)
            if driver.status().await.is_ok() {
                return Ok(driver.clone());
            }
        }

        // Connect to local geckodriver
        let mut caps = DesiredCapabilities::firefox();

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let profile_path = format!("{}/.swarmclaw/firefox_profile", home);
        std::fs::create_dir_all(&profile_path)
            .context("Failed to create firefox profile directory")?;

        // Pre-flight check: Kill any orphaned Firefox processes holding this profile lock
        // Otherwise, Geckodriver will throw HTTP 500 (Process unexpectedly closed with status 0)
        let _ = Command::new("pkill")
            .arg("-f")
            .arg(&profile_path)
            .output();
        
        // Brief pause to allow OS to clean up the process and release .parentlock
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Instruct Firefox to use our persistent profile
        caps.add_arg("-profile").map_err(|e| anyhow::anyhow!(e))?;
        caps.add_arg(&profile_path)
            .map_err(|e| anyhow::anyhow!(e))?;

        let driver = WebDriver::new("http://localhost:4444", caps).await
            .context("Failed to connect to WebDriver. Ensure geckodriver is running on port 4444 (run `geckodriver -p 4444` in a terminal)")?;

        *lock = Some(driver.clone());
        Ok(driver)
    }
}

#[async_trait]
impl Tool for FirefoxDriverTool {
    fn name(&self) -> &str {
        "firefox_driver"
    }

    fn description(&self) -> &str {
        "Control a local Firefox browser instance using geckodriver to navigate, extract content, and interact with elements. Maintains session across calls. Does not steal OS focus."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "get_content", "click", "type", "move_window", "screenshot_window", "dispatch_mouse", "close"],
                    "description": "The browser action to perform. Use 'close' when finished."
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (required for 'navigate')."
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector (required for 'get_content', 'click', 'type')."
                },
                "text": {
                    "type": "string",
                    "description": "Text to type into the element (required for 'type')."
                },
                "out_path": {
                    "type": "string",
                    "description": "File path to save the screenshot (required for 'screenshot_window')."
                },
                "mouse_action": {
                    "type": "string",
                    "enum": ["click", "move", "down", "up"],
                    "description": "The specific mouse action (required for 'dispatch_mouse')."
                },
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "width": { "type": "integer" },
                "height": { "type": "integer" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .context("Missing 'action' argument")?;

        if action == "close" {
            let mut lock = self.driver.lock().await;
            if let Some(driver) = lock.take() {
                let _ = driver.quit().await;
                return Ok("Browser session closed successfully".to_string());
            }
            return Ok("No active browser session to close".to_string());
        }

        let driver = self.get_or_create_driver().await?;

        if action == "screenshot_window" {
            let out_path = args
                .get("out_path")
                .and_then(|v| v.as_str())
                .unwrap_or("firefox_window_screenshot.png");
            let png = driver.screenshot_as_png().await?;
            std::fs::write(out_path, png)?;
            return Ok(format!(
                "Screenshot of Firefox window saved to {}",
                out_path
            ));
        }

        if action == "dispatch_mouse" {
            let mouse_action = args
                .get("mouse_action")
                .and_then(|v| v.as_str())
                .unwrap_or("click");
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);

            match mouse_action {
                "click" => {
                    driver
                        .action_chain()
                        .move_by_offset(x, y)
                        .click()
                        .perform()
                        .await?;
                }
                "move" => {
                    driver.action_chain().move_by_offset(x, y).perform().await?;
                }
                "down" => {
                    driver
                        .action_chain()
                        .move_by_offset(x, y)
                        .click_and_hold()
                        .perform()
                        .await?;
                }
                "up" => {
                    driver.action_chain().release().perform().await?;
                }
                _ => anyhow::bail!("Unknown mouse action: {}", mouse_action),
            }
            return Ok(format!(
                "Dispatched virtual mouse {} at ({}, {})",
                mouse_action, x, y
            ));
        }

        let result = match action {
            "navigate" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .context("Missing 'url' argument")?;
                driver.goto(url).await?;
                format!("Successfully navigated to {}", url)
            }
            "get_content" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("body");
                let elem = driver.find(By::Css(selector)).await?;
                let text = elem.text().await?;
                format!("Content of '{}':\n{}", selector, text)
            }
            "click" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .context("Missing 'selector' argument")?;
                let elem = driver.find(By::Css(selector)).await?;
                // Use JS injection to prevent OS window focus stealing
                driver
                    .execute(r#"arguments[0].click();"#, vec![elem.to_json()?])
                    .await?;
                format!("Successfully clicked element '{}' via JS", selector)
            }
            "type" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .context("Missing 'selector' argument")?;
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .context("Missing 'text' argument")?;
                let elem = driver.find(By::Css(selector)).await?;
                // Use JS injection to set value and trigger React/Vue events without stealing keyboard focus
                // Focus and click before typing to trigger event listeners properly
                let _ = driver.execute(r#"arguments[0].focus(); arguments[0].click();"#, vec![elem.to_json()?]).await;

                let script = r#"
                    let el = arguments[0];
                    el.value = arguments[1];
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    el.dispatchEvent(new Event('change', { bubbles: true }));
                "#;
                driver
                    .execute(script, vec![elem.to_json()?, serde_json::json!(text)])
                    .await?;
                format!("Successfully typed into '{}' via JS", selector)
            }
            "move_window" => {
                let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
                let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
                let width = args.get("width").and_then(|v| v.as_i64()).unwrap_or(1280);
                let height = args.get("height").and_then(|v| v.as_i64()).unwrap_or(800);

                driver
                    .set_window_rect(x, y, width as u32, height as u32)
                    .await?;
                format!(
                    "Moved window to x:{}, y:{}, w:{}, h:{}",
                    x, y, width, height
                )
            }
            _ => {
                anyhow::bail!("Unknown action: {}", action);
            }
        };

        Ok(result)
    }
}

// --- Firefox Skill ---

pub struct FirefoxSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl FirefoxSkill {
    pub fn new() -> Self {
        Self {
            tools: vec![Arc::new(FirefoxDriverTool::new())],
        }
    }
}

#[async_trait]
impl Skill for FirefoxSkill {
    fn name(&self) -> &str {
        "firefox"
    }

    fn description(&self) -> &str {
        "A skill to programmatically control a persistent local Firefox browser instance via WebDriver."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
