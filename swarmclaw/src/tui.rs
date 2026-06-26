//! Flicker-free fullscreen terminal UI rendering layer (ratatui).
//!
//! PR-1 ("foundation"): this module is **purely additive** and is NOT yet wired
//! into the live interactive loop. It provides a pure, testable rendering layer:
//! a [`TuiState`] holding renderable app state and a pure [`draw`] function that
//! paints a [`ratatui::Frame`] from that state. No terminal I/O, no raw mode, no
//! alternate screen, no async, no global state.
//!
//! Colors are kept consistent with the existing hand-rolled crossterm TUI in
//! `core::agent` by replicating the relevant `CLI_*_RGB` constants locally (those
//! constants are private to `core::agent`; replicating the few we need keeps this
//! PR isolated rather than widening their visibility).

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::core::state::{Message, Role};

// ---------------------------------------------------------------------------
// Palette (mirrors the private `CLI_*_RGB` constants in `core::agent`).
// ---------------------------------------------------------------------------

const CLI_BG_RGB: (u8, u8, u8) = (23, 24, 27);
const CLI_FG_RGB: (u8, u8, u8) = (231, 233, 238);
const CLI_WHITE_TEXT_RGB: (u8, u8, u8) = (245, 245, 250);
const CLI_BORDER_RGB: (u8, u8, u8) = (52, 56, 65);
const CLI_MUTED_RGB: (u8, u8, u8) = (154, 163, 173);
const CLI_CYAN_RGB: (u8, u8, u8) = (138, 162, 211);
const CLI_MAGENTA_RGB: (u8, u8, u8) = (96, 117, 158);
const CLI_AMBER_RGB: (u8, u8, u8) = (198, 160, 93);
const CLI_USER_BG_RGB: (u8, u8, u8) = (90, 95, 105);
const CLI_AI_BG_RGB: (u8, u8, u8) = (40, 44, 52);
const CLI_RESULT_RGB: (u8, u8, u8) = (45, 48, 54);

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A lightweight, render-only view of a single transcript message.
#[derive(Debug, Clone)]
pub struct MessageView {
    pub role: Role,
    pub content: String,
}

/// All state required to render one frame of the fullscreen TUI.
///
/// This is intentionally plain data: it is constructed from the live session
/// state and consumed by [`draw`]. It performs no I/O.
#[derive(Debug, Clone, Default)]
pub struct TuiState {
    /// The conversation transcript, oldest first.
    pub transcript: Vec<MessageView>,
    /// The current contents of the input box.
    pub input: String,
    /// Cursor position (byte offset) within `input`.
    pub cursor: usize,
    /// Vertical scroll offset (in rows) applied to the transcript.
    pub scroll: u16,
    /// Optional status / streaming-preview line shown above the input box.
    pub status: Option<String>,
}

impl TuiState {
    /// Build a [`TuiState`] from the live session history.
    pub fn from_history(history: &[Message]) -> Self {
        let transcript = history
            .iter()
            .map(|m| MessageView {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();
        TuiState {
            transcript,
            input: String::new(),
            cursor: 0,
            scroll: 0,
            status: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Role chips
// ---------------------------------------------------------------------------

/// The short, uppercase label shown for a role's chip.
fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "USER",
        Role::Assistant => "SWARMCLAW",
        Role::System => "SYSTEM",
        Role::Tool => "TOOL",
    }
}

/// The (foreground, background) chip colors for a role, mirroring the accents
/// used by the existing crossterm TUI.
fn role_chip_colors(role: &Role) -> ((u8, u8, u8), (u8, u8, u8)) {
    match role {
        Role::User => (CLI_WHITE_TEXT_RGB, CLI_USER_BG_RGB),
        Role::Assistant => (CLI_CYAN_RGB, CLI_AI_BG_RGB),
        Role::System => (CLI_MUTED_RGB, CLI_AI_BG_RGB),
        Role::Tool => (CLI_AMBER_RGB, CLI_RESULT_RGB),
    }
}

/// Build the styled spans for a single transcript message: a padded role chip
/// followed by the message content.
fn message_lines(view: &MessageView) -> Vec<Line<'static>> {
    let label = role_label(&view.role);
    let (fg, bg) = role_chip_colors(&view.role);
    let chip = Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(rgb(fg))
            .bg(rgb(bg))
            .add_modifier(Modifier::BOLD),
    );
    let body = Span::styled(
        format!(" {}", view.content.clone()),
        Style::default().fg(rgb(CLI_FG_RGB)),
    );
    vec![Line::from(vec![chip, body])]
}

// ---------------------------------------------------------------------------
// Draw
// ---------------------------------------------------------------------------

/// Render one frame of the fullscreen TUI from `state`.
///
/// Pure with respect to terminal I/O: it only paints into `frame`. Layout is a
/// vertical stack of (1) a transcript area filling the available height, (2) a
/// single status/divider line, and (3) a bordered, multi-line input box.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let area = frame.area();

    // Charcoal background across the whole surface.
    let background = Block::default().style(Style::default().bg(rgb(CLI_BG_RGB)));
    frame.render_widget(background, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // transcript (fills)
            Constraint::Length(1), // status / divider
            Constraint::Length(5), // bordered input box
        ])
        .split(area);

    // --- Transcript ------------------------------------------------------
    let mut lines: Vec<Line> = Vec::new();
    for view in &state.transcript {
        lines.extend(message_lines(view));
        // Blank spacer line between messages for readability.
        lines.push(Line::from(""));
    }

    let transcript = Paragraph::new(lines)
        .style(Style::default().fg(rgb(CLI_FG_RGB)).bg(rgb(CLI_BG_RGB)))
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    frame.render_widget(transcript, chunks[0]);

    // --- Status / divider line ------------------------------------------
    let status_text = state.status.clone().unwrap_or_default();
    let status = Paragraph::new(status_text)
        .style(Style::default().fg(rgb(CLI_MUTED_RGB)).bg(rgb(CLI_BG_RGB)));
    frame.render_widget(status, chunks[1]);

    // --- Input box -------------------------------------------------------
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " INPUT ",
            Style::default()
                .fg(rgb(CLI_MAGENTA_RGB))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(rgb(CLI_BG_RGB)))
        .border_style(Style::default().fg(rgb(CLI_BORDER_RGB)));

    let input = Paragraph::new(state.input.clone())
        .style(Style::default().fg(rgb(CLI_FG_RGB)).bg(rgb(CLI_BG_RGB)))
        .wrap(Wrap { trim: false })
        .block(input_block);
    frame.render_widget(input, chunks[2]);
}

// ---------------------------------------------------------------------------
// Feature gate
// ---------------------------------------------------------------------------

/// Whether the opt-in fullscreen TUI is enabled.
///
/// Gated on the `SWARMCLAW_FULLSCREEN_TUI` environment variable (`"1"` or
/// `"true"`, case-insensitive). This is a stub for later live-loop wiring; it is
/// intentionally **not** called from the interactive loop in this PR.
pub fn supported() -> bool {
    match std::env::var("SWARMCLAW_FULLSCREEN_TUI") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true"
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            timestamp: 0,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Render `state` at the given size and return the per-row text of the buffer.
    fn render_rows(width: u16, height: u16, state: &TuiState) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| draw(frame, state))
            .expect("draw frame");

        let buffer = terminal.backend().buffer().clone();
        let area = buffer.area;
        let mut rows = Vec::with_capacity(area.height as usize);
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            rows.push(row);
        }
        rows
    }

    /// True if any row in the buffer contains `needle`.
    fn buffer_contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    #[test]
    fn renders_empty_state_without_panicking() {
        let state = TuiState::default();
        let rows = render_rows(80, 24, &state);
        // We expect a full 24-row buffer and the input box title present.
        assert_eq!(rows.len(), 24);
        assert!(
            buffer_contains(&rows, "INPUT"),
            "input box title missing from empty render"
        );
    }

    #[test]
    fn transcript_shows_labels_and_text() {
        let history = vec![
            msg(Role::User, "hello world"),
            msg(Role::Assistant, "greetings traveler"),
        ];
        let state = TuiState::from_history(&history);
        let rows = render_rows(80, 24, &state);

        assert!(buffer_contains(&rows, "USER"), "USER chip missing");
        assert!(
            buffer_contains(&rows, "SWARMCLAW"),
            "SWARMCLAW chip missing"
        );
        assert!(
            buffer_contains(&rows, "hello world"),
            "user message text missing"
        );
        assert!(
            buffer_contains(&rows, "greetings traveler"),
            "assistant message text missing"
        );
    }

    #[test]
    fn input_box_renders_title_and_text() {
        let mut state = TuiState::default();
        state.input = "draft reply".to_string();
        let rows = render_rows(80, 24, &state);

        assert!(buffer_contains(&rows, "INPUT"), "INPUT title missing");
        assert!(
            buffer_contains(&rows, "draft reply"),
            "input text missing from input box"
        );
    }

    #[test]
    fn long_message_wraps_across_multiple_rows() {
        // A message far wider than the 40-column buffer must wrap. Pick a
        // distinctive last word and assert it lands below the first row of the
        // message.
        let long = format!("{} TAILWORD", "wrap ".repeat(40));
        let history = vec![msg(Role::User, &long)];
        let state = TuiState::from_history(&history);
        let rows = render_rows(40, 24, &state);

        let first_wrap_row = rows
            .iter()
            .position(|row| row.contains("wrap"))
            .expect("message body not found");
        let tail_row = rows
            .iter()
            .position(|row| row.contains("TAILWORD"))
            .expect("tail word not found");

        assert!(
            tail_row > first_wrap_row,
            "expected wrapping: tail row {tail_row} should be below first row {first_wrap_row}"
        );
    }

    #[test]
    fn status_line_renders_when_present() {
        let mut state = TuiState::default();
        state.status = Some("…streaming preview".to_string());
        let rows = render_rows(80, 24, &state);
        assert!(
            buffer_contains(&rows, "streaming preview"),
            "status line text missing"
        );
    }

    #[test]
    fn supported_reflects_env_var() {
        // Env tests can be flaky under parallelism; keep this self-contained by
        // saving and restoring the original value, and only asserting on values
        // we set ourselves immediately before reading.
        let key = "SWARMCLAW_FULLSCREEN_TUI";
        let original = std::env::var(key).ok();

        std::env::set_var(key, "1");
        assert!(supported(), "expected supported() == true for \"1\"");

        std::env::set_var(key, "true");
        assert!(supported(), "expected supported() == true for \"true\"");

        std::env::set_var(key, "0");
        assert!(!supported(), "expected supported() == false for \"0\"");

        std::env::remove_var(key);
        assert!(!supported(), "expected supported() == false when unset");

        // Restore.
        match original {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
