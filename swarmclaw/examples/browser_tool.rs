use anyhow::Result;
use std::env;
use std::fs;
use thirtyfour::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: browser_tool <action1> [arg1] ... -- <action2> [arg2] ...");
        return Ok(());
    }

    let caps = DesiredCapabilities::firefox();
    let driver = WebDriver::new("http://localhost:4444", caps).await?;

    let mut i = 1;
    while i < args.len() {
        let action = &args[i];
        match action.as_str() {
            "navigate" => {
                let url = &args[i + 1];
                driver.goto(url).await?;
                println!("Navigated to {}", url);
                i += 2;
            }
            "click" => {
                let selector = &args[i + 1];
                let elem = driver.find(By::Css(selector)).await?;
                elem.click().await?;
                println!("Clicked {}", selector);
                i += 2;
            }
            "type" => {
                let selector = &args[i + 1];
                let text = &args[i + 2];
                let elem = driver.find(By::Css(selector)).await?;
                elem.send_keys(text).await?;
                println!("Typed '{}' into {}", text, selector);
                i += 3;
            }
            "wait" => {
                let secs: u64 = args[i + 1].parse()?;
                tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
                println!("Waited {} seconds", secs);
                i += 2;
            }
            "title" => {
                let title = driver.title().await?;
                println!("Page title: {}", title);
                i += 1;
            }
            "screenshot" => {
                let filename = &args[i + 1];
                let png = driver.screenshot_as_png().await?;
                fs::write(filename, png)?;
                println!("Saved screenshot to {}", filename);
                i += 2;
            }
            "source" => {
                let source = driver.source().await?;
                println!("Source length: {}", source.len());
                i += 1;
            }
            "--" => {
                i += 1;
            }
            _ => {
                println!("Unknown action: {}", action);
                break;
            }
        }
    }

    Ok(())
}
