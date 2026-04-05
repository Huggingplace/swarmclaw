use anyhow::Result;
use serde_json::json;
use swarmclaw::tools::Tool;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let tool = swarmclaw::skills::firefox::FirefoxDriverTool::new();

    println!("Pushing Firefox window to external display...");
    let _ = tool
        .execute(json!({"action": "move_window", "x": 2000, "y": 0, "width": 1400, "height": 900}))
        .await;

    println!("Navigating to Facebook Sacramento Marketplace...");
    let _ = tool.execute(json!({"action": "navigate", "url": "https://www.facebook.com/marketplace/sacramento/"})).await;

    // Give it time to load
    sleep(Duration::from_secs(5)).await;

    println!("Taking screenshot of marketplace feed...");
    let _ = tool.execute(json!({"action": "screenshot_window", "out_path": "/Users/saumya.garg/.gemini/tmp/mothership-platform/fb_sacramento.png"})).await;

    Ok(())
}
