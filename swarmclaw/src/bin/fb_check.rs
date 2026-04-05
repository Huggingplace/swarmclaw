use anyhow::Result;
use thirtyfour::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let caps = DesiredCapabilities::firefox();
    let driver = WebDriver::new("http://localhost:4444", caps).await?;

    driver.goto("https://facebook.com/marketplace").await?;
    let title = driver.title().await?;
    println!("Page title: {}", title);

    // Check for "Create new listing" text or button
    let page_source = driver.source().await?;
    if page_source.contains("Create new listing") {
        println!("Found 'Create new listing' - user likely logged in.");
    } else {
        println!("'Create new listing' not found. User might need to log in.");
        // Print some of the source to see what's there
        if page_source.contains("Log In") {
            println!("Found 'Log In' button/text.");
        }
    }

    Ok(())
}
