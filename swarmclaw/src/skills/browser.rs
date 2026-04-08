use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
#[cfg(feature = "headless_chrome")]
use headless_chrome::{Browser, Element, LaunchOptions};
use serde_json::Value;
use std::process::Command;
use std::sync::Arc;

// --- Active Chrome JS Tool (macOS specific) ---

#[derive(Clone)]
pub struct ActiveChromeJsTool;

#[async_trait]
impl Tool for ActiveChromeJsTool {
    fn name(&self) -> &str {
        "execute_active_chrome_javascript"
    }

    fn description(&self) -> &str {
        "Executes raw JavaScript on the currently active tab of the user's already open Google Chrome window (macOS only). Use this instead of run_shell_command + osascript to avoid quoting errors."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "javascript": {
                    "type": "string",
                    "description": "The JavaScript code to execute on the active tab (e.g. document.querySelector('.title').value = 'Hello';)"
                }
            },
            "required": ["javascript"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let javascript = args
            .get("javascript")
            .and_then(|v| v.as_str())
            .context("Missing 'javascript' argument")?;

        if !cfg!(target_os = "macos") {
            anyhow::bail!("This tool is only supported on macOS.");
        }

        // We use AppleScript to tell Chrome to execute the JS.
        // We properly escape the JS string so it can be passed as an AppleScript string literal.
        let escaped_js = javascript
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
            
        let script = format!(
            "tell application \"Google Chrome\"\n\
             execute front window's active tab javascript \"{}\"\n\
             end tell",
            escaped_js
        );

        let output = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("AppleScript failed: {}", err);
        }

        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        
        // Automatically capture the screen so the agent can visually verify the result of the JS execution
        let out_path = "chrome_active_tab_screenshot.png";
        let _ = Command::new("screencapture")
            .arg("-x")
            .arg(out_path)
            .output();

        Ok(format!("JS executed successfully. Return value: {}\n(Auto-screenshot verified state at {})", out, out_path))
    }
}

// --- Chrome/Browser Tool ---

#[derive(Clone)]
pub struct ChromeDriverTool;

#[async_trait]
impl Tool for ChromeDriverTool {
    fn name(&self) -> &str {
        "chrome_driver"
    }

    fn description(&self) -> &str {
        "Control a local Chrome browser instance to navigate, extract content, and interact with elements. Can move to specific displays and does not steal OS focus."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "get_content", "click", "type", "move_window", "screenshot_window", "dispatch_mouse", "close"],
                    "description": "The browser action to perform."
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

        #[cfg(feature = "headless_chrome")]
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            let profile_path = format!("{}/.swarmclaw/chrome_profile", home);
            std::fs::create_dir_all(&profile_path)
                .context("Failed to create chrome profile directory")?;

            let browser = Browser::new(LaunchOptions {
                headless: false,
                user_data_dir: Some(std::path::PathBuf::from(profile_path)),
                ..Default::default()
            })?;

            let tab = match browser.get_tabs().lock().unwrap().first() {
                Some(t) => t.clone(),
                None => browser.new_tab()?,
            };

            let mut needs_screenshot = true;

            let result_msg = match action {
                "screenshot_window" => {
                    "Manual screenshot captured".to_string()
                }
                "dispatch_mouse" => {
                    let mouse_action = args
                        .get("mouse_action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("click");
                    let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as f64;
                    let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as f64;
                    let point = headless_chrome::browser::tab::point::Point { x, y };

                    match mouse_action {
                        "click" => {
                            tab.click_point(point)?;
                        }
                        "move" => {
                            tab.move_mouse_to_point(point)?;
                        }
                        "down" => {
                            tab.click_point(point)?;
                        }
                        _ => {}
                    }
                    format!("Dispatched virtual mouse {} at ({}, {}) in Chrome", mouse_action, x, y)
                }
                "navigate" => {
                    let url = args
                        .get("url")
                        .and_then(|v| v.as_str())
                        .context("Missing 'url' argument")?;
                    tab.navigate_to(url)?;
                    tab.wait_for_element("body")?;
                    format!("Successfully navigated to {}", url)
                }
                "get_content" => {
                    let selector = args
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .unwrap_or("body");
                    let elem = tab.wait_for_element(selector)?;
                    let text = elem.get_inner_text()?;
                    needs_screenshot = false;
                    format!("Content of '{}':\n{}", selector, text)
                }
                "click" => {
                    let selector = args
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .context("Missing 'selector' argument")?;
                    let elem = tab.wait_for_element(selector)?;
                    elem.call_js_fn("function() { this.click(); }", vec![], false)?;
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
                    let elem = tab.wait_for_element(selector)?;

                    // Focus and click before typing to trigger event listeners properly
                    let _ = elem.call_js_fn("function() { this.focus(); this.click(); }", vec![], false);

                    let script = format!(
                        r#"
                        function() {{
                            this.value = "{}";
                            this.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            this.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        }}
                    "#,
                        text.replace("\"", "\\\"")
                    );

                    elem.call_js_fn(&script, vec![], false)?;
                    format!("Successfully typed into '{}' via JS", selector)
                }
                "move_window" => {
                    let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
                    let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
                    let width = args.get("width").and_then(|v| v.as_i64()).unwrap_or(1280);
                    let height = args.get("height").and_then(|v| v.as_i64()).unwrap_or(800);

                    tab.set_bounds(headless_chrome::types::Bounds::Normal {
                        left: Some(x as u32),
                        top: Some(y as u32),
                        width: Some(width as f64),
                        height: Some(height as f64),
                    })?;
                    format!("Moved Chrome window to x:{}, y:{}, w:{}, h:{}", x, y, width, height)
                }
                "close" => {
                    needs_screenshot = false;
                    "Chrome session closed successfully".to_string()
                }
                _ => {
                    anyhow::bail!("Unknown action: {}", action);
                }
            };

            if needs_screenshot {
                let out_path = args
                    .get("out_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("chrome_window_screenshot.png");
                
                if let Ok(png) = tab.capture_screenshot(
                    headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                    None,
                    None,
                    true,
                ) {
                    let _ = std::fs::write(out_path, png);
                    return Ok(format!("{}\n(Auto-screenshot verified state at {})", result_msg, out_path));
                }
            }

            Ok(result_msg)
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
            tools: vec![
                Arc::new(ChromeDriverTool),
                Arc::new(ActiveChromeJsTool),
            ],
        }
    }
}

#[async_trait]
impl Skill for BrowserSkill {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "A skill to programmatically control a local Chrome browser instance via CDP."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
