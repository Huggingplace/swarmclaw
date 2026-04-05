use anyhow::Result;
use std::fs;
use thirtyfour::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let caps = DesiredCapabilities::firefox();
    let driver = WebDriver::new("http://localhost:4444", caps).await?;

    println!("Navigating to Facebook Marketplace...");
    driver.goto("https://facebook.com/marketplace").await?;

    // Wait for the page to load
    tokio::time::sleep(tokio::time::Duration::from_secs(8)).await;

    // Try to find the "Create new listing" button by various methods
    let selectors = vec![
        "//span[text()='Create new listing']",
        "//span[contains(text(), 'Create new listing')]",
        "a[href='/marketplace/create/item/']",
        "div[aria-label='Create new listing']",
    ];

    let mut found = false;
    for selector in selectors {
        println!("Trying selector: {}", selector);
        let elem_res = if selector.starts_with("//") {
            driver.find(By::XPath(selector)).await
        } else {
            driver.find(By::Css(selector)).await
        };

        if let Ok(elem) = elem_res {
            println!("Found element with selector: {}. Clicking...", selector);
            if let Err(e) = elem.click().await {
                println!("Click failed: {}. Trying javascript click...", e);
                driver
                    .execute(format!("arguments[0].click();",), vec![elem.to_json()?])
                    .await?;
            }
            found = true;
            break;
        }
    }

    if !found {
        println!("Could not find 'Create new listing' button. Saving source...");
        let source = driver.source().await?;
        fs::write("fb_debug.html", source)?;
        return Ok(());
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Now look for "Item for sale"
    let item_selectors = vec![
        "//span[text()='Item for sale']",
        "//span[contains(text(), 'Item for sale')]",
        "div[aria-label='Item for sale']",
    ];

    for selector in item_selectors {
        println!("Trying item selector: {}", selector);
        let elem_res = if selector.starts_with("//") {
            driver.find(By::XPath(selector)).await
        } else {
            driver.find(By::Css(selector)).await
        };

        if let Ok(elem) = elem_res {
            println!(
                "Found 'Item for sale' with selector: {}. Clicking...",
                selector
            );
            elem.click().await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            println!("Successfully reached the Item Creation page!");
            let screenshot = driver.screenshot_as_png().await?;
            fs::write("marketplace_success.png", screenshot)?;
            break;
        }
    }

    Ok(())
}
