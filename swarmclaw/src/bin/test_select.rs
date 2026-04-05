use std::time::Duration;
use tokio::time::sleep;

struct Agent;
impl Agent {
    async fn respond(&mut self) -> Result<(), ()> {
        sleep(Duration::from_secs(1)).await;
        Ok(())
    }
    
    async fn run(&mut self) {
        let mut fut = Box::pin(self.respond());
        tokio::select! {
            res = &mut fut => println!("Done"),
            _ = sleep(Duration::from_secs(2)) => println!("Timeout"),
        }
    }
}
#[tokio::main]
async fn main() {}
