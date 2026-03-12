use anyhow::Result;
use crate::gateways::ChatGateway;
use async_trait::async_trait;

pub struct SlackGateway;

impl SlackGateway {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[async_trait]
impl ChatGateway for SlackGateway {
    async fn start(&self) -> Result<()> {
        println!("Slack gateway not yet implemented");
        Ok(())
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}
