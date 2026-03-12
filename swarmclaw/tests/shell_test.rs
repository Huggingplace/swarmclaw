use huggingplace_swarmclaw::skills::shell::ShellSkill;
use huggingplace_swarmclaw::skills::Skill;
use serde_json::json;

#[tokio::test]
async fn test_shell_execute_tool() {
    let skill = ShellSkill::new();
    let tools = skill.tools();
    let shell_tool = tools.iter().find(|t| t.name() == "shell_execute").unwrap();

    let args = json!({
        "command": "echo 'Hello from SwarmClaw'"
    });

    let result = shell_tool.execute(args).await.unwrap();
    println!("Result: {}", result);
    assert!(result.contains("Hello from SwarmClaw"));
    assert!(result.to_lowercase().contains("exit"));
    assert!(result.contains("0"));
}

#[tokio::test]
async fn test_shell_execute_ls() {
    let skill = ShellSkill::new();
    let tools = skill.tools();
    let shell_tool = tools.iter().find(|t| t.name() == "shell_execute").unwrap();

    let args = json!({
        "command": "ls"
    });

    let result = shell_tool.execute(args).await.unwrap();
    assert!(result.contains("STDOUT:"));
    assert!(result.contains("Cargo.toml")); // Should be in the crate root
}
