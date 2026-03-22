use crate::core::state::{State, Message, Role};
use crate::config::AgentConfig;
use crate::llm::{LLMProvider, CompletionOptions, ChatChunk};
use crate::skills::Skill;
use crate::safety::SafetyLayer;
use crate::security::Redactor;
use crate::worker::WorkerPool;
use crate::outbox::{enqueue_message, OutboxMessage};
use std::sync::Arc;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use futures::StreamExt;
use tokio::time::interval;

pub struct Agent {
    pub id: String,
    pub config: AgentConfig,
    pub state: State,
    pub llm: Arc<dyn LLMProvider>,
    pub skills: Vec<Arc<dyn Skill>>,
}

impl Agent {
    pub fn new(id: String, config: AgentConfig, llm: Arc<dyn LLMProvider>) -> Self {
        Self {
            id,
            config,
            state: State::default(),
            llm,
            skills: Vec::new(),
        }
    }

    pub fn add_skill(&mut self, skill: Arc<dyn Skill>) {
        self.skills.push(skill);
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        println!("🤖 HuggingPlace SwarmClaw initialized (Agent: {})", self.id);
        println!("Type 'exit' or 'quit' to stop.");
        println!("--------------------------------------------------");

        let mut stdout = io::stdout();

        loop {
            print!("> ");
            stdout.flush()?;

            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_err() {
                break;
            }

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                break;
            }

            // Apply Safety Layer
            let safe_input = match SafetyLayer::scrub_prompt(input) {
                Ok(safe) => safe,
                Err(e) => {
                    eprintln!("{}", e);
                    continue;
                }
            };

            // User Message (Input is already scrubbed, but let's redact anyway)
            let redacted_input = Redactor::redact(&safe_input);
            let timestamp = now_secs();

            let user_msg = Message {
                role: Role::User,
                content: redacted_input,
                timestamp,
            };
            self.state.history.push(user_msg);

            // Default to streaming for better UX
            self.stream_think(None).await?;
        }

        println!("Goodbye!");
        Ok(())
    }

    /// Optimized streaming thought loop with debounced outbox updates.
    /// channel_info is optional for CLI usage, but required for Discord/Telegram sync.
    pub async fn stream_think(&mut self, channel_info: Option<(String, String, String, Option<String>)>) -> anyhow::Result<()> {
        let mut stdout = io::stdout();
        let options = CompletionOptions {
            model: self.config.model.clone(),
            ..Default::default()
        };

        let tools: Vec<_> = self.skills.iter()
            .flat_map(|s| s.tools())
            .collect();

        let mut stream = self.llm.stream(&self.state.history, &options, &tools).await?;
        
        let mut full_content = String::new();
        let mut last_published_content = String::new();
        let mut publish_interval = interval(Duration::from_millis(1000));
        let mut message_id = uuid::Uuid::new_v4().to_string();

        print!("Assistant: ");
        stdout.flush()?;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(ChatChunk::Content(delta))) => {
                            full_content.push_str(&delta);
                            print!("{}", delta);
                            stdout.flush()?;
                        }
                        Some(Ok(ChatChunk::Done)) | None => {
                            println!();
                            break;
                        }
                        Some(Err(e)) => {
                            let err_msg = format!("⚠️ Engine Error: {}", e);
                            eprintln!("\n{}", err_msg);

                            if let Some((platform, channel_id, token, app_id)) = &channel_info {
                                let payload = serde_json::json!({
                                    "content": err_msg,
                                    "sender_id": "System"
                                }).to_string();

                                let outbox_msg = OutboxMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    platform: platform.clone(),
                                    channel_id: channel_id.clone(),
                                    token: token.clone(),
                                    app_id: app_id.clone(),
                                    payload,
                                    ui_components: None,
                                    created_at: now_secs() as i64,
                                    sync_status: "pending".to_string(),
                                };
                                let _ = enqueue_message(outbox_msg);
                            }
                            break;
                        }
                        _ => {} // Handle tool calls in a future iteration
                    }
                }
                _ = publish_interval.tick() => {
                    if full_content != last_published_content {
                        // Redact content before sending to outbox
                        let redacted_content = Redactor::redact(&full_content);

                        if let Some((platform, channel_id, token, app_id)) = &channel_info {
                            let payload = serde_json::json!({
                                "content": redacted_content,
                            }).to_string();

                            let outbox_msg = OutboxMessage {
                                id: message_id.clone(),
                                platform: platform.clone(),
                                channel_id: channel_id.clone(),
                                token: token.clone(),
                                app_id: app_id.clone(),
                                payload,
                                ui_components: None,
                                created_at: now_secs() as i64,
                                sync_status: "pending".to_string(),
                            };
                            
                            // Fire and forget enqueue
                            let _ = enqueue_message(outbox_msg);
                            last_published_content = full_content.clone();
                        }
                    }
                }
            }
        }

        if !full_content.is_empty() {
            // Final redaction for history
            let redacted_content = Redactor::redact(&full_content);
            self.state.history.push(Message {
                role: Role::Assistant,
                content: redacted_content,
                timestamp: now_secs(),
            });
        }

        Ok(())
    }

    pub async fn think(&mut self) -> anyhow::Result<()> {
        let mut stdout = io::stdout();

        loop {
            print!("Thinking...");
            stdout.flush()?;

            let options = CompletionOptions {
                model: self.config.model.clone(),
                ..Default::default()
            };

            // Get all tools from all skills
            let tools: Vec<_> = self.skills.iter()
                .flat_map(|s| s.tools())
                .collect();

            match self.llm.complete_with_tools(&self.state.history, &options, &tools).await {
                Ok(response) => {
                    print!("\r\x1b[K"); 
                    stdout.flush()?;

                    if let Some(content) = response.content {
                        if !content.is_empty() {
                            let redacted_content = Redactor::redact(&content);
                            println!("Assistant: {}", redacted_content);
                            self.state.history.push(Message {
                                role: Role::Assistant,
                                content: redacted_content,
                                timestamp: now_secs(),
                            });
                        }
                    }

                    if let Some(tool_calls) = response.tool_calls {
                        if tool_calls.is_empty() {
                            break;
                        }

                        for tc in tool_calls {
                            println!("🛠️  Calling tool: {} (args: {})", tc.name, tc.arguments);
                            
                            let tool = tools.iter()
                                .find(|t| t.name() == tc.name)
                                .cloned();

                            let result = match tool {
                                Some(t) => {
                                    let args: serde_json::Value = serde_json::from_str(&tc.arguments)
                                        .unwrap_or_default();
                                        
                                    // Use WorkerPool to isolate tool execution
                                    match WorkerPool::execute_tool(t, args).await {
                                        Ok(res) => res,
                                        Err(e) => format!("Error: {}", e),
                                    }
                                }
                                None => format!("Tool '{}' not found", tc.name),
                            };

                            // Redact tool results
                            let redacted_result = Redactor::redact(&result);
                            println!("✅ Tool result: {}", redacted_result);

                            self.state.history.push(Message {
                                role: Role::Tool,
                                content: redacted_result,
                                timestamp: now_secs(),
                            });
                        }
                        // Continue loop to let LLM process tool results
                        continue;
                    }

                    break;
                }
                Err(e) => {
                    print!("\r\x1b[K"); 
                    stdout.flush()?;
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
