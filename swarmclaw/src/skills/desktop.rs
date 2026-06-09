
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};
use xcap::Monitor;

/// Lazily construct the [`Enigo`] handle the first time it is needed.
///
/// `Enigo::new` opens a connection to the platform input/display server, which
/// can fail (or panic on some backends) on a headless host or when permissions
/// are missing. We therefore build it lazily and surface any failure as a clean
/// error instead of panicking at skill construction time.
///
/// `Enigo` is not `Send`/`Sync`-friendly, so the returned reference must stay on
/// the current task; callers must hold the guard (which they do) for the
/// duration of their input operations.
fn ensure_enigo<'a>(guard: &'a mut MutexGuard<'_, Option<Enigo>>) -> Result<&'a mut Enigo> {
    if guard.is_none() {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("desktop input unavailable: {}", e))?;
        **guard = Some(enigo);
    }
    // Safe to unwrap: we just ensured it is `Some`.
    Ok(guard.as_mut().expect("enigo just initialized"))
}

/// Return the primary monitor's (width, height) in pixels, used to bounds-check
/// requested click coordinates against the visible screen.
fn primary_monitor_bounds() -> Result<(i32, i32)> {
    let monitors = Monitor::all().context("Failed to get monitors")?;
    let monitor = monitors.first().context("No monitors found")?;
    let width = monitor.width().context("Failed to read monitor width")? as i32;
    let height = monitor.height().context("Failed to read monitor height")? as i32;
    Ok((width, height))
}

fn capture_screen(action: &str) -> Result<String> {
    let monitors = Monitor::all().context("Failed to get monitors")?;
    let monitor = monitors.first().context("No monitors found")?;
    let image = monitor.capture_image().context("Failed to capture image")?;
    
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let screenshots_dir = format!("{}/.swarmclaw/screenshots", home);
    std::fs::create_dir_all(&screenshots_dir)
        .with_context(|| format!("Failed to create screenshots directory '{}'", screenshots_dir))?;

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
    enigo: Arc<Mutex<Option<Enigo>>>,
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
        let x_raw = args.get("x").and_then(|v| v.as_i64()).context("Missing x")?;
        let y_raw = args.get("y").and_then(|v| v.as_i64()).context("Missing y")?;
        let btn_str = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");

        // Bounds-check against the primary monitor BEFORE narrowing to i32, so a
        // huge coordinate is rejected with a clear error rather than silently
        // wrapping during the cast.
        let (mon_w, mon_h) = primary_monitor_bounds()?;
        if x_raw < 0 || y_raw < 0 || x_raw >= mon_w as i64 || y_raw >= mon_h as i64 {
            anyhow::bail!(
                "coordinate ({},{}) is outside screen bounds {}x{}",
                x_raw,
                y_raw,
                mon_w,
                mon_h
            );
        }
        let x = x_raw as i32;
        let y = y_raw as i32;

        let button = match btn_str {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };

        {
            let mut guard = self.enigo.lock().await;
            let enigo = ensure_enigo(&mut guard)?;
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
    enigo: Arc<Mutex<Option<Enigo>>>,
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
        // Count characters now so we can report length without echoing the
        // (possibly secret) literal text into tool output / logs.
        let char_count = text.chars().count();

        {
            let mut guard = self.enigo.lock().await;
            let enigo = ensure_enigo(&mut guard)?;
            enigo.text(text).context("Failed to type text")?;
            if press_enter {
                enigo.key(Key::Return, Direction::Click).context("Failed to press Enter")?;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let path = capture_screen("type")?;
        Ok(format!("Typed {} characters. Post-action screenshot saved to: {}", char_count, path))
    }
}

// --- Hotkey Tool ---

#[derive(Clone)]
pub struct DesktopHotkeyTool {
    enigo: Arc<Mutex<Option<Enigo>>>,
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
            let mut guard = self.enigo.lock().await;
            let enigo = ensure_enigo(&mut guard)?;
            enigo.key(modifier, Direction::Press).context("Failed to press modifier")?;
            // Capture the inner result and ALWAYS issue the modifier Release,
            // even if the keypress failed, so the modifier is never left stuck
            // down. Only after releasing do we propagate the keypress error.
            let press_res = enigo.key(key, Direction::Click).context("Failed to press key");
            let release_res = enigo
                .key(modifier, Direction::Release)
                .context("Failed to release modifier");
            press_res?;
            release_res?;
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
        // Do NOT construct Enigo here: `Enigo::new` can fail or panic on a
        // headless / no-permission host, which would take down the whole
        // process at skill-registration time. Instead share a lazily-initialized
        // slot that each tool fills in on first use (see `ensure_enigo`).
        let enigo: Arc<Mutex<Option<Enigo>>> = Arc::new(Mutex::new(None));

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
