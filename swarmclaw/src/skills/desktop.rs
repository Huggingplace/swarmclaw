
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use xcap::Monitor;

fn capture_screen(action: &str) -> Result<String> {
    let monitors = Monitor::all().context("Failed to get monitors")?;
    let monitor = monitors.first().context("No monitors found")?;
    let image = monitor.capture_image().context("Failed to capture image")?;
    
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let screenshots_dir = format!("{}/.swarmclaw/screenshots", home);
    let _ = std::fs::create_dir_all(&screenshots_dir);
    
    let path = format!("{}/desktop_{}_{}.png", screenshots_dir, action, chrono::Utc::now().timestamp_millis());
    image.save(&path).context("Failed to save screenshot")?;
    
    Ok(path)
}

// --- Screenshot Tool ---

#[derive(Clone)]
pub struct DesktopScreenshotTool;

#[async_trait]
impl Tool for DesktopScreenshotTool {
    fn name(&self) -> &str {
        "desktop_screenshot"
    }

    fn description(&self) -> &str {
        "Take a screenshot of the current primary desktop monitor. Returns the path to the PNG image."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: Value) -> Result<String> {
        let path = capture_screen("screenshot")?;
        Ok(format!("Screenshot saved to: {}", path))
    }
}

// --- Click Tool ---

#[derive(Clone)]
pub struct DesktopClickTool {
    enigo: Arc<Mutex<Enigo>>,
}

#[async_trait]
impl Tool for DesktopClickTool {
    fn name(&self) -> &str {
        "desktop_click"
    }

    fn description(&self) -> &str {
        "Move the mouse to absolute coordinates (x, y) and click the specified button. Automatically takes a screenshot 500ms after clicking to verify the action."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] }
            },
            "required": ["x", "y", "button"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let x = args.get("x").and_then(|v| v.as_i64()).context("Missing x")? as i32;
        let y = args.get("y").and_then(|v| v.as_i64()).context("Missing y")? as i32;
        let btn_str = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");

        let button = match btn_str {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };

        {
            let mut enigo = self.enigo.lock().await;
            enigo.move_mouse(x, y, Coordinate::Abs).context("Failed to move mouse")?;
            enigo.button(button, Direction::Click).context("Failed to click mouse")?;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let path = capture_screen("click")?;
        Ok(format!("Clicked ({}, {}) with {} button. Post-action screenshot saved to: {}", x, y, btn_str, path))
    }
}

// --- Type Tool ---

#[derive(Clone)]
pub struct DesktopTypeTool {
    enigo: Arc<Mutex<Enigo>>,
}

#[async_trait]
impl Tool for DesktopTypeTool {
    fn name(&self) -> &str {
        "desktop_type"
    }

    fn description(&self) -> &str {
        "Type a string of text using the simulated keyboard. Ensure the correct window/input field is focused first (via desktop_click). Automatically takes a screenshot 500ms after typing."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "press_enter": { "type": "boolean", "description": "If true, simulate pressing the Enter key after typing the text." }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let text = args.get("text").and_then(|v| v.as_str()).context("Missing text")?;
        let press_enter = args.get("press_enter").and_then(|v| v.as_bool()).unwrap_or(false);

        {
            let mut enigo = self.enigo.lock().await;
            enigo.text(text).context("Failed to type text")?;
            if press_enter {
                enigo.key(Key::Return, Direction::Click).context("Failed to press Enter")?;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let path = capture_screen("type")?;
        Ok(format!("Typed '{}'. Post-action screenshot saved to: {}", text, path))
    }
}

// --- Hotkey Tool ---

#[derive(Clone)]
pub struct DesktopHotkeyTool {
    enigo: Arc<Mutex<Enigo>>,
}

#[async_trait]
impl Tool for DesktopHotkeyTool {
    fn name(&self) -> &str {
        "desktop_hotkey"
    }

    fn description(&self) -> &str {
        "Simulate pressing a modifier key combination (e.g. Command+C, Ctrl+V). Use 'meta' for Command/Windows key."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "modifier": { "type": "string", "enum": ["ctrl", "alt", "shift", "meta"] },
                "key": { "type": "string", "description": "The character or key to press (e.g. 'c', 'v', 'tab', 'escape')" }
            },
            "required": ["modifier", "key"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let modifier_str = args.get("modifier").and_then(|v| v.as_str()).context("Missing modifier")?;
        let key_str = args.get("key").and_then(|v| v.as_str()).context("Missing key")?;

        let modifier = match modifier_str {
            "ctrl" => Key::Control,
            "alt" => Key::Alt,
            "shift" => Key::Shift,
            "meta" => Key::Meta,
            _ => Key::Control,
        };

        let key = match key_str.to_lowercase().as_str() {
            "tab" => Key::Tab,
            "escape" | "esc" => Key::Escape,
            "return" | "enter" => Key::Return,
            "space" => Key::Space,
            "backspace" => Key::Backspace,
            "up" => Key::UpArrow,
            "down" => Key::DownArrow,
            "left" => Key::LeftArrow,
            "right" => Key::RightArrow,
            c if c.len() == 1 => Key::Unicode(c.chars().next().unwrap()),
            _ => anyhow::bail!("Unsupported key: {}", key_str),
        };

        {
            let mut enigo = self.enigo.lock().await;
            enigo.key(modifier, Direction::Press).context("Failed to press modifier")?;
            enigo.key(key, Direction::Click).context("Failed to press key")?;
            enigo.key(modifier, Direction::Release).context("Failed to release modifier")?;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let path = capture_screen("hotkey")?;
        Ok(format!("Pressed {}+{}. Post-action screenshot saved to: {}", modifier_str, key_str, path))
    }
}

pub struct DesktopSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl DesktopSkill {
    pub fn new() -> Self {
        let enigo = Arc::new(Mutex::new(Enigo::new(&Settings::default()).unwrap()));
        
        Self {
            tools: vec![
                Arc::new(DesktopScreenshotTool),
                Arc::new(DesktopClickTool { enigo: enigo.clone() }),
                Arc::new(DesktopTypeTool { enigo: enigo.clone() }),
                Arc::new(DesktopHotkeyTool { enigo }),
            ],
        }
    }
}

#[async_trait]
impl Skill for DesktopSkill {
    fn name(&self) -> &str {
        "desktop"
    }

    fn description(&self) -> &str {
        "Control the host desktop (mouse/keyboard/screenshots). Use ONLY as a last resort when no CLI, API, or DOM automation is possible."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
