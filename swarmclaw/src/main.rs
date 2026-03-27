use clap::Parser;
use swarmclaw::cli::{Args, Commands};
use swarmclaw::core::Agent;
use swarmclaw::config::{AgentConfig, loader};
use swarmclaw::llm::openai::OpenAIProvider;
use swarmclaw::skills::fs::FileSystemSkill;
use swarmclaw::skills::shell::ShellSkill;
use swarmclaw::skills::wasm::WasmSkill;
use tracing::{info, warn};
use std::sync::Arc;
use std::env;
use std::path::PathBuf;
use dotenv::dotenv;
use wasmtime::{Engine, Module};
use colored::Colorize;
use std::io::Write;

fn save_env_var(key: &str, value: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(".env") {
        let _ = writeln!(file, "{}={}", key, value);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    match args.command.unwrap_or(Commands::Run) {
        Commands::Run => run_agent(args.workspace, args.agent).await,
        Commands::Repackage { input, output } => repackage_wasm(input, output),
        Commands::Skills => list_skills(),
    }
}

fn repackage_wasm(input: String, output: Option<String>) -> anyhow::Result<()> {
    let input_path = PathBuf::from(&input);
    let output_path = output.map(PathBuf::from).unwrap_or_else(|| {
        let mut p = input_path.clone();
        p.set_extension("cwasm");
        p
    });

    info!("Repackaging WASM from {:?} to {:?}", input_path, output_path);

    let engine = Engine::default();
    let module = Module::from_file(&engine, &input_path)?;
    let serialized = module.serialize()?;
    std::fs::write(&output_path, serialized)?;

    info!("Successfully repackaged to {:?}", output_path);
    Ok(())
}

fn list_skills() -> anyhow::Result<()> {
    println!("Installed Skills:");
    println!("- filesystem (Native)");
    println!("- shell (Native)");
    // TODO: List dynamic skills
    Ok(())
}

async fn run_agent(workspace: Option<String>, agent_id: Option<String>) -> anyhow::Result<()> {
    info!("Starting HuggingPlace SwarmClaw...");
    
    // Spawn the robust SQLite outbox worker to handle outbound retries and postgres syncing
    tokio::spawn(async {
        swarmclaw::outbox::start_outbox_worker().await;
    });

    // Workspace Path
    let workspace_path = workspace
        .map(PathBuf::from)
        .unwrap_or_else(|| {
             env::var("MOTHERSHIP_WORKTREE_PATH")
                .or_else(|_| env::var("OPENCLAW_WORKSPACE"))
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
        });

    info!("Using workspace: {:?}", workspace_path);

    let google_workspace_service =
        swarmclaw::services::google_workspace::GoogleWorkspaceService::from_env(&workspace_path)
            .await?;

    // Load agents config
    let agent_configs = loader::load_from_workspace(&workspace_path)
        .unwrap_or_else(|e| {
            warn!("Failed to load AGENTS.md: {}. Using default config.", e);
            vec![AgentConfig::default()]
        });

    let agent_id = agent_id.as_deref().unwrap_or("default");
    
    let mut config = agent_configs.iter()
        .find(|c| c.name.as_deref().unwrap_or("default") == agent_id)
        .cloned()
        .unwrap_or_else(|| {
             warn!("Agent '{}' not found in config. Using default.", agent_id);
             AgentConfig::default()
        });

    info!("Selected agent: {}", config.name.as_deref().unwrap_or("unknown"));

    // LLM Setup
    let mut provider_name = env::var("LLM_PROVIDER").unwrap_or_else(|_| "".to_string());
    
    if provider_name.is_empty() && env::var("API_KEY").is_err() && env::var("OPENAI_API_KEY").is_err() {
        loop {
            println!("{}", "╔═══════════════════════════════════════════════════════════════╗".bright_blue());
            println!("{} {}", "║".bright_blue(), "Welcome to HuggingPlace SwarmClaw! 🦀🤖                       ".bold());
            println!("{} {}", "║".bright_blue(), "Choose your AI Provider:                                      ");
            println!("{} {}", "║".bright_blue(), "  1. OpenAI                                                   ");
            println!("{} {}", "║".bright_blue(), "  2. Anthropic                                                ");
            println!("{} {}", "║".bright_blue(), "  3. Gemini                                                   ");
            println!("{} {}", "║".bright_blue(), "  4. Ollama (Local)                                           ");
            println!("{}", "╚═══════════════════════════════════════════════════════════════╝".bright_blue());
            print!("{}", "\nEnter choice (1-4): ".cyan().bold());
            std::io::stdout().flush().unwrap();
            
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                continue;
            }
            
            let choice = input.trim();
            if choice.is_empty() {
                continue;
            }

            provider_name = match choice {
                "1" => "OpenAI".to_string(),
                "2" => "Anthropic".to_string(),
                "3" => "Gemini".to_string(),
                "4" => "Ollama".to_string(),
                _ => {
                    println!("{}", "Invalid choice. Please pick 1, 2, 3, or 4.".red());
                    continue;
                }
            };
            save_env_var("LLM_PROVIDER", &provider_name);
            break;
        }
    } else if provider_name.is_empty() {
        provider_name = "OpenAI".to_string(); // Default if API key is present but provider isn't
        save_env_var("LLM_PROVIDER", &provider_name);
    }

    let mut api_key = match provider_name.as_str() {
        "Anthropic" => env::var("ANTHROPIC_API_KEY").or_else(|_| env::var("API_KEY")).unwrap_or_default(),
        "Gemini" => env::var("GEMINI_API_KEY").or_else(|_| env::var("API_KEY")).unwrap_or_default(),
        "Ollama" | "Local / Custom" => "not_needed".to_string(),
        _ => env::var("OPENAI_API_KEY").or_else(|_| env::var("API_KEY")).unwrap_or_default(),
    };

    while api_key.is_empty() {
        let key_name = match provider_name.as_str() {
            "Anthropic" => "Anthropic API key (sk-ant-...)",
            "Gemini" => "Gemini API key",
            _ => "OpenAI API key (sk-...)",
        };
        print!("{}", format!("\nPlease enter your {}: ", key_name).cyan().bold());
        std::io::stdout().flush().unwrap();
        
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            continue;
        }
        api_key = input.trim().to_string();
        
        if api_key.is_empty() {
            println!("{}", "API key cannot be empty. Please try again or press Ctrl+C to exit.".red());
        } else {
            let env_key_name = match provider_name.as_str() {
                "Anthropic" => "ANTHROPIC_API_KEY",
                "Gemini" => "GEMINI_API_KEY",
                _ => "OPENAI_API_KEY",
            };
            save_env_var(env_key_name, &api_key);
        }
    }

    if config.model.is_none() || config.model.as_deref() == Some("") {
        let default_model = match provider_name.as_str() {
            "Anthropic" => "claude-3-5-sonnet-20240620",
            "Gemini" => "gemini-1.5-pro",
            "Ollama" | "Local / Custom" => "llama3",
            _ => "gpt-4o",
        };
        
        loop {
            print!("{}", format!("\nEnter model name [{}]: ", default_model).cyan().bold());
            std::io::stdout().flush().unwrap();
            
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                continue;
            }
            
            let choice = input.trim();
            if choice.is_empty() {
                config.model = Some(default_model.to_string());
                break;
            } else {
                config.model = Some(choice.to_string());
                break;
            }
        }
    }

    info!("Using LLM Provider: {} (Model: {})", provider_name, config.model.as_deref().unwrap_or("unknown"));
    
    let llm_provider: Arc<dyn swarmclaw::llm::LLMProvider> = match provider_name.as_str() {
        "Anthropic" => Arc::new(swarmclaw::llm::anthropic::AnthropicProvider::new(api_key)),
        "Gemini" => Arc::new(swarmclaw::llm::gemini::GeminiProvider::new(api_key)),
        "Local / Custom" | "Ollama" => Arc::new(swarmclaw::llm::ollama::OllamaProvider::new(env::var("OLLAMA_HOST").unwrap_or_default())),
        _ => Arc::new(OpenAIProvider::new(api_key)),
    };
    
    // Ensure Model is available (if local)
    if let Some(model_name) = &config.model {
        if !model_name.starts_with("gpt-") { // Don't fetch if it's an OpenAI model
             info!("Ensuring local model '{}' is available...", model_name);
             use swarmclaw::services::model_fetcher::ModelFetcher;
             let fetcher = ModelFetcher::new(&workspace_path);
             if let Err(e) = fetcher.ensure_model(model_name).await {
                 warn!("Failed to fetch model: {}. Agent may fail if model is required locally.", e);
             }
        }
    }

    // HuggingPlace Memory Setup
    let mut use_memory = env::var("HUGGINGPLACE_MEMORY_ENABLED").map(|v| v.to_lowercase() == "true").unwrap_or(false);
    let mut memory_api_key = env::var("HUGGINGPLACE_MEMORY_API_KEY").unwrap_or_default();
    let mut memory_email = env::var("HUGGINGPLACE_MEMORY_EMAIL").unwrap_or_default();

    if env::var("HUGGINGPLACE_MEMORY_ENABLED").is_err() {
        loop {
            print!("{}", "\nDo you want to enable HuggingPlace Memory for long-term context? (y/N): ".cyan().bold());
            std::io::stdout().flush().unwrap();
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                continue;
            }
            
            let choice = input.trim().to_lowercase();
            if choice == "y" || choice == "yes" {
                use_memory = true;
                save_env_var("HUGGINGPLACE_MEMORY_ENABLED", "true");
                
                loop {
                    print!("{}", "Enter email to provision agent account (e.g. agent@domain.com): ".cyan().bold());
                    std::io::stdout().flush().unwrap();
                    let mut email_input = String::new();
                    if std::io::stdin().read_line(&mut email_input).is_err() {
                        continue;
                    }
                    
                    let email = email_input.trim();
                    if !email.is_empty() && email.contains("@") {
                        memory_email = email.to_string();
                        save_env_var("HUGGINGPLACE_MEMORY_EMAIL", &memory_email);
                        println!("{}", format!("Registering agent identity {} with HuggingPlace...", memory_email).yellow());
                        
                        let client = reqwest::Client::new();
                        let payload = serde_json::json!({
                            "email": memory_email,
                            "org_name": "SwarmClaw User",
                            "first_name": "SwarmClaw",
                            "last_name": "Agent"
                        });
                        
                        // Attempt to provision against the HuggingPlace API
                        let res = client.post("http://localhost:8001/api/users")
                            .json(&payload)
                            .send()
                            .await;
                            
                        match res {
                            Ok(response) if response.status().is_success() => {
                                if let Ok(body) = response.json::<serde_json::Value>().await {
                                    if let Some(key) = body.get("api_key").and_then(|v| v.as_str()) {
                                        memory_api_key = key.to_string();
                                        save_env_var("HUGGINGPLACE_MEMORY_API_KEY", &memory_api_key);
                                        println!("{}", "✅ Memory identity provisioned successfully!".green());
                                        if let Some(details) = body.get("details").and_then(|v| v.get("note")).and_then(|v| v.as_str()) {
                                             println!("{} {}", "ℹ️".blue(), details.dimmed());
                                        }
                                        break;
                                    }
                                }
                                // Fallback if the structure doesn't match our exact new spec yet
                                memory_api_key = format!("sk-fallback-{}", uuid::Uuid::new_v4());
                                save_env_var("HUGGINGPLACE_MEMORY_API_KEY", &memory_api_key);
                                println!("{}", "✅ Memory identity registered (fallback parsing).".green());
                                break;
                            }
                            Ok(response) => {
                                let err_txt = response.text().await.unwrap_or_default();
                                println!("{} {}", "⚠️ Provisioning failed:".red(), err_txt);
                                // For development: let's pretend it worked if the API just rejected a duplicate
                                memory_api_key = format!("sk-dev-{}", uuid::Uuid::new_v4());
                                save_env_var("HUGGINGPLACE_MEMORY_API_KEY", &memory_api_key);
                                println!("{}", "Proceeding with dev fallback key...".yellow());
                                break;
                            }
                            Err(e) => {
                                println!("{} {}", "⚠️ Could not connect to HuggingPlace Backend:".yellow(), e);
                                println!("{}", "Proceeding in mock memory mode for development.".yellow());
                                memory_api_key = format!("sk-mock-{}", uuid::Uuid::new_v4());
                                save_env_var("HUGGINGPLACE_MEMORY_API_KEY", &memory_api_key);
                                break;
                            }
                        }
                    } else {
                        println!("{}", "Invalid email. Please try again.".red());
                    }
                }
                break;
            } else if choice == "n" || choice == "no" || choice == "" {
                save_env_var("HUGGINGPLACE_MEMORY_ENABLED", "false");
                break;
            }
        }
    }

    let mut agent = Agent::new(agent_id.to_string(), config, llm_provider);

    // Inject HuggingPlace Memory if enabled
    if use_memory && !memory_api_key.is_empty() {
        let org_id = "default-org".to_string();
        agent = agent.with_memory(org_id, memory_api_key);
        info!("HuggingPlace Memory context injection enabled.");
    }

    // Add Native Skills
    info!("Adding FileSystem skill...");
    agent.add_skill(Arc::new(FileSystemSkill::new(workspace_path.clone())));
    
        info!("Adding Shell skill...");
    
        agent.add_skill(Arc::new(ShellSkill::new()));
    
    
    
        // Add Browser Skill
    
        #[cfg(feature = "headless_chrome")]
    
        {
    
            info!("Adding Browser skill...");
    
            use swarmclaw::skills::browser::BrowserSkill;
    
                    agent.add_skill(Arc::new(BrowserSkill::new()));
    
                }
    
            
    
                    // Add Media Skill
    
            
    
                    #[cfg(feature = "image")]
    
            
    
                    {
    
            
    
                        info!("Adding Media skill...");
    
            
    
                        use swarmclaw::skills::media::MediaSkill;
    
            
    
                        agent.add_skill(Arc::new(MediaSkill::new()));
    
            
    
                    }
    
            
    
                    
    
            
    
                    // Add Configuration Skill (Always enabled)
    
            
    
                    info!("Adding Config skill...");
    
            
    
                        use swarmclaw::skills::config::ConfigSkill;
    
            
    
                        agent.add_skill(Arc::new(ConfigSkill::new(workspace_path.clone())));
    
            
    
                    
    
            
    
                        // Add Fleet Skill (if configured)
    
            
    
                        use swarmclaw::skills::fleet::FleetSkill;
    
            
    
                            if let Some(fleet_skill) = FleetSkill::new() {
    
            
    
                                info!("Adding Fleet skill...");
    
            
    
                                agent.add_skill(Arc::new(fleet_skill));
    
            
    
                            }
    
            
    
                            
    
            
    
                            // Add ClawHub Skill (for discovery/install)
    
            
    
                                info!("Adding ClawHub skill...");
                                use swarmclaw::skills::clawhub::ClawHubSkill;
                                agent.add_skill(Arc::new(ClawHubSkill::new(workspace_path.clone())));

                                let mut has_google_sheets_wasm = false;
                                let workspace_skills_dir = workspace_path.join("skills");
                                if let Ok(entries) = std::fs::read_dir(&workspace_skills_dir) {
                                    use swarmclaw::skills::wasm::WasmSkill;

                                    for entry in entries.flatten() {
                                        let path = entry.path();
                                        if path.extension().and_then(|value| value.to_str()) != Some("wasm") {
                                            continue;
                                        }

                                        match WasmSkill::new(path.clone()) {
                                            Ok(skill) => {
                                                let skill_name = swarmclaw::skills::Skill::name(&skill).to_string();
                                                if skill_name == "google_sheets" {
                                                    has_google_sheets_wasm = true;
                                                }
                                                info!("Adding WASM skill '{}' from {}", skill_name, path.display());
                                                agent.add_skill(Arc::new(skill));
                                            }
                                            Err(error) => {
                                                warn!(
                                                    "Failed to load WASM skill from {}: {}",
                                                    path.display(),
                                                    error
                                                );
                                            }
                                        }
                                    }
                                }

                                if let Some(service) = google_workspace_service {
                                    let service_url = service.ui_url();
                                    let mcp_endpoint = service.mcp_endpoint();
                                    let service_runner = service.clone();

                                    info!("Starting Google Workspace service at {}", service_url);
                                    tokio::spawn(async move {
                                        if let Err(error) = service_runner.start().await {
                                            warn!("Google Workspace service error: {}", error);
                                        }
                                    });

                                    info!("Google Sheets UI available at {}", service_url);

                                    if has_google_sheets_wasm {
                                        info!(
                                            "Google Sheets WASM skill detected in workspace; skipping native MCP registration."
                                        );
                                    } else {
                                        use swarmclaw::skills::mcp::McpSkill;
                                        match McpSkill::connect("google_sheets", &mcp_endpoint).await {
                                            Ok(skill) => {
                                                info!("Adding Google Sheets skill...");
                                                agent.add_skill(Arc::new(skill));
                                            }
                                            Err(error) => {
                                                warn!(
                                                    "Failed to register Google Sheets MCP skill from {}: {}",
                                                    mcp_endpoint, error
                                                );
                                            }
                                        }
                                    }
                                }
                            
                                // Add Delegation Tool (Next-Gen feature)
                                info!("Adding Delegation tool...");
                                use swarmclaw::tools::delegate::DelegateTaskTool;
                                // We add it as a native skill containing just this tool for now
                                    struct DelegateSkill;
                                    impl swarmclaw::skills::Skill for DelegateSkill {
                                        fn name(&self) -> &str { "delegation" }
                                        fn description(&self) -> &str { "Allows delegating tasks to other agents." }
                                        fn tools(&self) -> Vec<Arc<dyn swarmclaw::tools::Tool>> {
                                            vec![Arc::new(DelegateTaskTool)]
                                        }
                                    }
                                
                                agent.add_skill(Arc::new(DelegateSkill));
                            
                                let agent_shared = Arc::new(tokio::sync::Mutex::new(agent));
                                
                                // Start Cron Worker (Proactive Automation)
                                let cron_agent = agent_shared.clone();
                                tokio::spawn(async move {
                                    use swarmclaw::services::cron::CronWorker;
                                    let worker = CronWorker::new(cron_agent);
                                    worker.start().await;
                                });
                            
                                info!("Agent initialized. Starting run loop...");
                                
                                // Start Chat Gateways in background
                                let agent_id_str = agent_id.to_string();
                                
                                // WebRTC Signaling Gateway
                                if let Ok(ws_url) = env::var("WEBRTC_SIGNALING_URL") {
                                    info!("Starting WebRTC Signaling gateway...");
                                    use swarmclaw::gateways::webrtc_signaling::WebRTCSignalingGateway;
                                    use swarmclaw::gateways::ChatGateway;
                                    let signaling = WebRTCSignalingGateway::new(ws_url, agent_id_str.clone(), agent_shared.clone());
                                    tokio::spawn(async move {
                                        if let Err(e) = signaling.start().await {
                                            warn!("WebRTC Signaling gateway error: {}", e);
                                        }
                                    });
                                }
                            
                                #[cfg(feature = "serenity")]
                                if let Ok(token) = env::var("DISCORD_TOKEN") {
                                    info!("Starting Discord gateway...");
                                    use swarmclaw::gateways::discord::DiscordGateway;
                                    use swarmclaw::gateways::ChatGateway;
                                    let discord = DiscordGateway::new()?;
                                    tokio::spawn(async move {
                                        if let Err(e) = discord.start().await {
                                            warn!("Discord gateway error: {}", e);
                                        }
                                    });
                                }
                            
                                // Run the main REPL loop using the shared agent
                                let mut agent_lock = agent_shared.lock().await;
                                agent_lock.run().await?;
                                
                                Ok(())
                            }
                            
