use crate::gateways::ChatGateway;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::env;

#[cfg(feature = "headless_chrome")]
use headless_chrome::{Browser, LaunchOptions};

pub struct BrowserService;

impl BrowserService {
    pub async fn render_page(url: &str) -> Result<String> {
        #[cfg(feature = "headless_chrome")]
        {
            let browser = Browser::new(LaunchOptions {
                headless: true,
                ..Default::default()
            })?;

            let tab = browser.new_tab()?;
            tab.navigate_to(url)?;
            tab.wait_for_element("body")?;

            let content = tab.find_element("body")?.get_description()?;
            // Simple extraction: return the description/inner text
            Ok(format!("Page Content: {:?}", content))
        }
        #[cfg(not(feature = "headless_chrome"))]
        {
            anyhow::bail!("Browser feature not enabled");
        }
    }
}
