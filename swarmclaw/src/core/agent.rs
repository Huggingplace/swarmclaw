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
use colored::Colorize;

pub struct Agent {
    pub id: String,
    pub config: AgentConfig,
    pub state: State,
    pub llm: Arc<dyn LLMProvider>,
    pub skills: Vec<Arc<dyn Skill>>,
    pub memory_org_id: Option<String>,
    pub memory_api_key: Option<String>,
}

impl Agent {
    pub fn new(id: String, config: AgentConfig, llm: Arc<dyn LLMProvider>) -> Self {
        let mut state = State::default();
        if let Some(instructions) = &config.instructions {
            state.history.push(Message {
                role: Role::System,
                content: instructions.clone(),
                timestamp: now_secs(),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        
        Self {
            id,
            config,
            state,
            llm,
            skills: Vec::new(),
            memory_org_id: None,
            memory_api_key: None,
        }
    }

    pub fn with_memory(mut self, org_id: String, api_key: String) -> Self {
        self.memory_org_id = Some(org_id);
        self.memory_api_key = Some(api_key);
        self
    }

    pub fn add_skill(&mut self, skill: Arc<dyn Skill>) {
        self.skills.push(skill);
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        println!("{}", format!("🤖 HuggingPlace SwarmClaw initialized (Agent: {})", self.id).green().bold());
        println!("{}", "Type 'exit' or 'quit' to stop.".dimmed());
        println!("{}", "--------------------------------------------------".dimmed());

        let mut rl = rustyline::DefaultEditor::new()?;
        let prompt = format!("{}", "> ".cyan().bold());
        let mut stdout = io::stdout();

        loop {
            let readline = rl.readline(&prompt);
            match readline {
                Ok(input) => {
                    let input = input.trim();
                    if input.is_empty() {
                        continue;
                    }

                    if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                        break;
                    }
                    
                    let _ = rl.add_history_entry(input);

                    // Apply Safety Layer
                    let safe_input = match SafetyLayer::scrub_prompt(input) {
                        Ok(safe) => safe,
                        Err(e) => {
                            eprintln!("{}", e.to_string().red());
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
                        tool_calls: None,
                        tool_call_id: None,
                    };
                    self.state.history.push(user_msg);

                    // Default to streaming for better UX
                    while let Err(e) = self.stream_think(None).await {
                        if self.llm.is_auth_error(&e) {
                             eprintln!("\n{}", format!("⚠️ Auth Error: {}. Let's fix that.", e).red().bold());
                             print!("{}", "\nPlease enter a new API key: ".cyan().bold());
                             stdout.flush()?;
                             let mut input = String::new();
                             if std::io::stdin().read_line(&mut input).is_ok() {
                                 let key = input.trim().to_string();
                                 if !key.is_empty() {
                                     self.llm.update_api_key(key);
                                     println!("{}", "API key updated. Retrying...".green());
                                     continue;
                                 }
                             }
                        }
                        eprintln!("\n{}", format!("Error: {}", e).red());
                        break;
                    }
                },
                Err(rustyline::error::ReadlineError::Interrupted) | Err(rustyline::error::ReadlineError::Eof) => {
                    break;
                },
                Err(err) => {
                    eprintln!("{}", format!("Error: {:?}", err).red());
                    break;
                }
            }
        }

        println!("{}", "Goodbye!".green());
        Ok(())
    }

    /// Optimized streaming thought loop with debounced outbox updates.
    /// channel_info is optional for CLI usage, but required for Discord/Telegram sync.
    pub async fn stream_think(&mut self, channel_info: Option<(String, String, String, Option<String>)>) -> anyhow::Result<()> {
        let mut stdout = io::stdout();

        loop {
            let options = CompletionOptions {
                model: self.config.model.clone(),
                ..Default::default()
            };

            let tools: Vec<_> = self.skills.iter()
                .flat_map(|s| s.tools())
                .collect();

            let mut history_to_send = self.state.history.clone();

            // Inject HuggingPlace Memory automatically if configured
            if let (Some(org_id), Some(api_key)) = (&self.memory_org_id, &self.memory_api_key) {
                // Only run memory extraction if the last message was from the user
                if let Some(last_msg) = history_to_send.last() {
                    if last_msg.role == Role::User {
                        let user_question = last_msg.content.clone();
                        let mut formatted_history = String::new();
                        // Format the last few turns (e.g. 10) for context
                        for msg in history_to_send.iter().rev().take(10).rev() {
                            match msg.role {
                                Role::User => formatted_history.push_str(&format!("Human: {}\n", msg.content)),
                                Role::Assistant => formatted_history.push_str(&format!("AI: {}\n\n", msg.content)),
                                _ => {}
                            }
                        }
                        
                        let client = reqwest::Client::new();
                        let payload = serde_json::json!({
                            "session_id": channel_info.as_ref().map(|info| info.1.clone()).unwrap_or_else(|| self.id.clone()),
                            "user_question": user_question,
                            "org_id": org_id,
                            "should_use_memory": "YES",
                            "variables": serde_json::json!({ "formatted_history": formatted_history }).to_string()
                        });

                        if let Ok(res) = client.post("http://localhost:8001/get-memory-context")
                            .header("Authorization", format!("Bearer {}", api_key))
                            .json(&payload)
                            .send()
                            .await {
                            
                            if res.status().is_success() {
                                if let Ok(body) = res.json::<serde_json::Value>().await {
                                    if let Some(memory_context) = body.get("memory_context_used").and_then(|v| v.as_str()) {
                                        if !memory_context.is_empty() && memory_context.to_lowercase() != "none" {
                                            history_to_send.push(Message {
                                                role: Role::System,
                                                content: format!("Memory Context (use this to preserve continuity): {}", memory_context),
                                                timestamp: now_secs(),
                                                tool_calls: None,
                                                tool_call_id: None,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let mut stream = self.llm.stream(&history_to_send, &options, &tools).await?;
            
            let mut full_content = String::new();
            let mut last_published_content = String::new();
            let mut publish_interval = interval(Duration::from_millis(1000));
            let mut message_id = uuid::Uuid::new_v4().to_string();

            let mut tool_calls = Vec::new();
            let mut current_tool_id = String::new();
            let mut current_tool_name = String::new();
            let mut current_tool_args = String::new();

            print!("{}", "Assistant: ".magenta().bold());
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
                            Some(Ok(ChatChunk::ToolCallStart { id, name })) => {
                                if !current_tool_name.is_empty() {
                                    tool_calls.push(crate::llm::ToolCall {
                                        id: current_tool_id.clone(),
                                        name: current_tool_name.clone(),
                                        arguments: current_tool_args.clone(),
                                    });
                                }
                                current_tool_id = id;
                                current_tool_name = name;
                                current_tool_args.clear();
                            }
                            Some(Ok(ChatChunk::ToolCallDelta { arguments })) => {
                                current_tool_args.push_str(&arguments);
                            }
                            Some(Ok(ChatChunk::Done)) | None => {
                                if !current_tool_name.is_empty() {
                                    tool_calls.push(crate::llm::ToolCall {
                                        id: current_tool_id.clone(),
                                        name: current_tool_name.clone(),
                                        arguments: current_tool_args.clone(),
                                    });
                                    current_tool_name.clear();
                                }
                                println!();
                                break;
                            }
                            Some(Err(e)) => {
                                if let Some((platform, channel_id, token, app_id)) = &channel_info {
                                    let err_msg = format!("⚠️ Engine Error: {}", e);
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
                                return Err(e);
                            }
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

            let mut assistant_tool_calls: Option<Vec<serde_json::Value>> = None;
            if !tool_calls.is_empty() {
                let mut tc_vec = Vec::new();
                for tc in &tool_calls {
                    tc_vec.push(serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments,
                        }
                    }));
                }
                assistant_tool_calls = Some(tc_vec);
            }

            if !full_content.is_empty() || assistant_tool_calls.is_some() {
                // Final redaction for history
                let redacted_content = Redactor::redact(&full_content);
                self.state.history.push(Message {
                    role: Role::Assistant,
                    content: redacted_content,
                    timestamp: now_secs(),
                    tool_calls: assistant_tool_calls,
                    tool_call_id: None,
                });
            }

            if !tool_calls.is_empty() {
                for tc in tool_calls {
                    println!("🛠️  Calling tool: {} (args: {})", tc.name.cyan(), tc.arguments.dimmed());
                    
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
                    println!("✅ Tool result: {}", redacted_result.dimmed());

                    self.state.history.push(Message {
                        role: Role::Tool,
                        content: redacted_result,
                        timestamp: now_secs(),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                    });
                }
                // Continue loop to let LLM process tool results
                continue;
            }

            break;
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

            let mut history_to_send = self.state.history.clone();

            // Inject HuggingPlace Memory automatically if configured
            if let (Some(org_id), Some(api_key)) = (&self.memory_org_id, &self.memory_api_key) {
                // Only run memory extraction if the last message was from the user
                if let Some(last_msg) = history_to_send.last() {
                    if last_msg.role == Role::User {
                        let user_question = last_msg.content.clone();
                        let mut formatted_history = String::new();
                        // Format the last few turns (e.g. 10) for context
                        for msg in history_to_send.iter().rev().take(10).rev() {
                            match msg.role {
                                Role::User => formatted_history.push_str(&format!("Human: {}\n", msg.content)),
                                Role::Assistant => formatted_history.push_str(&format!("AI: {}\n\n", msg.content)),
                                _ => {}
                            }
                        }

                        let client = reqwest::Client::new();
                        let payload = serde_json::json!({
                            "session_id": self.id.clone(),
                            "user_question": user_question,
                            "org_id": org_id,
                            "should_use_memory": "YES",
                            "variables": serde_json::json!({ "formatted_history": formatted_history }).to_string()
                        });

                        if let Ok(res) = client.post("http://localhost:8001/get-memory-context")
                            .header("Authorization", format!("Bearer {}", api_key))
                            .json(&payload)
                            .send()
                            .await {

                            if res.status().is_success() {
                                if let Ok(body) = res.json::<serde_json::Value>().await {
                                    if let Some(memory_context) = body.get("memory_context_used").and_then(|v| v.as_str()) {
                                        if !memory_context.is_empty() && memory_context.to_lowercase() != "none" {
                                            history_to_send.push(Message {
                                                role: Role::System,
                                                content: format!("Memory Context (use this to preserve continuity): {}", memory_context),
                                                timestamp: now_secs(),
                                                tool_calls: None,
                                                tool_call_id: None,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            match self.llm.complete_with_tools(&history_to_send, &options, &tools).await {
                Ok(response) => {
                    print!("\r\x1b[K"); 
                    stdout.flush()?;

                    if let Some(content) = &response.content {
                        if !content.is_empty() {
                            let redacted_content = Redactor::redact(content);
                            println!("Assistant: {}", redacted_content);
                            
                            let mut assistant_tool_calls: Option<Vec<serde_json::Value>> = None;
                            if let Some(tool_calls) = &response.tool_calls {
                                if !tool_calls.is_empty() {
                                    let mut tc_vec = Vec::new();
                                    for tc in tool_calls {
                                        tc_vec.push(serde_json::json!({
                                            "id": tc.id,
                                            "type": "function",
                                            "function": {
                                                "name": tc.name,
                                                "arguments": tc.arguments,
                                            }
                                        }));
                                    }
                                    assistant_tool_calls = Some(tc_vec);
                                }
                            }

                            self.state.history.push(Message {
                                role: Role::Assistant,
                                content: redacted_content,
                                timestamp: now_secs(),
                                tool_calls: assistant_tool_calls,
                                tool_call_id: None,
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
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
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
