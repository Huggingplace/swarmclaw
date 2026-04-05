use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use swarmclaw::tools::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Testing Firefox Skill ===");
    {
        println!("Ensure geckodriver is running on port 4444!");
        let ff_tool = swarmclaw::skills::firefox::FirefoxDriverTool::new();

        let move_res = ff_tool
            .execute(json!({
                "action": "move_window", "x": 100, "y": 100, "width": 800, "height": 600
            }))
            .await;
        println!("Firefox move_window: {:?}", move_res.is_ok());

        let nav_res = ff_tool
            .execute(json!({
                "action": "navigate", "url": "https://example.com"
            }))
            .await;
        println!("Firefox navigate: {:?}", nav_res.is_ok());

        let mouse_res = ff_tool
            .execute(json!({
                "action": "dispatch_mouse", "mouse_action": "click", "x": 200, "y": 200
            }))
            .await;
        println!("Firefox dispatch_mouse: {:?}", mouse_res.is_ok());

        let screen_res = ff_tool
            .execute(json!({
                "action": "screenshot_window", "out_path": "ff_test.png"
            }))
            .await;
        println!("Firefox screenshot_window: {:?}", screen_res.is_ok());

        let _ = ff_tool.execute(json!({"action": "close"})).await;
    }

    println!("\n=== Testing Chrome Skill ===");
    {
        let ch_tool = swarmclaw::skills::browser::ChromeDriverTool;

        let move_res = ch_tool
            .execute(json!({
                "action": "move_window", "x": 100, "y": 100, "width": 800, "height": 600
            }))
            .await;
        println!("Chrome move_window: {:?}", move_res.is_ok());

        let nav_res = ch_tool
            .execute(json!({
                "action": "navigate", "url": "https://example.com"
            }))
            .await;
        println!("Chrome navigate: {:?}", nav_res.is_ok());

        let mouse_res = ch_tool
            .execute(json!({
                "action": "dispatch_mouse", "mouse_action": "click", "x": 200, "y": 200
            }))
            .await;
        println!("Chrome dispatch_mouse: {:?}", mouse_res.is_ok());

        let screen_res = ch_tool
            .execute(json!({
                "action": "screenshot_window", "out_path": "ch_test.png"
            }))
            .await;
        println!("Chrome screenshot_window: {:?}", screen_res.is_ok());
    }

    println!("All tests complete.");
    Ok(())
}
