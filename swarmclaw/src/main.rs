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

    // Load agents config
    let agent_configs = loader::load_from_workspace(&workspace_path)
        .unwrap_or_else(|e| {
            warn!("Failed to load AGENTS.md: {}. Using default config.", e);
            vec![AgentConfig::default()]
        });

    let agent_id = agent_id.as_deref().unwrap_or("default");
    
    let config = agent_configs.iter()
        .find(|c| c.name.as_deref().unwrap_or("default") == agent_id)
        .cloned()
        .unwrap_or_else(|| {
             warn!("Agent '{}' not found in config. Using default.", agent_id);
             AgentConfig::default()
        });

    info!("Selected agent: {}", config.name.as_deref().unwrap_or("unknown"));

    // LLM Setup
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_else(|_| {
        warn!("OPENAI_API_KEY not set. Using dummy key.");
        "dummy".to_string()
    });

    let llm_provider = Arc::new(OpenAIProvider::new(api_key));
    
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

    let mut agent = Agent::new(agent_id.to_string(), config, llm_provider);

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
                                
                                // ClawNet Gateway
                                if let Ok(ws_url) = env::var("CLAWNET_WS_URL") {
                                    info!("Starting ClawNet gateway...");
                                    use swarmclaw::gateways::clawnet::ClawNetGateway;
                                    use swarmclaw::gateways::ChatGateway;
                                    let clawnet = ClawNetGateway::new(ws_url, agent_id_str.clone(), agent_shared.clone());
                                    tokio::spawn(async move {
                                        if let Err(e) = clawnet.start().await {
                                            warn!("ClawNet gateway error: {}", e);
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
                            
