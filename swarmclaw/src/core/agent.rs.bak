use crate::config::AgentConfig;
use crate::core::session_store::{
    derive_store_path, load_session_state, persist_message, persist_seed_state,
};
use crate::core::state::{Message, Role, State};
use crate::llm::{ChatChunk, CompletionOptions, LLMProvider, ProviderCapabilities};
use crate::outbox::enqueue_gateway_text_message;
use crate::safety::SafetyLayer;
use crate::security::Redactor;
use crate::skills::Skill;
use crate::worker::WorkerPool;
use colored::Colorize;
use crossterm::{
    cursor::{MoveTo, MoveToColumn},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen, SetTitle,
    },
};
use futures::StreamExt;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, info_span, warn, Instrument};
use uuid::Uuid;

const CLI_BG: Color = Color::Rgb {
    r: 23,
    g: 24,
    b: 27,
};
const CLI_FG: Color = Color::Rgb {
    r: 231,
    g: 233,
    b: 238,
};
const CLI_BG_RGB: (u8, u8, u8) = (23, 24, 27);
const CLI_FG_RGB: (u8, u8, u8) = (231, 233, 238);
const CLI_PANEL_RGB: (u8, u8, u8) = (34, 37, 42);
const CLI_DEEP_RGB: (u8, u8, u8) = (13, 14, 16);
const CLI_BORDER_RGB: (u8, u8, u8) = (52, 56, 65);
const CLI_MUTED_RGB: (u8, u8, u8) = (154, 163, 173);
const CLI_CYAN_RGB: (u8, u8, u8) = (138, 162, 211);
const CLI_MAGENTA_RGB: (u8, u8, u8) = (96, 117, 158);
const CLI_AMBER_RGB: (u8, u8, u8) = (198, 160, 93);
const CLI_GREEN_RGB: (u8, u8, u8) = (118, 181, 132);
const CLI_RED_RGB: (u8, u8, u8) = (203, 108, 108);
const CLI_RESULT_RGB: (u8, u8, u8) = (45, 48, 54);
const CLI_LOGO_OUTLINE_RGB: (u8, u8, u8) = (76, 86, 106);
const CLI_LOGO_WING_RGB: (u8, u8, u8) = (255, 255, 255);
const CLI_LOGO_WING_SHADE_RGB: (u8, u8, u8) = (236, 239, 244);
const CLI_LOGO_CORAL_RGB: (u8, u8, u8) = (255, 136, 130);
const CLI_LOGO_BLUSH_RGB: (u8, u8, u8) = (255, 107, 107);
const CLI_LOGO_FACE_RGB: (u8, u8, u8) = (46, 52, 64);

struct TerminalUiGuard;

#[derive(Clone, Debug)]
pub struct ChannelInfo {
    pub platform: String,
    pub channel_id: String,
    pub token: String,
    pub app_id: Option<String>,
    pub delivery_context: Option<serde_json::Value>,
}

impl ChannelInfo {
    pub fn new(
        platform: impl Into<String>,
        channel_id: impl Into<String>,
        token: impl Into<String>,
        app_id: Option<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            channel_id: channel_id.into(),
            token: token.into(),
            app_id,
            delivery_context: None,
        }
    }

    pub fn with_delivery_context(mut self, delivery_context: serde_json::Value) -> Self {
        self.delivery_context = Some(delivery_context);
        self
    }
}

enum TurnMode {
    Streaming,
    NonStreaming,
}

struct TurnPreparation {
    history: Vec<Message>,
    tools: Vec<Arc<dyn crate::tools::Tool>>,
    disabled_tool_notice: Option<String>,
}

impl TerminalUiGuard {
    fn enter(stdout: &mut io::Stdout) -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, SetTitle("SwarmClaw CLI"))?;
        apply_cli_terminal_theme(stdout)?;
        execute!(
            stdout,
            SetBackgroundColor(CLI_BG),
            SetForegroundColor(CLI_FG),
            Clear(ClearType::All),
            MoveTo(0, 0),
        )?;
        paint_cli_viewport(stdout)?;
        execute!(stdout, MoveTo(0, 0))?;
        Ok(Self)
    }
}

impl Drop for TerminalUiGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        let _ = reset_cli_terminal_theme(&mut stdout);
        let _ = execute!(stdout, ResetColor, LeaveAlternateScreen);
    }
}

#[derive(Clone)]
pub struct Agent {
    pub id: String,
    pub config: AgentConfig,
    pub state: State,
    state_path: Option<PathBuf>,
    state_store_path: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
    pub llm: Arc<dyn LLMProvider>,
    pub skills: Vec<Arc<dyn Skill>>,
    pub memory_org_id: Option<String>,
    pub memory_api_key: Option<String>,
}

impl Agent {
    pub fn new(id: String, config: AgentConfig, llm: Arc<dyn LLMProvider>) -> Self {
        let state = seeded_state(&config);

        Self {
            id,
            config,
            state,
            state_path: None,
            state_store_path: None,
            workspace_root: None,
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

    pub fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = Some(workspace_root);
        self
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    pub fn spawn_session(&self, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        let mut agent = Self::new(session_id.clone(), self.config.clone(), self.llm.clone());
        agent.skills = self.skills.clone();
        agent.memory_org_id = self.memory_org_id.clone();
        agent.memory_api_key = self.memory_api_key.clone();
        agent.workspace_root = self.workspace_root.clone();

        if let Some(state_path) = self.session_state_path(&session_id) {
            agent = agent.with_state_path(state_path);
        }

        agent
    }

    pub fn with_state_path(mut self, state_path: PathBuf) -> Self {
        let store_path = derive_store_path(&state_path);
        let loaded_state = match load_session_state(&store_path, &self.id, &state_path) {
            Ok(Some(state)) => {
                self.state = state;
                true
            }
            Ok(None) => false,
            Err(error) => {
                warn!(
                    agent_id = %self.id,
                    state_path = %state_path.display(),
                    state_store_path = %store_path.display(),
                    "Failed to restore session history: {error}"
                );
                false
            }
        };

        self.state_path = Some(state_path);
        self.state_store_path = Some(store_path);

        if !loaded_state && !self.state.history.is_empty() {
            self.persist_seed_state_best_effort();
        }

        self
    }

    pub fn record_message(&mut self, message: Message) {
        self.state.history.push(message);
        self.persist_state_best_effort();
    }

    fn persist_state_best_effort(&self) {
        let Some(store_path) = &self.state_store_path else {
            return;
        };
        let Some(message) = self.state.history.last() else {
            return;
        };
        let message_index = self.state.history.len().saturating_sub(1);

        if let Err(error) = persist_message(store_path, &self.id, message_index, message) {
            warn!(
                agent_id = %self.id,
                state_store_path = %store_path.display(),
                message_index,
                "Failed to persist session message: {error}"
            );
        }
    }

    fn persist_seed_state_best_effort(&self) {
        let Some(store_path) = &self.state_store_path else {
            return;
        };

        if let Err(error) = persist_seed_state(store_path, &self.id, &self.state) {
            warn!(
                agent_id = %self.id,
                state_store_path = %store_path.display(),
                "Failed to persist seeded session state: {error}"
            );
        }
    }

    fn session_state_path(&self, session_id: &str) -> Option<PathBuf> {
        let parent = self.state_path.as_ref()?.parent()?.to_path_buf();
        Some(parent.join(format!("{}.json", sanitize_session_id(session_id))))
    }

    fn redraw_cli_screen(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        execute!(
            stdout,
            SetBackgroundColor(CLI_BG),
            SetForegroundColor(CLI_FG),
            Clear(ClearType::All),
            MoveTo(0, 0),
        )?;
        paint_cli_viewport(stdout)?;
        execute!(stdout, MoveTo(0, 0))?;

        render_cli_intro(
            stdout,
            &self.id,
            self.llm.provider_name(),
            self.config.model.as_deref().unwrap_or("auto"),
            self.skills.len(),
            self.llm.capabilities(),
        )?;

        if let Some(notice) = session_capability_notice(
            self.llm.provider_name(),
            self.llm.capabilities(),
            self.skills.len(),
        ) {
            write_cli_line(
                stdout,
                format!(
                    "{} {}",
                    cli_chip("LIMITED", CLI_DEEP_RGB, CLI_AMBER_RGB),
                    notice.truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2),
                ),
            )?;
            write_cli_line(stdout, "")?;
        }

        render_cli_history(stdout, &self.state.history)
    }

    fn read_cli_input(
        &self,
        stdout: &mut io::Stdout,
        label: &str,
        chip_fg: (u8, u8, u8),
        chip_bg: (u8, u8, u8),
        history: &mut Vec<String>,
        record_history: bool,
    ) -> io::Result<Option<String>> {
        let mut input = String::new();
        let mut history_index: Option<usize> = None;
        self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;

        loop {
            match event::read()? {
                Event::Resize(_, _) => {
                    self.redraw_cli_screen(stdout)?;
                    self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        write_cli_line(stdout, "")?;
                        return Ok(None);
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        write_cli_line(stdout, "")?;
                        return Ok(None);
                    }
                    KeyCode::Enter => {
                        if input.trim().is_empty() { return Ok(Some(input)); }
                        write_cli_line(stdout, "")?;
                        if record_history && !input.trim().is_empty() {
                            history.push(input.clone());
                        }
                        return Ok(Some(input));
                    }
                    KeyCode::Backspace => {
                        input.pop();
                        history_index = None;
                        self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                    }
                    KeyCode::Esc => {
                        input.clear();
                        history_index = None;
                        self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                    }
                    KeyCode::Up if !history.is_empty() => {
                        history_index = Some(match history_index {
                            Some(index) if index > 0 => index - 1,
                            Some(index) => index,
                            None => history.len().saturating_sub(1),
                        });
                        if let Some(index) = history_index {
                            input = history[index].clone();
                            self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                        }
                    }
                    KeyCode::Down if !history.is_empty() => {
                        if let Some(index) = history_index {
                            if index + 1 < history.len() {
                                history_index = Some(index + 1);
                                input = history[index + 1].clone();
                            } else {
                                history_index = None;
                                input.clear();
                            }
                            self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                        }
                    }
                    KeyCode::Char(ch) => {
                        input.push(ch);
                        history_index = None;
                        self.render_input_prompt(stdout, label, chip_fg, chip_bg, &input)?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn render_input_prompt(
        &self,
        stdout: &mut io::Stdout,
        label: &str,
        chip_fg: (u8, u8, u8),
        chip_bg: (u8, u8, u8),
        input: &str,
    ) -> io::Result<()> {
        let prompt_width = label.chars().count() + 3;
        let available = terminal_width()
            .saturating_sub(prompt_width)
            .saturating_sub(1);
        let visible_input = fit_tail_text(input, available);

        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        write!(
            stdout,
            "{} {}",
            cli_chip(label, chip_fg, chip_bg),
            visible_input.truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2),
        )?;
        apply_cli_palette(stdout)?;
        stdout.flush()
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut stdout = io::stdout();
        {
            let _ui = TerminalUiGuard::enter(&mut stdout)?;
            self.redraw_cli_screen(&mut stdout)?;
            let mut input_history = Vec::new();

            loop {
                let Some(input) = self.read_cli_input(
                    &mut stdout,
                    "USER",
                    CLI_DEEP_RGB,
                    CLI_CYAN_RGB,
                    &mut input_history,
                    true,
                )?
                else {
                    break;
                };

                let input = input.trim();
                if input.is_empty() {
                    continue;
                }

                if input.starts_with("/key") {
                    let parts: Vec<&str> = input.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        let new_key = parts[1].trim().to_string();
                        self.llm.update_api_key(new_key.clone());
                        let provider_name = self.llm.provider_name();
                        let env_key = match provider_name.to_lowercase().as_str() {
                            "openai" => "OPENAI_API_KEY",
                            "anthropic" => "ANTHROPIC_API_KEY",
                            "gemini" => "GEMINI_API_KEY",
                            "groq" => "GROQ_API_KEY",
                            "grok" | "xai" => "XAI_API_KEY",
                            _ => "API_KEY"
                        };
                        if let Ok(contents) = std::fs::read_to_string(".env") {
                            let lines: Vec<String> = contents.lines().filter(|line| !line.starts_with(env_key)).map(String::from).collect();
                            let new_contents = format!("{}\n{}={}\n", lines.join("\n"), env_key, new_key);
                            let _ = std::fs::write(".env", new_contents);
                        } else {
                            let _ = std::fs::write(".env", format!("{}={}\n", env_key, new_key));
                        }
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_GREEN_RGB), "API key updated successfully.".truecolor(CLI_GREEN_RGB.0, CLI_GREEN_RGB.1, CLI_GREEN_RGB.2)))?;
                    } else {
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_AMBER_RGB), "Usage: /key <your-api-key>".truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2)))?;
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("INFO", CLI_DEEP_RGB, CLI_CYAN_RGB), format!("Current Provider: {}", self.llm.provider_name()).truecolor(CLI_CYAN_RGB.0, CLI_CYAN_RGB.1, CLI_CYAN_RGB.2)))?;
                    }
                    continue;
                }

                if input.starts_with("/model") {
                    let parts: Vec<&str> = input.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        let new_model = parts[1].trim().to_string();
                        self.config.model = Some(new_model.clone());
                        if new_model.starts_with("gemini") { self.llm = std::sync::Arc::new(crate::llm::gemini::GeminiProvider::new(std::env::var("GEMINI_API_KEY").unwrap_or_default())); }
                        else if new_model.starts_with("gpt-") || new_model.starts_with("o1-") || new_model.starts_with("o3-") { self.llm = std::sync::Arc::new(crate::llm::openai::OpenAIProvider::new(std::env::var("OPENAI_API_KEY").unwrap_or_default())); }
                        else if new_model.starts_with("claude-") { self.llm = std::sync::Arc::new(crate::llm::anthropic::AnthropicProvider::new(std::env::var("ANTHROPIC_API_KEY").unwrap_or_default())); }
                        else if new_model.starts_with("grok-") { self.llm = std::sync::Arc::new(crate::llm::openai::OpenAIProvider::grok(std::env::var("XAI_API_KEY").or_else(|_| std::env::var("GROK_API_KEY")).unwrap_or_default())); }
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_GREEN_RGB), format!("Model updated to {} (Provider auto-detected: {})", new_model, self.llm.provider_name()).truecolor(CLI_GREEN_RGB.0, CLI_GREEN_RGB.1, CLI_GREEN_RGB.2)))?;
                    } else {
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_AMBER_RGB), "Usage: /model <model-name>".truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2)))?;
                        let current = self.config.model.as_deref().unwrap_or("default");
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("INFO", CLI_DEEP_RGB, CLI_CYAN_RGB), format!("Current Model: {}\nExamples: gemini-3.1-pro-preview, claude-3-5-sonnet-latest, gpt-4o, o3-mini", current).truecolor(CLI_CYAN_RGB.0, CLI_CYAN_RGB.1, CLI_CYAN_RGB.2)))?;
                    }
                    continue;
                }

                if input.starts_with("/provider") {
                    let parts: Vec<&str> = input.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        let new_provider = parts[1].trim().to_lowercase();
                        let result = match new_provider.as_str() {
                            "openai" => {
                                let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::openai::OpenAIProvider::new(key));
                                Ok("Switched to OpenAI")
                            }
                            "anthropic" | "claude" => {
                                let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::anthropic::AnthropicProvider::new(key));
                                Ok("Switched to Anthropic")
                            }
                            "gemini" | "google" => {
                                let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::gemini::GeminiProvider::new(key));
                                Ok("Switched to Gemini")
                            }
                            "groq" => {
                                let key = std::env::var("GROQ_API_KEY").unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::openai::OpenAIProvider::groq(key));
                                Ok("Switched to Groq")
                            }
                            "grok" | "xai" => {
                                let key = std::env::var("XAI_API_KEY").or_else(|_| std::env::var("GROK_API_KEY")).unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::openai::OpenAIProvider::grok(key));
                                Ok("Switched to Grok")
                            }
                            "ollama" | "local" => {
                                let host = std::env::var("OLLAMA_HOST").unwrap_or_default();
                                self.llm = std::sync::Arc::new(crate::llm::ollama::OllamaProvider::new(host));
                                Ok("Switched to Ollama")
                            }
                            _ => Err("Unknown provider. Valid options: openai, anthropic, gemini, groq, grok, ollama")
                        };
                        match result {
                            Ok(msg) => write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_GREEN_RGB), msg.truecolor(CLI_GREEN_RGB.0, CLI_GREEN_RGB.1, CLI_GREEN_RGB.2)))?,
                            Err(msg) => write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_AMBER_RGB), msg.truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2)))?,
                        }
                    } else {
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("SYSTEM", CLI_DEEP_RGB, CLI_AMBER_RGB), "Usage: /provider <provider-name>".truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2)))?;
                        write_cli_line(&mut stdout, format!("{} {}", cli_chip("INFO", CLI_DEEP_RGB, CLI_CYAN_RGB), format!("Current Provider: {}\nAvailable: gemini, anthropic, openai, groq, grok, ollama", self.llm.provider_name()).truecolor(CLI_CYAN_RGB.0, CLI_CYAN_RGB.1, CLI_CYAN_RGB.2)))?;
                    }
                    continue;
                }

                if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                    break;
                }

                let turn_span = cli_turn_span(&self.id, self.llm.provider_name(), input.len());
                let safe_input = match SafetyLayer::scrub_prompt(input) {
                    Ok(safe) => safe,
                    Err(e) => {
                        {
                            let _guard = turn_span.enter();
                            warn!(error = %e, "CLI turn blocked by safety");
                        }
                        write_cli_line(
                            &mut stdout,
                            format!(
                                "{} {}",
                                cli_chip("BLOCKED", CLI_DEEP_RGB, CLI_RED_RGB),
                                e.to_string().truecolor(
                                    CLI_RED_RGB.0,
                                    CLI_RED_RGB.1,
                                    CLI_RED_RGB.2
                                ),
                            ),
                        )?;
                        continue;
                    }
                };

                let redacted_input = Redactor::redact(&safe_input);
                let timestamp = now_secs();

                {
                    let _guard = turn_span.enter();
                    self.record_message(Message {
                        role: Role::User,
                        content: redacted_input,
                        timestamp,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    info!("CLI turn started");
                }
                write_cli_line(&mut stdout, "")?;
                let mut turn_failed = false;
                loop {
                    let mut respond_fut = Box::pin(self.respond(None).instrument(turn_span.clone()));
                    let mut cancel_stream = crossterm::event::EventStream::new();
                    let res = loop {
                        tokio::select! {
                            r = &mut respond_fut => break r,
                            Some(Ok(event)) = cancel_stream.next() => {
                                if let crossterm::event::Event::Key(key) = event {
                                    if key.code == crossterm::event::KeyCode::Esc || (key.code == crossterm::event::KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)) {
                                        break Err(anyhow::anyhow!("[Operation Cancelled] User cancelled tool execution."));
                                    }
                                }
                            }
                        }
                    };

                    drop(respond_fut);
                    drop(cancel_stream);

                    match res {
                        Ok(_) => break,
                        Err(e) => {
                            if self.llm.is_auth_error(&e) {
                            write_cli_line(
                                &mut stdout,
                                format!(
                                    "{} {}",
                                    cli_chip("AUTH", CLI_DEEP_RGB, CLI_RED_RGB),
                                    format!("Provider rejected the request: {e}")
                                        .truecolor(CLI_RED_RGB.0, CLI_RED_RGB.1, CLI_RED_RGB.2)
                                        .bold(),
                                ),
                            )?;
    
                            let mut auth_history = Vec::new();
                            if let Some(key) = self.read_cli_input(
                                &mut stdout,
                                "NEW KEY",
                                CLI_DEEP_RGB,
                                CLI_AMBER_RGB,
                                &mut auth_history,
                                false,
                            )? {
                                let key = key.trim().to_string();
                                if !key.is_empty() {
                                    self.llm.update_api_key(key);
                                    write_cli_line(
                                        &mut stdout,
                                        format!(
                                            "{} {}",
                                            cli_chip("UPDATED", CLI_DEEP_RGB, CLI_GREEN_RGB),
                                            "API key refreshed. Retrying request...".truecolor(
                                                CLI_GREEN_RGB.0,
                                                CLI_GREEN_RGB.1,
                                                CLI_GREEN_RGB.2
                                            ),
                                        ),
                                    )?;
                                    continue;
                                }
                            }
                        }
    
                            turn_failed = true;
                        {
                            let _guard = turn_span.enter();
                            warn!(error = %e, "CLI turn failed");
                        }
    
                        write_cli_line(
                            &mut stdout,
                            format!(
                                "{} {}",
                                cli_chip("ERROR", CLI_DEEP_RGB, CLI_RED_RGB),
                                format!("{e}").truecolor(CLI_RED_RGB.0, CLI_RED_RGB.1, CLI_RED_RGB.2),
                            ),
                        )?;
                            break;
                        }
                    }
                }

                if !turn_failed {
                    let _guard = turn_span.enter();
                    info!("CLI turn completed");
                }
            }
        }

        println!(
            "{} {}",
            "SwarmClaw"
                .truecolor(CLI_CYAN_RGB.0, CLI_CYAN_RGB.1, CLI_CYAN_RGB.2)
                .bold(),
            "session closed.".truecolor(CLI_MUTED_RGB.0, CLI_MUTED_RGB.1, CLI_MUTED_RGB.2),
        );
        Ok(())
    }

    pub async fn handle_gateway_turn(
        &mut self,
        input: &str,
        channel_info: ChannelInfo,
    ) -> anyhow::Result<()> {
        let turn_span = gateway_turn_span(
            &self.id,
            self.llm.provider_name(),
            &channel_info,
            input.len(),
        );
        let input = input.trim();
        if input.is_empty() {
            anyhow::bail!("No message content was provided by the gateway.");
        }

        {
            let _guard = turn_span.enter();
            info!("Gateway turn started");
        }

        let safe_input = match SafetyLayer::scrub_prompt(input) {
            Ok(safe) => safe,
            Err(error) => {
                let blocked_message =
                    format!("Request blocked by SwarmClaw safety checks: {error}");
                queue_gateway_text(&channel_info, &blocked_message);
                {
                    let _guard = turn_span.enter();
                    self.record_message(Message {
                        role: Role::Assistant,
                        content: blocked_message,
                        timestamp: now_secs(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    warn!("Gateway turn blocked by safety");
                }
                return Ok(());
            }
        };

        {
            let _guard = turn_span.enter();
            self.record_message(Message {
                role: Role::User,
                content: Redactor::redact(&safe_input),
                timestamp: now_secs(),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        match self
            .respond(Some(channel_info))
            .instrument(turn_span.clone())
            .await
        {
            Ok(()) => {
                let _guard = turn_span.enter();
                info!("Gateway turn completed");
                Ok(())
            }
            Err(error) => {
                let _guard = turn_span.enter();
                warn!(error = %error, "Gateway turn failed");
                Err(error)
            }
        }
    }

    async fn respond(&mut self, channel_info: Option<ChannelInfo>) -> anyhow::Result<()> {
        let capabilities = self.llm.capabilities();

        if capabilities.supports_streaming {
            self.stream_think(channel_info).await
        } else if capabilities.supports_non_streaming && channel_info.is_none() {
            self.think().await
        } else if capabilities.supports_non_streaming {
            anyhow::bail!(
                "Provider '{}' only supports non-streaming responses in this SwarmClaw adapter, which is incompatible with the current gateway path.",
                self.llm.provider_name()
            )
        } else {
            anyhow::bail!(
                "Provider '{}' does not support response generation in this SwarmClaw adapter.",
                self.llm.provider_name()
            )
        }
    }

    /// Streaming thought loop for CLI and webhook gateways.
    pub async fn stream_think(&mut self, channel_info: Option<ChannelInfo>) -> anyhow::Result<()> {
        let capabilities = self.llm.capabilities();
        if !capabilities.supports_streaming {
            anyhow::bail!(
                "Provider '{}' does not support streaming responses in this SwarmClaw adapter.",
                self.llm.provider_name()
            );
        }

        let mut stdout = io::stdout();
        let cli_mode = channel_info.is_none();

        loop {
            if cli_mode {
                drain_resize_events(self, &mut stdout)?;
            }
            debug!(
                history_len = self.state.history.len(),
                cli_mode, "Preparing provider request"
            );
            let options = CompletionOptions {
                model: self.config.model.clone(),
                ..Default::default()
            };

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
                                Role::User => {
                                    formatted_history.push_str(&format!("Human: {}\n", msg.content))
                                }
                                Role::Assistant => {
                                    formatted_history.push_str(&format!("AI: {}\n\n", msg.content))
                                }
                                _ => {}
                            }
                        }

                        let client = reqwest::Client::new();
                        let payload = serde_json::json!({
                            "session_id": channel_info
                                .as_ref()
                                .map(|info| info.channel_id.clone())
                                .unwrap_or_else(|| self.id.clone()),
                            "user_question": user_question,
                            "org_id": org_id,
                            "should_use_memory": "YES",
                            "variables": serde_json::json!({ "formatted_history": formatted_history }).to_string()
                        });

                        if let Ok(res) = client
                            .post("http://localhost:8001/get-memory-context")
                            .header("Authorization", format!("Bearer {}", api_key))
                            .json(&payload)
                            .send()
                            .await
                        {
                            if res.status().is_success() {
                                if let Ok(body) = res.json::<serde_json::Value>().await {
                                    if let Some(memory_context) =
                                        body.get("memory_context_used").and_then(|v| v.as_str())
                                    {
                                        if !memory_context.is_empty()
                                            && memory_context.to_lowercase() != "none"
                                        {
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

            let prepared = prepare_turn_request(
                history_to_send,
                self.skills.iter().flat_map(|s| s.tools()).collect(),
                self.llm.provider_name(),
                capabilities,
                TurnMode::Streaming,
                now_secs(),
            );
            let history_to_send = prepared.history;
            let tools = prepared.tools;

            if let Some(notice) = prepared.disabled_tool_notice {
                if cli_mode {
                    write_cli_line(
                        &mut stdout,
                        format!(
                            "{} {}",
                            cli_chip("LIMITED", CLI_DEEP_RGB, CLI_AMBER_RGB),
                            notice.truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2),
                        ),
                    )?;
                }
            }

            let mut stream = self.llm.stream(&history_to_send, &options, &tools).await?;

            let mut full_content = String::new();
            let mut tool_calls = Vec::new();
            let mut current_tool_id = String::new();
            let mut current_tool_name = String::new();
            let mut current_tool_args = String::new();
            let mut current_thought_signature: Option<String> = None;

            if cli_mode {
                write_cli(
                    &mut stdout,
                    format!("{} ", cli_chip("SWARMCLAW", CLI_DEEP_RGB, CLI_MAGENTA_RGB)),
                )?;
                stdout.flush()?;
            }

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(ChatChunk::Content(delta)) => {
                        full_content.push_str(&delta);
                        if cli_mode {
                            print!("{}", delta);
                            stdout.flush()?;
                            drain_resize_events(self, &mut stdout)?;
                        }
                    }
                    Ok(ChatChunk::ToolCallStart {
                        id,
                        name,
                        thought_signature,
                    }) => {
                        debug!(tool_name = %name, tool_id = %id, "Received tool call start");
                        if !current_tool_name.is_empty() {
                            tool_calls.push(crate::llm::ToolCall {
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                arguments: current_tool_args.clone(),
                                thought_signature: current_thought_signature.take(),
                            });
                        }
                        current_tool_id = id;
                        current_tool_name = name;
                        current_tool_args.clear();
                        current_thought_signature = thought_signature;
                    }
                    Ok(ChatChunk::ToolCallDelta { arguments }) => {
                        current_tool_args.push_str(&arguments);
                    }
                    Ok(ChatChunk::Done) => {
                        if !current_tool_name.is_empty() {
                            tool_calls.push(crate::llm::ToolCall {
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                arguments: current_tool_args.clone(),
                                thought_signature: current_thought_signature.take(),
                            });
                            current_tool_name.clear();
                        }
                        if cli_mode {
                            write_cli_line(&mut stdout, "")?;
                        }
                        debug!(
                            assistant_chars = full_content.len(),
                            tool_calls = tool_calls.len(),
                            "Provider stream completed"
                        );
                        break;
                    }
                    Err(error) => {
                        if let Some(channel_info) = &channel_info {
                            queue_gateway_text(
                                channel_info,
                                &format!("SwarmClaw engine error: {error}"),
                            );
                        }
                        return Err(error);
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
                            "thought_signature": tc.thought_signature,
                        }
                    }));
                }
                assistant_tool_calls = Some(tc_vec);
            }

            if !full_content.is_empty() || assistant_tool_calls.is_some() {
                // Final redaction for history
                let redacted_content = Redactor::redact(&full_content);
                self.record_message(Message {
                    role: Role::Assistant,
                    content: redacted_content.clone(),
                    timestamp: now_secs(),
                    tool_calls: assistant_tool_calls,
                    tool_call_id: None,
                });

                if tool_calls.is_empty() {
                    if let Some(channel_info) = &channel_info {
                        queue_gateway_text(channel_info, &redacted_content);
                    }
                }
            }

            if !tool_calls.is_empty() {
                for tc in tool_calls {
                    if cli_mode {
                        write_cli_line(
                            &mut stdout,
                            format!(
                                "{} {} {}",
                                cli_chip("TOOL", CLI_FG_RGB, CLI_BORDER_RGB),
                                tc.name
                                    .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2)
                                    .bold(),
                                tc.arguments.truecolor(
                                    CLI_MUTED_RGB.0,
                                    CLI_MUTED_RGB.1,
                                    CLI_MUTED_RGB.2
                                ),
                            ),
                        )?;
                    }

                    let tool = tools.iter().find(|t| t.name() == tc.name).cloned();

                    let result = match tool {
                        Some(t) => {
                            debug!(tool_name = %tc.name, "Executing tool");
                            let args: serde_json::Value =
                                serde_json::from_str(&tc.arguments).unwrap_or_default();

                            // Use WorkerPool to isolate tool execution
                            match WorkerPool::execute_tool(t, args).await {
                                Ok(res) => res,
                                Err(e) => format!("Error: {}", e),
                            }
                        }
                        None => format!("Tool '{}' not found", tc.name),
                    };
                    debug!(
                        tool_name = %tc.name,
                        result_bytes = result.len(),
                        "Tool execution finished"
                    );

                    // Redact tool results
                    let redacted_result = Redactor::redact(&result);
                    if cli_mode {
                        write_cli_line(
                            &mut stdout,
                            format!(
                                "{} {}",
                                cli_chip("RESULT", CLI_FG_RGB, CLI_RESULT_RGB),
                                redacted_result.truecolor(
                                    CLI_MUTED_RGB.0,
                                    CLI_MUTED_RGB.1,
                                    CLI_MUTED_RGB.2
                                ),
                            ),
                        )?;
                    }

                    self.record_message(Message {
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

            if let Some(channel_info) = &channel_info {
                if full_content.trim().is_empty() {
                    queue_gateway_text(
                        channel_info,
                        "SwarmClaw completed the request, but the model returned no text.",
                    );
                }
            }

            break;
        }

        Ok(())
    }

    pub async fn think(&mut self) -> anyhow::Result<()> {
        let capabilities = self.llm.capabilities();
        if !capabilities.supports_non_streaming {
            anyhow::bail!(
                "Provider '{}' does not support non-streaming responses in this SwarmClaw adapter.",
                self.llm.provider_name()
            );
        }

        let mut stdout = io::stdout();

        loop {
            print!("Thinking...");
            stdout.flush()?;

            let options = CompletionOptions {
                model: self.config.model.clone(),
                ..Default::default()
            };

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
                                Role::User => {
                                    formatted_history.push_str(&format!("Human: {}\n", msg.content))
                                }
                                Role::Assistant => {
                                    formatted_history.push_str(&format!("AI: {}\n\n", msg.content))
                                }
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

                        if let Ok(res) = client
                            .post("http://localhost:8001/get-memory-context")
                            .header("Authorization", format!("Bearer {}", api_key))
                            .json(&payload)
                            .send()
                            .await
                        {
                            if res.status().is_success() {
                                if let Ok(body) = res.json::<serde_json::Value>().await {
                                    if let Some(memory_context) =
                                        body.get("memory_context_used").and_then(|v| v.as_str())
                                    {
                                        if !memory_context.is_empty()
                                            && memory_context.to_lowercase() != "none"
                                        {
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

            let prepared = prepare_turn_request(
                history_to_send,
                self.skills.iter().flat_map(|s| s.tools()).collect(),
                self.llm.provider_name(),
                capabilities,
                TurnMode::NonStreaming,
                now_secs(),
            );
            let history_to_send = prepared.history;
            let tools = prepared.tools;

            if let Some(notice) = prepared.disabled_tool_notice {
                write_cli_line(&mut stdout, format!(
                    "{} {}",
                    cli_chip("LIMITED", CLI_DEEP_RGB, CLI_AMBER_RGB),
                    notice.truecolor(CLI_AMBER_RGB.0, CLI_AMBER_RGB.1, CLI_AMBER_RGB.2),
                ))?;
                apply_cli_palette(&mut stdout)?;
            }

            match self
                .llm
                .complete_with_tools(&history_to_send, &options, &tools)
                .await
            {
                Ok(response) => {
                    print!("\r\x1b[K");
                    stdout.flush()?;

                    if let Some(content) = &response.content {
                        if !content.is_empty() {
                            let redacted_content = Redactor::redact(content);
                            write_cli_line(&mut stdout, format!(
                                "{} {}",
                                cli_chip("SWARMCLAW", CLI_DEEP_RGB, CLI_MAGENTA_RGB),
                                redacted_content.truecolor(
                                    CLI_FG_RGB.0,
                                    CLI_FG_RGB.1,
                                    CLI_FG_RGB.2
                                ),
                            ))?;
                            apply_cli_palette(&mut stdout)?;

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
                                                "thought_signature": tc.thought_signature,
                                            }
                                        }));
                                    }
                                    assistant_tool_calls = Some(tc_vec);
                                }
                            }

                            self.record_message(Message {
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
                            write_cli_line(&mut stdout, format!(
                                "{} {} {}",
                                cli_chip("TOOL", CLI_FG_RGB, CLI_BORDER_RGB),
                                tc.name
                                    .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2)
                                    .bold(),
                                tc.arguments.truecolor(
                                    CLI_MUTED_RGB.0,
                                    CLI_MUTED_RGB.1,
                                    CLI_MUTED_RGB.2
                                ),
                            ))?;
                            apply_cli_palette(&mut stdout)?;

                            let tool = tools.iter().find(|t| t.name() == tc.name).cloned();

                            let result = match tool {
                                Some(t) => {
                                    let args: serde_json::Value =
                                        serde_json::from_str(&tc.arguments).unwrap_or_default();

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
                            write_cli_line(&mut stdout, format!(
                                "{} {}",
                                cli_chip("RESULT", CLI_FG_RGB, CLI_RESULT_RGB),
                                redacted_result.truecolor(
                                    CLI_MUTED_RGB.0,
                                    CLI_MUTED_RGB.1,
                                    CLI_MUTED_RGB.2
                                ),
                            ))?;
                            apply_cli_palette(&mut stdout)?;

                            self.record_message(Message {
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
                    write_cli_line(&mut stdout, format!("Error: {}", e))?;
                    break;
                }
            }
        }

        if self.config.enable_analytics.unwrap_or(true) {
            self.log_analytics_turn().await;
        }

        Ok(())
    }

    async fn log_analytics_turn(&self) {
        if let Some(workspace_dir) = &self.workspace_root {
            let log_dir = workspace_dir.join(".swarmclaw");
            if !log_dir.exists() {
                let _ = tokio::fs::create_dir_all(&log_dir).await;
            }

            let log_file = log_dir.join("analytics.jsonl");
            
            // Gather the latest assistant response from history
            let mut prompt = String::new();
            let mut result = String::new();
            
            for msg in self.state.history.iter().rev() {
                if msg.role == Role::Assistant && result.is_empty() {
                    result = msg.content.clone();
                } else if msg.role == Role::User && prompt.is_empty() {
                    prompt = msg.content.clone();
                }
                
                if !prompt.is_empty() && !result.is_empty() {
                    break;
                }
            }

            let log_entry = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event": "agent_turn_completed",
                "data": {
                    "prompt": prompt,
                    "response": result,
                    "total_messages": self.state.history.len()
                }
            });

            let mut line = log_entry.to_string();
            line.push('\n');

            if let Ok(mut file) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .await
            {
                let _ = tokio::io::AsyncWriteExt::write_all(&mut file, line.as_bytes()).await;
            }

            // Circular Buffer Logic (Limit based on config)
            let max_mb = self.config.analytics_max_size_mb.unwrap_or(100);
            if let Ok(metadata) = tokio::fs::metadata(&log_file).await {
                if metadata.len() > max_mb * 1024 * 1024 {
                    // Rotate the log file by renaming it to .old
                    let rotated_file = log_dir.join("analytics.jsonl.old");
                    let _ = tokio::fs::rename(&log_file, &rotated_file).await;
                    // Note: This simple rotation drops the oldest 100MB when it fills up again.
                }
            }
        }
    }
}

fn sanitize_session_id(raw: &str) -> String {
    let sanitized = raw
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
        "session".to_string()
    } else {
        sanitized
    }
}

fn queue_gateway_text(channel_info: &ChannelInfo, content: &str) {
    if channel_info.platform == "internal" {
        return;
    }

    let _ = enqueue_gateway_text_message(
        &channel_info.platform,
        &channel_info.channel_id,
        &channel_info.token,
        channel_info.app_id.clone(),
        channel_info.delivery_context.clone(),
        content,
    );
}

fn cli_turn_span(session_id: &str, provider: &str, input_bytes: usize) -> tracing::Span {
    info_span!(
        "agent_turn",
        turn_id = %next_turn_id(),
        source = "cli",
        session_id = %session_id,
        provider = %provider,
        input_bytes = input_bytes
    )
}

fn gateway_turn_span(
    session_id: &str,
    provider: &str,
    channel_info: &ChannelInfo,
    input_bytes: usize,
) -> tracing::Span {
    info_span!(
        "agent_turn",
        turn_id = %next_turn_id(),
        source = "gateway",
        session_id = %session_id,
        provider = %provider,
        platform = %channel_info.platform,
        channel_id = %channel_info.channel_id,
        input_bytes = input_bytes
    )
}

fn next_turn_id() -> String {
    Uuid::new_v4().to_string()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn seeded_state(config: &AgentConfig) -> State {
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
    state
}

fn drain_resize_events(agent: &Agent, stdout: &mut io::Stdout) -> io::Result<()> {
    while event::poll(Duration::from_millis(0))? {
        if let Event::Resize(_, _) = event::read()? {
            agent.redraw_cli_screen(stdout)?;
        }
    }

    Ok(())
}

fn prepare_turn_request(
    mut history: Vec<Message>,
    tools: Vec<Arc<dyn crate::tools::Tool>>,
    provider_name: &str,
    capabilities: ProviderCapabilities,
    mode: TurnMode,
    timestamp: u64,
) -> TurnPreparation {
    if tools.is_empty() {
        return TurnPreparation {
            history,
            tools,
            disabled_tool_notice: None,
        };
    }

    let tools_supported = match mode {
        TurnMode::Streaming => capabilities.supports_streaming_tool_calls,
        TurnMode::NonStreaming => capabilities.supports_tool_calls,
    };

    if tools_supported {
        return TurnPreparation {
            history,
            tools,
            disabled_tool_notice: None,
        };
    }

    let reason = if capabilities.supports_tool_calls {
        "does not support streaming tool calls"
    } else {
        "does not support local tool calling"
    };
    let notice = format!(
        "SwarmClaw runtime note: Provider '{}' {} in this adapter. SwarmClaw tools are disabled for this turn. Do not claim to have executed commands, inspected files, or called tools.",
        provider_name, reason
    );
    history.push(Message {
        role: Role::System,
        content: notice.clone(),
        timestamp,
        tool_calls: None,
        tool_call_id: None,
    });

    TurnPreparation {
        history,
        tools: Vec::new(),
        disabled_tool_notice: Some(notice),
    }
}

fn session_capability_notice(
    provider_name: &str,
    capabilities: ProviderCapabilities,
    skill_count: usize,
) -> Option<String> {
    if skill_count == 0 {
        return None;
    }

    if !capabilities.supports_tool_calls {
        return Some(format!(
            "Provider '{}' is running in text-only mode in this adapter. SwarmClaw skills are loaded, but local tool execution is disabled.",
            provider_name
        ));
    }

    if !capabilities.supports_streaming_tool_calls {
        return Some(format!(
            "Provider '{}' supports tool calls, but not in the streaming path. SwarmClaw will fall back to text-only turns when streaming is required.",
            provider_name
        ));
    }

    None
}

fn apply_cli_palette(stdout: &mut io::Stdout) -> io::Result<()> {
    execute!(
        stdout,
        SetBackgroundColor(CLI_BG),
        SetForegroundColor(CLI_FG)
    )
}

fn apply_cli_terminal_theme(stdout: &mut io::Stdout) -> io::Result<()> {
    write!(
        stdout,
        "\x1b]10;rgb:{:02x}/{:02x}/{:02x}\x07\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x07",
        CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2, CLI_BG_RGB.0, CLI_BG_RGB.1, CLI_BG_RGB.2,
    )?;
    stdout.flush()
}

fn reset_cli_terminal_theme(stdout: &mut io::Stdout) -> io::Result<()> {
    write!(stdout, "\x1b]110\x07\x1b]111\x07")?;
    stdout.flush()
}

fn paint_cli_viewport(stdout: &mut io::Stdout) -> io::Result<()> {
    let (cols, rows) = size().unwrap_or((96, 32));
    let fill = " ".repeat(cols as usize);

    for row in 0..rows {
        execute!(stdout, MoveTo(0, row))?;
        write!(stdout, "{fill}")?;
    }

    stdout.flush()
}

fn write_cli(stdout: &mut io::Stdout, content: impl Display) -> io::Result<()> {
    let safe_content = content.to_string().replace("\r\n", "\n").replace("\n", "\r\n"); write!(stdout, "{safe_content}")?;
    apply_cli_palette(stdout)
}

fn write_cli_line(stdout: &mut io::Stdout, content: impl Display) -> io::Result<()> {
    let safe_content = content.to_string().replace("\r\n", "\n").replace("\n", "\r\n"); write!(stdout, "{safe_content}")?;
    execute!(stdout, Clear(ClearType::UntilNewLine))?;
    write!(stdout, "\r\n")?;
    apply_cli_palette(stdout)
}

fn render_cli_intro(
    stdout: &mut io::Stdout,
    agent_id: &str,
    provider_name: &str,
    model: &str,
    skill_count: usize,
    capabilities: ProviderCapabilities,
) -> io::Result<()> {
    let width = terminal_width();
    let subtitle = "Autonomous Rust runtime with streamed replies and native tool execution.";
    let stats = format!("Agent {agent_id}  |  Provider {provider_name}  |  Model {model}");
    let tool_mode = if skill_count == 0 {
        "Tools none".to_string()
    } else if capabilities.supports_tool_calls {
        "Tools enabled".to_string()
    } else {
        "Tools disabled".to_string()
    };
    let status = format!("{tool_mode}  |  Skills {skill_count}");
    let hints =
        "Type exit or quit to close the session. Tool calls stream inline below the response.";

    render_cli_logo(stdout, width)?;
    write_cli_line(stdout, cli_frame_top(width))?;
    write_cli_line(
        stdout,
        cli_frame_line("SwarmClaw CLI", width)
            .bold()
            .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2)
            .on_truecolor(CLI_DEEP_RGB.0, CLI_DEEP_RGB.1, CLI_DEEP_RGB.2),
    )?;
    write_cli_line(
        stdout,
        cli_frame_line(subtitle, width)
            .truecolor(CLI_MUTED_RGB.0, CLI_MUTED_RGB.1, CLI_MUTED_RGB.2)
            .on_truecolor(CLI_DEEP_RGB.0, CLI_DEEP_RGB.1, CLI_DEEP_RGB.2),
    )?;
    write_cli_line(
        stdout,
        cli_frame_line(&stats, width)
            .truecolor(CLI_CYAN_RGB.0, CLI_CYAN_RGB.1, CLI_CYAN_RGB.2)
            .on_truecolor(CLI_PANEL_RGB.0, CLI_PANEL_RGB.1, CLI_PANEL_RGB.2),
    )?;
    write_cli_line(
        stdout,
        cli_frame_line(&status, width)
            .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2)
            .on_truecolor(CLI_PANEL_RGB.0, CLI_PANEL_RGB.1, CLI_PANEL_RGB.2),
    )?;
    write_cli_line(
        stdout,
        cli_frame_line(hints, width)
            .truecolor(CLI_MUTED_RGB.0, CLI_MUTED_RGB.1, CLI_MUTED_RGB.2)
            .on_truecolor(CLI_PANEL_RGB.0, CLI_PANEL_RGB.1, CLI_PANEL_RGB.2),
    )?;
    write_cli_line(stdout, cli_frame_bottom(width))?;
    write_cli_line(stdout, cli_divider("READY", width, CLI_CYAN_RGB))?;
    Ok(())
}

fn render_cli_logo(stdout: &mut io::Stdout, width: usize) -> io::Result<()> {
    const LARGE_LOGO: &[&str] = &[
        "      OOO   OOO      ",
        "   OWSWWWOOOWWWSWO   ",
        "  OWWWWWSOOOSWWWWWWO  ",
        " OWWWWWWWPPPPWWWWWWO ",
        "OWWWWWWPPPPPPPPWWWWWWO",
        "OWWWWPPPPPPPPPPPPWWWWO",
        " OWWPPPEPPPPPEPPPWWO ",
        "  OWPPPPPMMPPPPPWO  ",
        "   OPPPBPPPPBPPPO   ",
        "    OPPPPOOOPPPPO    ",
        "     OOOO   OOOO     ",
    ];
    const COMPACT_LOGO: &[&str] = &[
        "    OOO OOO    ",
        "  OWWWWWWWWWO  ",
        " OWWWWPPWWWWWO ",
        "OWWWPPPPPPWWWO",
        " OWPPEPPPEPWO ",
        "  OPPPMMPPPO  ",
        "   OPPPOOPPO   ",
        "    OOO  OOO    ",
    ];
    let cell_width = if width >= 72 { 2 } else { 1 };
    let logo = if width >= 58 {
        LARGE_LOGO
    } else {
        COMPACT_LOGO
    };
    let title_lines: &[&str] = if width >= 72 {
        &["S W A R M", "C L A W"]
    } else {
        &["SWARMCLAW"]
    };
    let subtitle = if width >= 72 {
        "Winged local runtime with tools, streams, and graphite terminals."
    } else {
        "Winged local runtime."
    };

    for row in logo {
        write_cli_line(stdout, render_logo_pixel_row(row, width, cell_width))?;
    }
    for (index, title) in title_lines.iter().enumerate() {
        let color = if index == 0 {
            CLI_LOGO_CORAL_RGB
        } else {
            CLI_LOGO_BLUSH_RGB
        };
        write_cli_line(
            stdout,
            cli_center_line(title, width)
                .bold()
                .truecolor(color.0, color.1, color.2)
                .on_truecolor(CLI_DEEP_RGB.0, CLI_DEEP_RGB.1, CLI_DEEP_RGB.2),
        )?;
    }
    write_cli_line(
        stdout,
        cli_center_line(subtitle, width)
            .truecolor(CLI_MUTED_RGB.0, CLI_MUTED_RGB.1, CLI_MUTED_RGB.2)
            .on_truecolor(CLI_PANEL_RGB.0, CLI_PANEL_RGB.1, CLI_PANEL_RGB.2),
    )?;
    Ok(())
}

fn render_cli_history(stdout: &mut io::Stdout, history: &[Message]) -> io::Result<()> {
    let mut has_visible_messages = false;

    for message in history {
        match message.role {
            Role::System => continue,
            Role::User => {
                has_visible_messages = true;
                write_cli_line(
                    stdout,
                    format!(
                        "{} {}",
                        cli_chip("USER", CLI_DEEP_RGB, CLI_CYAN_RGB),
                        message
                            .content
                            .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2),
                    ),
                )?;
            }
            Role::Assistant => {
                if !message.content.is_empty() {
                    has_visible_messages = true;
                    write_cli_line(
                        stdout,
                        format!(
                            "{} {}",
                            cli_chip("SWARMCLAW", CLI_DEEP_RGB, CLI_MAGENTA_RGB),
                            message
                                .content
                                .truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2),
                        ),
                    )?;
                }

                if let Some(tool_calls) = &message.tool_calls {
                    for tool_call in tool_calls {
                        let name = tool_call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool");
                        let arguments = tool_call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");

                        has_visible_messages = true;
                        write_cli_line(
                            stdout,
                            format!(
                                "{} {} {}",
                                cli_chip("TOOL", CLI_FG_RGB, CLI_BORDER_RGB),
                                name.truecolor(CLI_FG_RGB.0, CLI_FG_RGB.1, CLI_FG_RGB.2)
                                    .bold(),
                                arguments.truecolor(
                                    CLI_MUTED_RGB.0,
                                    CLI_MUTED_RGB.1,
                                    CLI_MUTED_RGB.2
                                ),
                            ),
                        )?;
                    }
                }
            }
            Role::Tool => {
                has_visible_messages = true;
                write_cli_line(
                    stdout,
                    format!(
                        "{} {}",
                        cli_chip("RESULT", CLI_FG_RGB, CLI_RESULT_RGB),
                        message.content.truecolor(
                            CLI_MUTED_RGB.0,
                            CLI_MUTED_RGB.1,
                            CLI_MUTED_RGB.2
                        ),
                    ),
                )?;
            }
        }
    }

    if has_visible_messages {
        write_cli_line(stdout, "")?;
    }

    Ok(())
}

fn render_logo_pixel_row(pattern: &str, width: usize, cell_width: usize) -> String {
    let visible_width = pattern.chars().count() * cell_width;
    let left_pad = width.saturating_sub(visible_width) / 2;
    let right_pad = width.saturating_sub(visible_width + left_pad);
    let mut rendered = String::new();

    rendered.push_str(&logo_panel_fill(left_pad));
    for symbol in pattern.chars() {
        rendered.push_str(&logo_cell(symbol, cell_width));
    }
    rendered.push_str(&logo_panel_fill(right_pad));

    rendered
}

fn logo_panel_fill(width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    " ".repeat(width)
        .on_truecolor(CLI_DEEP_RGB.0, CLI_DEEP_RGB.1, CLI_DEEP_RGB.2)
        .to_string()
}

fn logo_cell(symbol: char, cell_width: usize) -> String {
    let glyph = match symbol {
        ' ' => " ".repeat(cell_width),
        'W' => "░".repeat(cell_width),
        'S' | 'B' => "▒".repeat(cell_width),
        'P' => "▓".repeat(cell_width),
        _ => "█".repeat(cell_width),
    };
    let bg = (CLI_DEEP_RGB.0, CLI_DEEP_RGB.1, CLI_DEEP_RGB.2);

    match symbol {
        'O' => glyph
            .truecolor(
                CLI_LOGO_OUTLINE_RGB.0,
                CLI_LOGO_OUTLINE_RGB.1,
                CLI_LOGO_OUTLINE_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        'W' => glyph
            .truecolor(
                CLI_LOGO_WING_RGB.0,
                CLI_LOGO_WING_RGB.1,
                CLI_LOGO_WING_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        'S' => glyph
            .truecolor(
                CLI_LOGO_WING_SHADE_RGB.0,
                CLI_LOGO_WING_SHADE_RGB.1,
                CLI_LOGO_WING_SHADE_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        'P' => glyph
            .truecolor(
                CLI_LOGO_CORAL_RGB.0,
                CLI_LOGO_CORAL_RGB.1,
                CLI_LOGO_CORAL_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        'B' => glyph
            .truecolor(
                CLI_LOGO_BLUSH_RGB.0,
                CLI_LOGO_BLUSH_RGB.1,
                CLI_LOGO_BLUSH_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        'E' | 'M' => glyph
            .truecolor(
                CLI_LOGO_FACE_RGB.0,
                CLI_LOGO_FACE_RGB.1,
                CLI_LOGO_FACE_RGB.2,
            )
            .on_truecolor(bg.0, bg.1, bg.2)
            .to_string(),
        _ => glyph.on_truecolor(bg.0, bg.1, bg.2).to_string(),
    }
}

fn terminal_width() -> usize {
    size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(96)
        .min(108)
        .max(1)
}

fn cli_frame_top(width: usize) -> String {
    format!("╭{}╮", "─".repeat(width.saturating_sub(2)))
        .truecolor(CLI_BORDER_RGB.0, CLI_BORDER_RGB.1, CLI_BORDER_RGB.2)
        .to_string()
}

fn cli_frame_bottom(width: usize) -> String {
    format!("╰{}╯", "─".repeat(width.saturating_sub(2)))
        .truecolor(CLI_BORDER_RGB.0, CLI_BORDER_RGB.1, CLI_BORDER_RGB.2)
        .to_string()
}

fn cli_frame_line(text: &str, width: usize) -> String {
    let inner = width.saturating_sub(4);
    let text = fit_text(text, inner);
    format!("│ {:inner$} │", text, inner = inner)
}

fn cli_divider(label: &str, width: usize, color: (u8, u8, u8)) -> String {
    let label = format!(" {} ", label);
    let available = width.saturating_sub(label.chars().count()).max(8);
    let left = available / 2;
    let right = available - left;
    format!("{}{}{}", "─".repeat(left), label, "─".repeat(right))
        .truecolor(color.0, color.1, color.2)
        .to_string()
}

fn cli_center_line(text: &str, width: usize) -> String {
    let text = fit_text(text, width);
    let pad = width.saturating_sub(text.chars().count());
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn fit_text(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }

    if max <= 1 {
        return "…".to_string();
    }

    let mut truncated = text.chars().take(max - 1).collect::<String>();
    truncated.push('…');
    truncated
}

fn fit_tail_text(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }

    if max <= 1 {
        return "…".to_string();
    }

    let tail = text
        .chars()
        .skip(len.saturating_sub(max.saturating_sub(1)))
        .collect::<String>();
    format!("…{tail}")
}

fn cli_chip(label: &str, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> String {
    format!(
        "\x1b[1m\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m {} \x1b[22m\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
        fg.0,
        fg.1,
        fg.2,
        bg.0,
        bg.1,
        bg.2,
        label,
        CLI_FG_RGB.0,
        CLI_FG_RGB.1,
        CLI_FG_RGB.2,
        CLI_BG_RGB.0,
        CLI_BG_RGB.1,
        CLI_BG_RGB.2,
    )
}

#[cfg(test)]
mod tests {
    use super::{now_secs, prepare_turn_request, session_capability_notice, TurnMode};
    use crate::core::state::{Message, Role};
    use crate::llm::ProviderCapabilities;
    use crate::tools::Tool;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Arc;

    #[derive(Clone)]
    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }

        fn description(&self) -> &str {
            "dummy tool"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({
                "type": "object",
            })
        }

        async fn execute(&self, _args: Value) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    fn sample_history() -> Vec<Message> {
        vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            timestamp: now_secs(),
            tool_calls: None,
            tool_call_id: None,
        }]
    }

    #[test]
    fn disables_tools_when_provider_lacks_tool_call_support() {
        let prepared = prepare_turn_request(
            sample_history(),
            vec![Arc::new(DummyTool)],
            "Gemini",
            ProviderCapabilities::streaming_text_only(),
            TurnMode::Streaming,
            123,
        );

        assert!(prepared.tools.is_empty());
        assert!(prepared.disabled_tool_notice.is_some());
        assert_eq!(prepared.history.len(), 2);
        assert_eq!(prepared.history[1].role, Role::System);
        assert!(prepared.history[1]
            .content
            .contains("SwarmClaw tools are disabled for this turn"));
    }

    #[test]
    fn preserves_tools_for_openai_compatible_providers() {
        let prepared = prepare_turn_request(
            sample_history(),
            vec![Arc::new(DummyTool)],
            "OpenAI",
            ProviderCapabilities::openai_compatible(),
            TurnMode::Streaming,
            123,
        );

        assert_eq!(prepared.tools.len(), 1);
        assert!(prepared.disabled_tool_notice.is_none());
        assert_eq!(prepared.history.len(), 1);
    }

    #[test]
    fn reports_session_notice_for_text_only_providers() {
        let notice =
            session_capability_notice("Anthropic", ProviderCapabilities::streaming_text_only(), 2);

        assert!(notice.is_some());
        assert!(notice.unwrap().contains("text-only mode"));
    }
}
