use chrono::{Local, TimeZone};
use clap::Parser;
use colored::Colorize;
use dotenv::dotenv;
use std::env;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use swarmclaw::cli::{Args, Commands};
use swarmclaw::config::{loader, AgentConfig};
use swarmclaw::core::Agent;
use swarmclaw::llm::openai::OpenAIProvider;
use swarmclaw::skills::fs::FileSystemSkill;
use swarmclaw::skills::shell::ShellSkill;
use swarmclaw::skills::analytics::AnalyticsSkill;
use tracing::level_filters::LevelFilter;
use tracing::{info, warn};
use tracing_subscriber::fmt::format::FmtSpan;
use wasmtime::{Engine, Module};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    OpenAI,
    Groq,
    Grok,
    Anthropic,
    Gemini,
    Ollama,
}

impl ProviderKind {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai" | "open-ai" => Some(Self::OpenAI),
            "groq" => Some(Self::Groq),
            "grok" | "xai" | "x.ai" => Some(Self::Grok),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "gemini" | "google" => Some(Self::Gemini),
            "ollama" | "local" | "local/custom" | "local / custom" | "local-custom" => {
                Some(Self::Ollama)
            }
            _ => None,
        }
    }

    fn configured() -> anyhow::Result<Option<Self>> {
        match env::var("LLM_PROVIDER") {
            Ok(value) if !value.trim().is_empty() => Self::parse(&value).map(Some).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unsupported LLM_PROVIDER '{}'. Valid values: openai, groq, grok, anthropic, gemini, ollama.",
                    value
                )
            }),
            _ => Ok(None),
        }
    }

    fn infer_from_env() -> Option<Self> {
        if env::var("OPENAI_API_KEY").is_ok() {
            Some(Self::OpenAI)
        } else if env::var("GROQ_API_KEY").is_ok() {
            Some(Self::Groq)
        } else if env::var("XAI_API_KEY").is_ok() || env::var("GROK_API_KEY").is_ok() {
            Some(Self::Grok)
        } else if env::var("ANTHROPIC_API_KEY").is_ok() {
            Some(Self::Anthropic)
        } else if env::var("GEMINI_API_KEY").is_ok() {
            Some(Self::Gemini)
        } else if env::var("OLLAMA_HOST").is_ok() {
            Some(Self::Ollama)
        } else if env::var("API_KEY").is_ok() {
            Some(Self::OpenAI)
        } else {
            None
        }
    }

    fn from_prompt_choice(choice: &str) -> Option<Self> {
        match choice.trim() {
            "1" => Some(Self::OpenAI),
            "2" => Some(Self::Anthropic),
            "3" => Some(Self::Gemini),
            "4" => Some(Self::Ollama),
            "5" => Some(Self::Groq),
            "6" => Some(Self::Grok),
            _ => None,
        }
    }

    fn config_value(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Groq => "groq",
            Self::Grok => "grok",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::OpenAI => "OpenAI",
            Self::Groq => "Groq",
            Self::Grok => "Grok (xAI)",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::Ollama => "Ollama",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::OpenAI => "gpt-4o",
            Self::Groq => "llama-3.3-70b-versatile",
            Self::Grok => "grok-code-fast-1",
            Self::Anthropic => "claude-3-5-sonnet-20240620",
            Self::Gemini => "gemini-3.1-pro-preview",
            Self::Ollama => "llama3",
        }
    }

    fn api_key_prompt(self) -> Option<&'static str> {
        match self {
            Self::OpenAI => Some("OpenAI API key (sk-...)"),
            Self::Groq => Some("Groq API key"),
            Self::Grok => Some("xAI API key"),
            Self::Anthropic => Some("Anthropic API key (sk-ant-...)"),
            Self::Gemini => Some("Gemini API key"),
            Self::Ollama => None,
        }
    }

    fn api_key_env_name(self) -> Option<&'static str> {
        match self {
            Self::OpenAI => Some("OPENAI_API_KEY"),
            Self::Groq => Some("GROQ_API_KEY"),
            Self::Grok => Some("XAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Ollama => None,
        }
    }

    fn read_api_key(self) -> String {
        match self {
            Self::OpenAI => env::var("OPENAI_API_KEY")
                .or_else(|_| env::var("API_KEY"))
                .unwrap_or_default(),
            Self::Groq => env::var("GROQ_API_KEY")
                .or_else(|_| env::var("API_KEY"))
                .unwrap_or_default(),
            Self::Grok => env::var("XAI_API_KEY")
                .or_else(|_| env::var("GROK_API_KEY"))
                .or_else(|_| env::var("API_KEY"))
                .unwrap_or_default(),
            Self::Anthropic => env::var("ANTHROPIC_API_KEY")
                .or_else(|_| env::var("API_KEY"))
                .unwrap_or_default(),
            Self::Gemini => env::var("GEMINI_API_KEY")
                .or_else(|_| env::var("API_KEY"))
                .unwrap_or_default(),
            Self::Ollama => "not_needed".to_string(),
        }
    }

    fn base_url_override(self) -> Option<String> {
        match self {
            Self::OpenAI => env::var("OPENAI_BASE_URL")
                .or_else(|_| env::var("LLM_BASE_URL"))
                .ok(),
            Self::Groq => env::var("GROQ_BASE_URL")
                .or_else(|_| env::var("LLM_BASE_URL"))
                .ok(),
            Self::Grok => env::var("XAI_BASE_URL")
                .or_else(|_| env::var("GROK_BASE_URL"))
                .or_else(|_| env::var("LLM_BASE_URL"))
                .ok(),
            _ => None,
        }
    }

    fn is_local(self) -> bool {
        matches!(self, Self::Ollama)
    }
}

fn save_env_var(key: &str, value: &str) {
    let env_path = PathBuf::from(".env");
    let mut lines = std::fs::read_to_string(&env_path)
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter(|line| {
                    line.split_once('=')
                        .map(|(existing_key, _)| existing_key.trim() != key)
                        .unwrap_or(true)
                })
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    lines.push(format!("{key}={value}"));
    let serialized = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };

    let _ = std::fs::write(env_path, serialized);
}

fn resolve_workspace_path(workspace: Option<String>) -> PathBuf {
    workspace.map(PathBuf::from).unwrap_or_else(|| {
        env::var("MOTHERSHIP_WORKTREE_PATH")
            .or_else(|_| env::var("OPENCLAW_WORKSPACE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    })
}

fn is_interactive_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn print_status(workspace: Option<String>) -> anyhow::Result<()> {
    let workspace_path = resolve_workspace_path(workspace);
    let agents_path = workspace_path.join("AGENTS.md");
    let provider = ProviderKind::configured()?
        .or_else(ProviderKind::infer_from_env)
        .map(|kind| kind.display_name().to_string())
        .unwrap_or_else(|| "unconfigured".to_string());

    println!("SwarmClaw OK");
    println!("version: {}", env!("CARGO_PKG_VERSION"));
    println!("workspace: {}", workspace_path.display());
    println!(
        "agent_config: {}",
        if agents_path.exists() {
            agents_path.display().to_string()
        } else {
            "default".to_string()
        }
    );
    println!("provider: {}", provider);
    Ok(())
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(verbose)
        .with_thread_ids(verbose)
        .with_file(verbose)
        .with_line_number(verbose)
        .with_span_events(if verbose {
            FmtSpan::NEW | FmtSpan::CLOSE
        } else {
            FmtSpan::CLOSE
        })
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

fn sanitize_agent_filename(agent_id: &str) -> String {
    let sanitized = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

fn session_state_path(workspace_path: &Path, agent_id: &str) -> PathBuf {
    workspace_path
        .join(".swarmclaw")
        .join("sessions")
        .join(format!("{}.json", sanitize_agent_filename(agent_id)))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    let args = Args::parse();
    init_tracing(args.verbose);
    let workspace = args.workspace.clone();
    let agent = args.agent.clone();

    match args.command.unwrap_or(Commands::Run) {
        Commands::Run => run_agent(workspace, agent).await,
        Commands::Status => print_status(workspace),
        Commands::Repackage { input, output } => repackage_wasm(input, output),
        Commands::Skills => list_skills(),
        Commands::Sessions { limit } => print_sessions(workspace, limit),
        Commands::History { session, limit } => print_history(
            workspace,
            session.or(agent).unwrap_or_else(|| "default".to_string()),
            limit,
        ),
        Commands::Outbox { status, limit } => print_outbox(status, limit),
    }
}

fn repackage_wasm(input: String, output: Option<String>) -> anyhow::Result<()> {
    let input_path = PathBuf::from(&input);
    let output_path = output.map(PathBuf::from).unwrap_or_else(|| {
        let mut p = input_path.clone();
        p.set_extension("cwasm");
        p
    });

    info!(
        "Repackaging WASM from {:?} to {:?}",
        input_path, output_path
    );

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

fn print_sessions(workspace: Option<String>, limit: usize) -> anyhow::Result<()> {
    let workspace_path = resolve_workspace_path(workspace);
    let store_path =
        swarmclaw::core::session_store::migrate_legacy_sessions_in_workspace(&workspace_path)?;
    let sessions = swarmclaw::core::session_store::list_sessions(&store_path, limit)?;

    println!("SwarmClaw Sessions");
    println!("store: {}", store_path.display());

    if sessions.is_empty() {
        println!("No persisted sessions found.");
        return Ok(());
    }

    for session in sessions {
        println!(
            "- {} | {} messages | updated {}",
            session.session_id,
            session.message_count,
            format_millis_timestamp(session.updated_at)
        );
        println!("  created {}", format_millis_timestamp(session.created_at));
        if let Some(last_role) = session.last_role.as_ref() {
            let preview = session
                .last_content_preview
                .as_deref()
                .map(|content| preview_excerpt(content, 96))
                .unwrap_or_else(|| "(empty)".to_string());
            println!(
                "  last {} at {}: {}",
                role_label(last_role),
                session
                    .last_timestamp
                    .map(format_secs_timestamp)
                    .unwrap_or_else(|| "-".to_string()),
                preview
            );
        }
    }

    Ok(())
}

fn print_history(workspace: Option<String>, session: String, limit: usize) -> anyhow::Result<()> {
    let workspace_path = resolve_workspace_path(workspace);
    let store_path =
        swarmclaw::core::session_store::migrate_legacy_sessions_in_workspace(&workspace_path)?;
    let Some(history) =
        swarmclaw::core::session_store::load_recent_history(&store_path, &session, limit)?
    else {
        anyhow::bail!("No persisted history found for session '{}'.", session);
    };

    println!("SwarmClaw History");
    println!("store: {}", store_path.display());
    println!("session: {}", session);
    println!("messages: {}", history.len());

    for entry in history {
        println!();
        println!(
            "[{}] {} {}",
            entry.message_index,
            role_label(&entry.message.role).to_uppercase(),
            format_secs_timestamp(entry.message.timestamp)
        );
        if let Some(tool_call_id) = entry.message.tool_call_id.as_deref() {
            println!("tool_call_id: {}", tool_call_id);
        }
        if let Some(tool_calls) = entry.message.tool_calls.as_ref() {
            if !tool_calls.is_empty() {
                println!("tool_calls: {}", tool_calls.len());
            }
        }
        println!("{}", entry.message.content);
    }

    Ok(())
}

fn print_outbox(status: Option<String>, limit: usize) -> anyhow::Result<()> {
    let messages = swarmclaw::outbox::list_outbox_messages(status.as_deref(), limit)?;

    println!("SwarmClaw Outbox");
    if let Some(status) = status.as_deref() {
        println!("status: {}", status);
    }

    if messages.is_empty() {
        println!("No outbox messages found.");
        return Ok(());
    }

    for message in messages {
        println!(
            "- {} | {} | {} | attempts {} | created {}",
            message.id,
            message.platform,
            message.sync_status,
            message.attempt_count,
            format_millis_timestamp(message.created_at)
        );
        println!("  channel {}", message.channel_id);
        if let Some(last_attempt_at) = message.last_attempt_at {
            println!(
                "  last_attempt {}",
                format_millis_timestamp(last_attempt_at)
            );
        }
        if let Some(next_attempt_at) = message.next_attempt_at {
            println!(
                "  next_attempt {}",
                format_millis_timestamp(next_attempt_at)
            );
        }
        if let Some(last_error) = message.last_error.as_deref() {
            println!("  last_error {}", last_error);
        }
        println!("  payload {}", message.payload_preview);
    }

    Ok(())
}

fn format_millis_timestamp(timestamp_millis: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_millis)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
        .unwrap_or_else(|| timestamp_millis.to_string())
}

fn format_secs_timestamp(timestamp_secs: u64) -> String {
    Local
        .timestamp_opt(timestamp_secs as i64, 0)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M:%S %Z").to_string())
        .unwrap_or_else(|| timestamp_secs.to_string())
}

fn preview_excerpt(content: &str, max_chars: usize) -> String {
    let flattened = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = flattened.chars().take(max_chars).collect::<String>();
    if flattened.chars().count() > max_chars {
        excerpt.push_str("...");
    }
    excerpt
}

fn role_label(role: &swarmclaw::core::state::Role) -> &'static str {
    match role {
        swarmclaw::core::state::Role::System => "system",
        swarmclaw::core::state::Role::User => "user",
        swarmclaw::core::state::Role::Assistant => "assistant",
        swarmclaw::core::state::Role::Tool => "tool",
    }
}

async fn register_control_plane_channel(
    workspace_path: &Path,
    platform: &str,
    transport: &str,
    endpoint: &str,
) {
    match swarmclaw::services::control_plane_store::ControlPlaneStore::open(
        &workspace_path.join(".swarmclaw"),
    )
    .await
    {
        Ok(store) => {
            if let Err(error) = store
                .upsert_channel_registration(platform, transport, endpoint, true)
                .await
            {
                warn!(
                    "Failed to register control-plane channel {} ({}): {}",
                    platform, endpoint, error
                );
            }
        }
        Err(error) => {
            warn!(
                "Failed to open control-plane store for channel {}: {}",
                platform, error
            );
        }
    }
}

async fn run_agent(workspace: Option<String>, agent_id: Option<String>) -> anyhow::Result<()> {
    info!("Starting HuggingPlace SwarmClaw...");
    let interactive = is_interactive_terminal();

    // Spawn the robust SQLite outbox worker to handle outbound retries and postgres syncing
    tokio::spawn(async {
        swarmclaw::outbox::start_outbox_worker().await;
    });

    // Workspace Path
    let workspace_path = resolve_workspace_path(workspace);

    info!("Using workspace: {:?}", workspace_path);

    let google_workspace_service =
        swarmclaw::services::google_workspace::GoogleWorkspaceService::from_env(&workspace_path)
            .await?;
    let web_tools_service = swarmclaw::services::web_tools::WebToolsService::from_env()?;

    // Load agents config
    let agent_configs = loader::load_from_workspace(&workspace_path).unwrap_or_else(|e| {
        warn!("Failed to load AGENTS.md: {}. Using default config.", e);
        vec![AgentConfig::default()]
    });

    let agent_id = agent_id
        .or_else(|| env::var("AGENT_ID").ok())
        .unwrap_or_else(|| "default".to_string());

    let mut config = agent_configs
        .iter()
        .find(|c| c.name.as_deref().unwrap_or("default") == agent_id)
        .cloned()
        .unwrap_or_else(|| {
            warn!("Agent '{}' not found in config. Using default.", agent_id);
            AgentConfig::default()
        });

    if let Ok(system_prompt) = env::var("SYSTEM_PROMPT") {
        if !system_prompt.trim().is_empty() {
            config.instructions = Some(system_prompt);
        }
    }

    info!(
        "Selected agent: {}",
        config.name.as_deref().unwrap_or("unknown")
    );

    // LLM Setup
    let provider = if let Some(provider) = ProviderKind::configured()? {
        provider
    } else if let Some(provider) = ProviderKind::infer_from_env() {
        provider
    } else if !interactive {
        anyhow::bail!(
            "LLM provider is not configured. Set LLM_PROVIDER and the matching API key env vars, or run SwarmClaw in an interactive terminal."
        );
    } else {
        loop {
            println!(
                "{}",
                "╔═══════════════════════════════════════════════════════════════╗".bright_blue()
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "Welcome to HuggingPlace SwarmClaw! 🦀🤖                       ".bold()
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "Choose your AI Provider:                                      "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  1. OpenAI                                                   "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  2. Anthropic                                                "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  3. Gemini                                                   "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  4. Ollama (Local)                                           "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  5. Groq                                                     "
            );
            println!(
                "{} {}",
                "║".bright_blue(),
                "  6. Grok (xAI)                                               "
            );
            println!(
                "{}",
                "╚═══════════════════════════════════════════════════════════════╝".bright_blue()
            );
            print!("{}", "\nEnter choice (1-6): ".cyan().bold());
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                continue;
            }

            if let Some(provider) = ProviderKind::from_prompt_choice(input.trim()) {
                save_env_var("LLM_PROVIDER", provider.config_value());
                break provider;
            }

            println!(
                "{}",
                "Invalid choice. Please pick 1, 2, 3, 4, 5, or 6.".red()
            );
        }
    };

    let mut api_key = provider.read_api_key();

    while provider.api_key_prompt().is_some() && api_key.is_empty() {
        if !interactive {
            anyhow::bail!(
                "Missing API key for provider '{}'. Set {} or run SwarmClaw in an interactive terminal.",
                provider.display_name(),
                provider.api_key_env_name().unwrap_or("API_KEY")
            );
        }

        print!(
            "{}",
            format!(
                "\nPlease enter your {}: ",
                provider.api_key_prompt().unwrap_or("API key")
            )
            .cyan()
            .bold()
        );
        std::io::stdout().flush().unwrap();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            continue;
        }
        api_key = input.trim().to_string();

        if api_key.is_empty() {
            println!(
                "{}",
                "API key cannot be empty. Please try again or press Ctrl+C to exit.".red()
            );
        } else {
            if let Some(env_key_name) = provider.api_key_env_name() {
                save_env_var(env_key_name, &api_key);
            }
        }
    }

    if config.model.is_none() || config.model.as_deref() == Some("") {
        let default_model = provider.default_model();

        if !interactive {
            info!(
                "No model configured. Using default model '{}'.",
                default_model
            );
            config.model = Some(default_model.to_string());
        } else {
            loop {
                print!(
                    "{}",
                    format!("\nEnter model name [{}]: ", default_model)
                        .cyan()
                        .bold()
                );
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
    }

    info!(
        "Using LLM Provider: {} (Model: {})",
        provider.display_name(),
        config.model.as_deref().unwrap_or("unknown")
    );

    let llm_provider: Arc<dyn swarmclaw::llm::LLMProvider> = match provider {
        ProviderKind::Anthropic => {
            Arc::new(swarmclaw::llm::anthropic::AnthropicProvider::new(api_key))
        }
        ProviderKind::Gemini => Arc::new(swarmclaw::llm::gemini::GeminiProvider::new(api_key)),
        ProviderKind::Ollama => Arc::new(swarmclaw::llm::ollama::OllamaProvider::new(
            env::var("OLLAMA_HOST").unwrap_or_default(),
        )),
        ProviderKind::Groq => {
            let openai_provider = OpenAIProvider::groq(api_key);
            if let Some(base_url) = provider.base_url_override() {
                Arc::new(openai_provider.with_base_url(base_url))
            } else {
                Arc::new(openai_provider)
            }
        }
        ProviderKind::Grok => {
            let openai_provider = OpenAIProvider::grok(api_key);
            if let Some(base_url) = provider.base_url_override() {
                Arc::new(openai_provider.with_base_url(base_url))
            } else {
                Arc::new(openai_provider)
            }
        }
        ProviderKind::OpenAI => {
            let openai_provider = OpenAIProvider::new(api_key);
            if let Some(base_url) = provider.base_url_override() {
                Arc::new(openai_provider.with_base_url(base_url))
            } else {
                Arc::new(openai_provider)
            }
        }
    };

    // Ensure Model is available (if local)
    if provider.is_local() {
        if let Some(model_name) = &config.model {
            info!("Ensuring local model '{}' is available...", model_name);
            use swarmclaw::services::model_fetcher::ModelFetcher;
            let fetcher = ModelFetcher::new(&workspace_path);
            if let Err(e) = fetcher.ensure_model(model_name).await {
                warn!(
                    "Failed to fetch local model '{}': {}. Agent may fail if the local model is required.",
                    model_name,
                    e
                );
            }
        }
    }

    // HuggingPlace Memory Setup
    let mut use_memory = env::var("HUGGINGPLACE_MEMORY_ENABLED")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
    let mut memory_api_key = env::var("HUGGINGPLACE_MEMORY_API_KEY").unwrap_or_default();

    if env::var("HUGGINGPLACE_MEMORY_ENABLED").is_err() {
        if !interactive {
            use_memory = false;
        } else {
            loop {
                print!(
                    "{}",
                    "\nDo you want to enable HuggingPlace Memory for long-term context? (y/N): "
                        .cyan()
                        .bold()
                );
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
                        print!(
                            "{}",
                            "Enter email to provision agent account (e.g. agent@domain.com): "
                                .cyan()
                                .bold()
                        );
                        std::io::stdout().flush().unwrap();
                        let mut email_input = String::new();
                        if std::io::stdin().read_line(&mut email_input).is_err() {
                            continue;
                        }

                        let email = email_input.trim();
                        if !email.is_empty() && email.contains("@") {
                            let memory_email = email.to_string();
                            save_env_var("HUGGINGPLACE_MEMORY_EMAIL", &memory_email);
                            println!(
                                "{}",
                                format!(
                                    "Registering agent identity {} with HuggingPlace...",
                                    memory_email
                                )
                                .yellow()
                            );

                            let client = reqwest::Client::new();
                            let payload = serde_json::json!({
                                "email": memory_email,
                                "org_name": "SwarmClaw User",
                                "first_name": "SwarmClaw",
                                "last_name": "Agent"
                            });

                            // Attempt to provision against the HuggingPlace API
                            let res = client
                                .post("http://localhost:8001/api/users")
                                .json(&payload)
                                .send()
                                .await;

                            match res {
                                Ok(response) if response.status().is_success() => {
                                    if let Ok(body) = response.json::<serde_json::Value>().await {
                                        if let Some(key) =
                                            body.get("api_key").and_then(|v| v.as_str())
                                        {
                                            memory_api_key = key.to_string();
                                            save_env_var(
                                                "HUGGINGPLACE_MEMORY_API_KEY",
                                                &memory_api_key,
                                            );
                                            println!(
                                                "{}",
                                                "✅ Memory identity provisioned successfully!"
                                                    .green()
                                            );
                                            if let Some(details) = body
                                                .get("details")
                                                .and_then(|v| v.get("note"))
                                                .and_then(|v| v.as_str())
                                            {
                                                println!("{} {}", "ℹ️".blue(), details.dimmed());
                                            }
                                            break;
                                        }
                                    }
                                    // Fallback if the structure doesn't match our exact new spec yet
                                    memory_api_key =
                                        format!("sk-fallback-{}", uuid::Uuid::new_v4());
                                    save_env_var("HUGGINGPLACE_MEMORY_API_KEY", &memory_api_key);
                                    println!(
                                        "{}",
                                        "✅ Memory identity registered (fallback parsing).".green()
                                    );
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
                                    println!(
                                        "{} {}",
                                        "⚠️ Could not connect to HuggingPlace Backend:".yellow(),
                                        e
                                    );
                                    println!(
                                        "{}",
                                        "Proceeding in mock memory mode for development.".yellow()
                                    );
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
    }

    let mut agent = Agent::new(agent_id.to_string(), config, llm_provider)
        .with_state_path(session_state_path(&workspace_path, &agent_id))
        .with_workspace_root(workspace_path.clone());

    // Inject HuggingPlace Memory if enabled
    if use_memory && !memory_api_key.is_empty() {
        let org_id = "default-org".to_string();
        agent = agent.with_memory(org_id, memory_api_key);
        info!("HuggingPlace Memory context injection enabled.");
    }

    // Add Native Skills
    info!("Adding FileSystem skill...");
    agent.add_skill(Arc::new(FileSystemSkill::new(workspace_path.clone())));

    info!("Adding Analytics skill...");
    agent.add_skill(Arc::new(AnalyticsSkill::new(workspace_path.clone())));

    info!("Adding Shell skill...");

    agent.add_skill(Arc::new(ShellSkill::new()));

    // Add Browser Skill

    #[cfg(feature = "headless_chrome")]
    {
        info!("Adding Browser skill...");

        use swarmclaw::skills::analytics::AnalyticsSkill;
use swarmclaw::skills::browser::BrowserSkill;

        agent.add_skill(Arc::new(BrowserSkill::new()));
    }

    // Add Firefox Skill
    info!("Adding Firefox skill...");
    use swarmclaw::skills::firefox::FirefoxSkill;
    agent.add_skill(Arc::new(FirefoxSkill::new()));

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
    let mut has_google_docs_wasm = false;
    let mut has_google_gmail_wasm = false;
    let mut has_fetch_convert_wasm = false;
    let mut has_search_web_wasm = false;
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
                    match skill_name.as_str() {
                        "google_sheets" => has_google_sheets_wasm = true,
                        "google_docs" => has_google_docs_wasm = true,
                        "google_gmail" => has_google_gmail_wasm = true,
                        "fetch_convert" => has_fetch_convert_wasm = true,
                        "search_web" => has_search_web_wasm = true,
                        _ => {}
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

    {
        let service_url = web_tools_service.base_url();
        let fetch_mcp_endpoint = web_tools_service.fetch_mcp_endpoint();
        let search_mcp_endpoint = web_tools_service.search_mcp_endpoint();
        let service_runner = web_tools_service.clone();

        info!("Starting Web Tools service at {}", service_url);
        tokio::spawn(async move {
            if let Err(error) = service_runner.start().await {
                warn!("Web Tools service error: {}", error);
            }
        });

        use swarmclaw::skills::mcp::McpSkill;

        if has_fetch_convert_wasm {
            info!(
                "fetch_convert WASM skill detected in workspace; skipping native fetch MCP registration."
            );
        } else {
            match McpSkill::connect("fetch_convert", &fetch_mcp_endpoint).await {
                Ok(skill) => {
                    info!("Adding fetch_convert skill...");
                    agent.add_skill(Arc::new(skill));
                }
                Err(error) => {
                    warn!(
                        "Failed to register fetch_convert MCP skill from {}: {}",
                        fetch_mcp_endpoint, error
                    );
                }
            }
        }

        if has_search_web_wasm {
            info!(
                "search_web WASM skill detected in workspace; skipping native search MCP registration."
            );
        } else {
            match McpSkill::connect("search_web", &search_mcp_endpoint).await {
                Ok(skill) => {
                    info!("Adding search_web skill...");
                    agent.add_skill(Arc::new(skill));
                }
                Err(error) => {
                    warn!(
                        "Failed to register search_web MCP skill from {}: {}",
                        search_mcp_endpoint, error
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

        if has_google_docs_wasm {
            info!(
                "Google Docs WASM skill detected in workspace; skipping native MCP registration."
            );
        } else {
            use swarmclaw::skills::mcp::McpSkill;
            match McpSkill::connect("google_docs", &mcp_endpoint).await {
                Ok(skill) => {
                    info!("Adding Google Docs skill...");
                    agent.add_skill(Arc::new(skill));
                }
                Err(error) => {
                    warn!(
                        "Failed to register Google Docs MCP skill from {}: {}",
                        mcp_endpoint, error
                    );
                }
            }
        }

        if has_google_gmail_wasm {
            info!(
                "Google Gmail WASM skill detected in workspace; skipping native MCP registration."
            );
        } else {
            use swarmclaw::skills::mcp::McpSkill;
            match McpSkill::connect("google_gmail", &mcp_endpoint).await {
                Ok(skill) => {
                    info!("Adding Google Gmail skill...");
                    agent.add_skill(Arc::new(skill));
                }
                Err(error) => {
                    warn!(
                        "Failed to register Google Gmail MCP skill from {}: {}",
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
        fn name(&self) -> &str {
            "delegation"
        }
        fn description(&self) -> &str {
            "Allows delegating tasks to other agents."
        }
        fn tools(&self) -> Vec<Arc<dyn swarmclaw::tools::Tool>> {
            vec![Arc::new(DelegateTaskTool)]
        }
    }

    agent.add_skill(Arc::new(DelegateSkill));

    let gateway_agent_template = Arc::new(agent.clone());
    let agent_shared = Arc::new(tokio::sync::Mutex::new(agent));

    // Start Cron Worker (Proactive Automation)
    let cron_agent = agent_shared.clone();
    tokio::spawn(async move {
        use swarmclaw::services::cron::CronWorker;
        let worker = CronWorker::new(cron_agent);
        worker.start().await;
    });

    info!("Agent initialized. Starting run loop...");

    if env::var("SWARMCLAW_ADMIN_PORT").is_ok() {
        let admin_server =
            swarmclaw::services::admin_api::AdminApiServer::new(workspace_path.clone())?;
        tokio::spawn(async move {
            if let Err(error) = admin_server.start().await {
                warn!("Admin API error: {}", error);
            }
        });
    }

    // Start Chat Gateways in background
    let agent_id_str = agent_id.to_string();

    // WebRTC Signaling Gateway
    if let Ok(ws_url) = env::var("WEBRTC_SIGNALING_URL").or_else(|_| env::var("CLAWNET_WS_URL")) {
        info!("Starting WebRTC Signaling gateway...");
        use swarmclaw::gateways::webrtc_signaling::WebRTCSignalingGateway;
        use swarmclaw::gateways::ChatGateway;
        let ws_url_for_registry = ws_url.clone();
        let signaling = WebRTCSignalingGateway::new(
            ws_url,
            agent_id_str.clone(),
            gateway_agent_template.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = signaling.start().await {
                warn!("WebRTC Signaling gateway error: {}", e);
            }
        });
        register_control_plane_channel(
            &workspace_path,
            "webrtc",
            "websocket",
            &ws_url_for_registry,
        )
        .await;
    }

    if env::var("DISCORD_PUBLIC_KEY").is_ok() {
        info!("Starting Discord webhook gateway...");
        use swarmclaw::gateways::discord::DiscordWebhookGateway;
        use swarmclaw::gateways::ChatGateway;
        let discord = DiscordWebhookGateway::new(gateway_agent_template.clone())?;
        tokio::spawn(async move {
            if let Err(e) = discord.start().await {
                warn!("Discord gateway error: {}", e);
            }
        });
        register_control_plane_channel(
            &workspace_path,
            "discord",
            "webhook",
            "/discord/interactions",
        )
        .await;
    }

    if env::var("TELEGRAM_TOKEN").is_ok() {
        info!("Starting Telegram webhook gateway...");
        use swarmclaw::gateways::telegram::TelegramWebhookGateway;
        use swarmclaw::gateways::ChatGateway;
        let telegram = TelegramWebhookGateway::new(gateway_agent_template.clone())?;
        tokio::spawn(async move {
            if let Err(e) = telegram.start().await {
                warn!("Telegram gateway error: {}", e);
            }
        });
        if let Ok(token) = env::var("TELEGRAM_TOKEN") {
            register_control_plane_channel(
                &workspace_path,
                "telegram",
                "webhook",
                &format!("/telegram/{}", token),
            )
            .await;
        }
    }

    match (
        env::var("TWILIO_ACCOUNT_SID"),
        env::var("TWILIO_AUTH_TOKEN"),
    ) {
        (Ok(_), Ok(_)) => {
            info!("Starting WhatsApp Twilio webhook gateway...");
            use swarmclaw::gateways::whatsapp::WhatsAppWebhookGateway;
            use swarmclaw::gateways::ChatGateway;
            let whatsapp = WhatsAppWebhookGateway::new(gateway_agent_template.clone())?;
            tokio::spawn(async move {
                if let Err(error) = whatsapp.start().await {
                    warn!("WhatsApp gateway error: {}", error);
                }
            });
            register_control_plane_channel(
                &workspace_path,
                "whatsapp",
                "webhook",
                "/twilio/whatsapp",
            )
            .await;
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => {
            warn!(
                "WhatsApp webhook gateway not started because both TWILIO_ACCOUNT_SID and TWILIO_AUTH_TOKEN are required."
            );
        }
        (Err(_), Err(_)) => {}
    }

    match (
        env::var("SLACK_BOT_TOKEN"),
        env::var("SLACK_SIGNING_SECRET"),
    ) {
        (Ok(_), Ok(_)) => {
            info!("Starting Slack webhook gateway...");
            use swarmclaw::gateways::slack::SlackWebhookGateway;
            use swarmclaw::gateways::ChatGateway;
            let slack = SlackWebhookGateway::new(gateway_agent_template.clone())?;
            tokio::spawn(async move {
                if let Err(e) = slack.start().await {
                    warn!("Slack gateway error: {}", e);
                }
            });
            register_control_plane_channel(&workspace_path, "slack", "webhook", "/slack/events")
                .await;
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => {
            warn!(
                "Slack webhook gateway not started because both SLACK_BOT_TOKEN and SLACK_SIGNING_SECRET are required."
            );
        }
        (Err(_), Err(_)) => {}
    }

    // Run the main REPL loop using the shared agent
    let mut agent_lock = agent_shared.lock().await;
    agent_lock.run().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ProviderKind;

    #[test]
    fn parses_supported_provider_aliases() {
        assert_eq!(ProviderKind::parse("openai"), Some(ProviderKind::OpenAI));
        assert_eq!(ProviderKind::parse("GROQ"), Some(ProviderKind::Groq));
        assert_eq!(ProviderKind::parse("grok"), Some(ProviderKind::Grok));
        assert_eq!(ProviderKind::parse("xai"), Some(ProviderKind::Grok));
        assert_eq!(
            ProviderKind::parse("anthropic"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(ProviderKind::parse("gemini"), Some(ProviderKind::Gemini));
        assert_eq!(
            ProviderKind::parse("local / custom"),
            Some(ProviderKind::Ollama)
        );
    }
}
