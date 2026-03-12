use tokio::time::{interval, Duration};
use tracing::{info, error};
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::core::agent::Agent;
use chrono::Timelike;

pub struct CronWorker {
    agent: Arc<Mutex<Agent>>,
}

impl CronWorker {
    pub fn new(agent: Arc<Mutex<Agent>>) -> Self {
        Self { agent }
    }

    pub async fn start(&self) {
        info!("Starting Proactive Automation (Cron Worker)");
        
        // Check every minute
        let mut interval = interval(Duration::from_secs(60));
        
        loop {
            interval.tick().await;
            
            // In a real implementation, we would check a schedule.yaml or database
            // For now, we simulate a 9 AM heartbeat check
            let now = chrono::Local::now();
            if now.minute() == 0 {
                info!("Heartbeat trigger: Checking system status at {}", now);
                
                let mut agent = self.agent.lock().await;
                let system_prompt = format!(
                    "SYSTEM: The current time is {}. Please perform your proactive system checks.",
                    now.format("%H:%M")
                );
                
                // Inject the synthetic prompt and trigger thinking
                agent.state.history.push(crate::core::state::Message {
                    role: crate::core::state::Role::System,
                    content: system_prompt,
                    timestamp: chrono::Utc::now().timestamp() as u64,
                });
                
                if let Err(e) = agent.think().await {
                    error!("Cron worker agent thinking failed: {}", e);
                }
            }
        }
    }
}
