use anyhow::Result;
use serde_json::json;
use std::env;
use swarmclaw::tools::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let ch_tool = swarmclaw::skills::browser::ChromeDriverTool;

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: cmd <json>");
        return Ok(());
    }

    let json_str = &args[1];
    let v: serde_json::Value = serde_json::from_str(json_str)?;

    let res = ch_tool.execute(v).await?;
    println!("{}", res);

    Ok(())
}
