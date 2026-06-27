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

/// An active mouse drag-selection over the rendered transcript, in terminal
/// cell coordinates (`(col, row)`). `anchor` is where the drag began (mouse
/// down) and `cursor` is the current/last drag position. The two are ordered
/// in row-major order at extraction/highlight time (see [`normalize_selection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started (mouse-down), in `(col, row)` cell coords.
    pub anchor: (u16, u16),
    /// The current end of the selection (last drag position), in `(col, row)`.
    pub cursor: (u16, u16),
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
    /// Active mouse drag-selection over the transcript, if any. `None` when no
    /// selection is in progress / shown. Highlighted by [`draw`] and extracted
    /// (for copy) on mouse-up.
    pub selection: Option<Selection>,
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
            selection: None,
        }
    }

    /// Replace the transcript with a freshly-built view of `history`, preserving
    /// the current input, cursor, scroll and status. Used mid-turn after the live
    /// session history is mutated (e.g. assistant / tool messages recorded) so the
    /// transcript reflects the engine state while the user keeps typing.
    pub fn sync_transcript(&mut self, history: &[Message]) {
        self.transcript = history
            .iter()
            .map(|m| MessageView {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();
    }

    /// Insert a printable char at the cursor (byte offset) and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        let cursor = self.cursor.min(self.input.len());
        self.input.insert(cursor, c);
        self.cursor = cursor + c.len_utf8();
    }

    /// Delete the char immediately before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Find the start of the char preceding the cursor.
        let prev = prev_char_boundary(&self.input, self.cursor);
        self.input.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// Move the cursor one char to the left.
    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_char_boundary(&self.input, self.cursor);
        }
    }

    /// Move the cursor one char to the right.
    pub fn cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor = next_char_boundary(&self.input, self.cursor);
        }
    }

    /// Clear the input box and reset the cursor.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Take the current input (trimmed for command/submit purposes is the
    /// caller's job); resets the input box.
    pub fn take_input(&mut self) -> String {
        let out = std::mem::take(&mut self.input);
        self.cursor = 0;
        out
    }
}

/// Byte offset of the char boundary immediately before `idx`.
fn prev_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Byte offset of the char boundary immediately after `idx`.
fn next_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = (idx + 1).min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Clamp a desired scroll offset so the transcript can never scroll past its
/// content. `content_rows` is the total number of rendered transcript rows and
/// `viewport_rows` is the height (in rows) of the visible transcript area. The
/// maximum scroll is `content_rows - viewport_rows` (0 when everything fits).
pub fn clamp_scroll(desired: u16, content_rows: u16, viewport_rows: u16) -> u16 {
    let max = content_rows.saturating_sub(viewport_rows);
    desired.min(max)
}

/// Apply a signed scroll delta (positive = down, negative = up) to `current`,
/// clamped to `[0, max_scroll]`. Used for PageUp/PageDown and the mouse wheel.
pub fn scroll_by(current: u16, delta: i32, content_rows: u16, viewport_rows: u16) -> u16 {
    let next = (current as i32 + delta).max(0) as u16;
    clamp_scroll(next, content_rows, viewport_rows)
}

/// A single-line, whitespace-flattened preview of the streamed content, kept to
/// `width` chars (with a leading ellipsis when truncated). Mirrors the private
/// `streaming_preview` in `core::agent` so the fullscreen status line matches the
/// classic CLI spinner preview.
pub fn streaming_preview(content: &str, width: usize) -> String {
    let width = width.max(1);
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= width {
        flat
    } else {
        let tail: String = chars[chars.len() - (width - 1)..].iter().collect();
        format!("…{tail}")
    }
}

// ---------------------------------------------------------------------------
// Selection (mouse drag-select over rendered cells)
// ---------------------------------------------------------------------------

/// Order two `(col, row)` cell coordinates into `(start, end)` in row-major
/// order (top-to-bottom, then left-to-right). After this, `start` is always
/// at or before `end` when scanning rows then columns.
pub fn normalize_selection(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    // Compare by row first, then by column.
    let a_key = (a.1, a.0);
    let b_key = (b.1, b.0);
    if a_key <= b_key {
        (a, b)
    } else {
        (b, a)
    }
}

/// Extract the WYSIWYG text covered by a selection over already-rendered rows.
///
/// `rows` are the rendered terminal lines (one `String` per terminal row, each
/// `width` cells wide). `start`/`end` are `(col, row)` cell coordinates; they
/// are normalized internally so callers may pass them in any order.
///
/// Column rules per row `r` in `[start.row, end.row]`:
/// - left bound is `start.col` if `r == start.row`, else `0`.
/// - right bound (inclusive) is `end.col` if `r == end.row`, else the last column.
///
/// Each line's trailing whitespace is trimmed; lines are joined with `'\n'`.
/// Because it reads the rendered cells, this matches exactly what the user sees
/// regardless of wrapping or scroll.
pub fn extract_selection(rows: &[String], width: u16, start: (u16, u16), end: (u16, u16)) -> String {
    if rows.is_empty() || width == 0 {
        return String::new();
    }
    let (start, end) = normalize_selection(start, end);
    let last_col = width.saturating_sub(1);
    let max_row = rows.len().saturating_sub(1) as u16;

    let mut out_lines: Vec<String> = Vec::new();
    let first_row = start.1.min(max_row);
    let last_row = end.1.min(max_row);

    for r in first_row..=last_row {
        let row_chars: Vec<char> = rows[r as usize].chars().collect();
        let left = if r == start.1 { start.0 } else { 0 };
        let right = if r == end.1 { end.0 } else { last_col };
        // Clamp to actual rendered width.
        let left = left.min(last_col);
        let right = right.min(last_col);
        if right < left {
            out_lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for c in left..=right {
            if let Some(ch) = row_chars.get(c as usize) {
                line.push(*ch);
            }
        }
        // Trim trailing whitespace per line (WYSIWYG selections include the
        // blank padding cells of the terminal buffer).
        line.truncate(line.trim_end().len());
        out_lines.push(line);
    }

    out_lines.join("\n")
}

/// Read a rendered [`ratatui::buffer::Buffer`] into one `String` per row (each
/// the buffer's full width). Used so [`run_app`] can extract the selected text
/// from the most recently drawn frame, mirroring the test backend's row reader.
pub fn buffer_to_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buffer.area;
    let mut rows = Vec::with_capacity(area.height as usize);
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        rows.push(row);
    }
    rows
}

/// Encode `text` as an OSC 52 clipboard escape sequence: `ESC ] 52 ; c ; <b64> BEL`.
/// The payload is the standard-alphabet base64 of the UTF-8 bytes. Terminals
/// that support OSC 52 (including most over SSH) copy the payload to the system
/// clipboard. Pure; performs no I/O.
pub fn osc52(text: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let encoded = STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
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

    // --- Selection highlight --------------------------------------------
    // Applied last so it overlays the normal widgets. Uses the same normalized
    // linear range as `extract_selection`, clamped to the frame area, and paints
    // a reversed style onto the covered cells.
    if let Some(sel) = state.selection {
        highlight_selection(frame, area, sel.anchor, sel.cursor);
    }
}

/// Paint a reversed highlight over the cells covered by the selection
/// `anchor`..`cursor` (in `(col, row)` cell coords), clamped to `area`. Mirrors
/// the per-row column rules of [`extract_selection`] so the highlight matches
/// exactly what will be copied.
fn highlight_selection(frame: &mut Frame, area: ratatui::layout::Rect, anchor: (u16, u16), cursor: (u16, u16)) {
    let (start, end) = normalize_selection(anchor, cursor);
    let buf = frame.buffer_mut();
    let last_col = area.right().saturating_sub(1);
    let first_row = start.1.max(area.top());
    let last_row = end.1.min(area.bottom().saturating_sub(1));
    if area.width == 0 || area.height == 0 {
        return;
    }
    let highlight = Style::default().add_modifier(Modifier::REVERSED);
    let mut r = first_row;
    while r <= last_row {
        let left = if r == start.1 { start.0 } else { area.left() };
        let right = if r == end.1 { end.0 } else { last_col };
        let left = left.max(area.left()).min(last_col);
        let right = right.min(last_col);
        if right >= left {
            for c in left..=right {
                if let Some(cell) = buf.cell_mut((c, r)) {
                    cell.set_style(highlight);
                }
            }
        }
        if r == u16::MAX {
            break;
        }
        r += 1;
    }
}

// ---------------------------------------------------------------------------
// Live event loop (opt-in, default-OFF; see `supported`)
// ---------------------------------------------------------------------------

use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

/// RAII guard that puts the terminal into the fullscreen TUI state on creation
/// and ALWAYS restores it on drop (normal return, `?` early-return, or panic).
///
/// This is the single point that owns raw-mode / alternate-screen / mouse-capture
/// so the user's terminal can never be left in a broken state.
pub struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    /// Enter raw mode, switch to the alternate screen, and enable mouse capture.
    pub fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self { restored: false })
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let mut out = std::io::stdout();
        // Best-effort teardown in the exact reverse order of setup. We ignore
        // errors here because we are usually unwinding and must not panic.
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// What the key handler decided the event loop should do next.
#[derive(Debug, PartialEq, Eq)]
pub enum LoopAction {
    /// Nothing special; continue (state may have been mutated for redraw).
    Continue,
    /// The user submitted a line that should be processed as input/command.
    Submit(String),
    /// The user requested to exit the application.
    Quit,
}

/// Approximate the transcript viewport height for a given total terminal height.
/// Mirrors the layout in [`draw`]: 1 status line + 5-row input box are reserved,
/// the rest is transcript. Kept pure for clamping/scroll math and unit tests.
pub fn transcript_viewport_height(total_height: u16) -> u16 {
    total_height.saturating_sub(6).max(1)
}

/// Estimate the number of rendered transcript rows for scroll clamping. This is a
/// lightweight upper-bound proxy (each message contributes its label row plus a
/// blank spacer; wrapping is not precisely modeled). It only needs to be a sane
/// clamp bound so the user cannot scroll into a void of blank rows.
pub fn estimate_content_rows(state: &TuiState) -> u16 {
    let mut rows: usize = 0;
    for view in &state.transcript {
        // chip/body line + spacer
        rows += 2;
        // rough wrap estimate at 80 cols so long messages remain scrollable
        rows += view.content.len() / 80;
    }
    rows.min(u16::MAX as usize) as u16
}

/// Pure key-event reducer: applies a key press to `state` and reports the action
/// the event loop should take. Factored out of the terminal loop so it is unit
/// testable. `turn_in_flight` controls Esc semantics (cancel vs clear).
pub fn handle_key(
    state: &mut TuiState,
    code: KeyCode,
    modifiers: KeyModifiers,
    turn_in_flight: bool,
) -> LoopAction {
    let content_rows = estimate_content_rows(state);
    let viewport = transcript_viewport_height(0); // height unknown here; loop re-clamps on draw
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => LoopAction::Quit,
        KeyCode::Char(c) => {
            state.insert_char(c);
            LoopAction::Continue
        }
        KeyCode::Backspace => {
            state.backspace();
            LoopAction::Continue
        }
        KeyCode::Left => {
            state.cursor_left();
            LoopAction::Continue
        }
        KeyCode::Right => {
            state.cursor_right();
            LoopAction::Continue
        }
        KeyCode::Enter => {
            let line = state.take_input();
            if line.trim().is_empty() {
                LoopAction::Continue
            } else {
                LoopAction::Submit(line)
            }
        }
        KeyCode::Esc => {
            // Esc clears the input box; if there is nothing to clear and a turn is
            // running, the loop treats it as a cancel (handled by the caller).
            if !state.input.is_empty() {
                state.clear_input();
            }
            if turn_in_flight {
                // Signal handled by caller via separate cancel path; keep input cleared.
            }
            LoopAction::Continue
        }
        KeyCode::PageUp => {
            state.scroll = scroll_by(state.scroll, -(viewport.max(1) as i32), content_rows, viewport);
            LoopAction::Continue
        }
        KeyCode::PageDown => {
            state.scroll = scroll_by(state.scroll, viewport.max(1) as i32, content_rows, viewport);
            LoopAction::Continue
        }
        _ => LoopAction::Continue,
    }
}

/// Pure mouse-wheel reducer for scroll. Returns the new scroll offset.
pub fn handle_mouse_scroll(state: &TuiState, kind: MouseEventKind, viewport_rows: u16) -> u16 {
    let content_rows = estimate_content_rows(state);
    match kind {
        MouseEventKind::ScrollUp => scroll_by(state.scroll, -3, content_rows, viewport_rows),
        MouseEventKind::ScrollDown => scroll_by(state.scroll, 3, content_rows, viewport_rows),
        _ => state.scroll,
    }
}

/// Run the fullscreen interactive loop, driving the supplied agent.
///
/// This is the opt-in counterpart to the classic `Agent::run` REPL. It owns
/// terminal setup/teardown (via [`TerminalGuard`]) and an async `crossterm`
/// `EventStream`. Each input submission is dispatched to
/// [`crate::core::agent::Agent::run_fullscreen_turn`], which is a self-contained
/// parallel of `stream_think` rendered through ratatui.
pub async fn run_app(agent: &mut crate::core::agent::Agent) -> anyhow::Result<()> {
    use futures::StreamExt;

    let mut guard = TerminalGuard::enter()?;
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::from_history(&agent.state.history);
    let mut events = EventStream::new();
    // Snapshot of the most recently rendered buffer rows, used to extract the
    // WYSIWYG text under a mouse selection on release. Refreshed after each draw
    // because `terminal.draw` swaps buffers (so `current_buffer_mut` would be the
    // cleared next buffer, not what is on screen).
    let mut last_rows: Vec<String>;
    let mut last_width: u16;

    loop {
        let completed = terminal.draw(|frame| draw(frame, &state))?;
        last_rows = buffer_to_rows(completed.buffer);
        last_width = completed.area.width;

        let Some(ev) = events.next().await else {
            break;
        };
        let ev = match ev {
            Ok(ev) => ev,
            Err(_) => break,
        };

        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match handle_key(&mut state, key.code, key.modifiers, false) {
                    LoopAction::Continue => {}
                    LoopAction::Quit => break,
                    LoopAction::Submit(line) => {
                        let trimmed = line.trim().to_string();
                        if trimmed.eq_ignore_ascii_case("exit")
                            || trimmed.eq_ignore_ascii_case("quit")
                        {
                            break;
                        }
                        if trimmed.eq_ignore_ascii_case("/help")
                            || trimmed.eq_ignore_ascii_case("/helpp")
                        {
                            agent.record_message(Message {
                                role: Role::System,
                                content: help_text(),
                                timestamp: now_secs(),
                                tool_calls: None,
                                tool_call_id: None,
                            });
                            state.sync_transcript(&agent.state.history);
                            continue;
                        }

                        // Stateful slash-commands (/key, /model, /provider,
                        // /orchestrator, /multithread). Any other "/..." input is
                        // reported as unknown rather than sent to the model.
                        if trimmed.starts_with('/') {
                            let output = match agent.run_slash(&trimmed) {
                                crate::core::agent::SlashAction::Message(msg) => msg,
                                crate::core::agent::SlashAction::NotHandled => format!(
                                    "Unknown command: {trimmed}. Type /help for available commands."
                                ),
                            };
                            agent.record_message(Message {
                                role: Role::System,
                                content: output,
                                timestamp: now_secs(),
                                tool_calls: None,
                                tool_call_id: None,
                            });
                            state.sync_transcript(&agent.state.history);
                            continue;
                        }

                        // Record the user message, reflect it, redraw, then run the turn.
                        agent.record_message(Message {
                            role: Role::User,
                            content: trimmed.clone(),
                            timestamp: now_secs(),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                        state.sync_transcript(&agent.state.history);
                        terminal.draw(|frame| draw(frame, &state))?;

                        // Run the agent turn, rendering through ratatui.
                        if let Err(e) = agent
                            .run_fullscreen_turn(&mut terminal, &mut state, &mut events)
                            .await
                        {
                            agent.record_message(Message {
                                role: Role::System,
                                content: format!("[ERROR] {e}"),
                                timestamp: now_secs(),
                                tool_calls: None,
                                tool_call_id: None,
                            });
                        }
                        state.sync_transcript(&agent.state.history);
                        state.status = None;
                    }
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let size = terminal.size()?;
                    let viewport = transcript_viewport_height(size.height);
                    state.scroll = handle_mouse_scroll(&state, m.kind, viewport);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Begin a new selection anchored at the click cell.
                    let pos = (m.column, m.row);
                    state.selection = Some(Selection {
                        anchor: pos,
                        cursor: pos,
                    });
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(sel) = state.selection.as_mut() {
                        sel.cursor = (m.column, m.row);
                    }
                    // Redraw at top of loop reflects the moving highlight.
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(sel) = state.selection {
                        // A plain click (no drag) selects nothing.
                        if sel.anchor == sel.cursor {
                            state.selection = None;
                        } else {
                            // Extract from the most recently rendered buffer snapshot.
                            let text =
                                extract_selection(&last_rows, last_width, sel.anchor, sel.cursor);
                            if !text.is_empty() {
                                copy_to_clipboard(&text);
                                state.status = Some(format!("Copied {} chars", text.chars().count()));
                            }
                            // Keep the highlight visible; the next click/keypress
                            // begins a fresh selection (Down(Left) re-anchors).
                        }
                    }
                }
                _ => {}
            },
            Event::Resize(_, _) => { /* next draw re-lays out automatically */ }
            _ => {}
        }
    }

    // Explicit restore (Drop would also handle it) before returning to the shell.
    guard.restore();
    drop(terminal);
    Ok(())
}

/// Copy `text` to the clipboard, best-effort, via two independent mechanisms so
/// at least one succeeds across environments:
/// - OSC 52: written to stdout and flushed (works over SSH / inside tmux-aware
///   terminals, and in headless CI where no display clipboard exists).
/// - `arboard`: the local OS clipboard (fails silently on headless / no-display
///   hosts — that's expected and must not break the loop).
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    // OSC 52 — never let an I/O error escape.
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52(text).as_bytes());
    let _ = out.flush();

    // arboard — best-effort; ignore any error (headless / no-display).
    let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string()));
}

/// Help text shown in-transcript for `/help` in the fullscreen loop.
fn help_text() -> String {
    [
        "SwarmClaw Commands (fullscreen)",
        "  /help, /helpp      - Show this help",
        "  /key <API_KEY>     - Update the API key for the current provider",
        "  /model <name>      - Switch model (provider auto-detected)",
        "  /provider <name>   - Switch provider (openai, anthropic, gemini, groq, grok, ollama)",
        "  /orchestrator [on|off] - Toggle the orchestrator planner",
        "  /multithread [on|off]  - Toggle multithreaded execution",
        "  /think <off|low|medium|high> - Set reasoning verbosity",
        "  /trace [on|off]    - Toggle per-turn diagnostic lines",
        "  /fallback [a,b,c|off] - Set backup providers tried if the primary fails",
        "  /recall <query>    - Search past sessions for relevant messages",
        "  /usage             - Show session usage summary",
        "  /skills            - List learned skills for this workspace",
        "  /skill <slug>      - Show one learned skill's content",
        "  /forget <slug>     - Delete a learned skill",
        "  exit, quit         - Exit SwarmClaw",
        "  Esc                - Clear input / cancel an in-flight turn",
        "  Ctrl+C             - Exit",
        "  PageUp/Down, mouse wheel - Scroll transcript",
        "  Click+drag to select, release to copy",
    ]
    .join("\n")
}

use crate::core::agent::now_secs;

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
    fn clamp_scroll_bounds() {
        // Everything fits: max scroll is 0.
        assert_eq!(clamp_scroll(5, 3, 10), 0);
        // Content taller than viewport: max = content - viewport.
        assert_eq!(clamp_scroll(100, 20, 10), 10);
        assert_eq!(clamp_scroll(3, 20, 10), 3);
        // Exactly filling viewport => 0.
        assert_eq!(clamp_scroll(7, 10, 10), 0);
    }

    #[test]
    fn scroll_by_clamps_both_ends() {
        // Cannot go below 0.
        assert_eq!(scroll_by(0, -5, 50, 10), 0);
        // Going down clamps to max (50 - 10 = 40).
        assert_eq!(scroll_by(38, 10, 50, 10), 40);
        // Normal in-range move.
        assert_eq!(scroll_by(5, 3, 50, 10), 8);
    }

    #[test]
    fn insert_and_backspace_edit_input_and_cursor() {
        let mut s = TuiState::default();
        s.insert_char('h');
        s.insert_char('i');
        assert_eq!(s.input, "hi");
        assert_eq!(s.cursor, 2);
        s.backspace();
        assert_eq!(s.input, "h");
        assert_eq!(s.cursor, 1);
        s.backspace();
        s.backspace(); // no-op at start
        assert_eq!(s.input, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn cursor_movement_and_midline_insert() {
        let mut s = TuiState::default();
        for c in "abc".chars() {
            s.insert_char(c);
        }
        s.cursor_left();
        s.cursor_left();
        assert_eq!(s.cursor, 1);
        s.insert_char('Z');
        assert_eq!(s.input, "aZbc");
        assert_eq!(s.cursor, 2);
        s.cursor_right();
        s.cursor_right();
        s.cursor_right(); // clamps at end
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn multibyte_editing_respects_char_boundaries() {
        let mut s = TuiState::default();
        s.insert_char('é'); // 2 bytes
        s.insert_char('x');
        assert_eq!(s.cursor, 3);
        s.cursor_left();
        assert_eq!(s.cursor, 2);
        s.backspace(); // removes 'é'
        assert_eq!(s.input, "x");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn handle_key_enter_submits_nonempty_and_clears() {
        let mut s = TuiState::default();
        for c in "hello".chars() {
            s.insert_char(c);
        }
        let action = handle_key(&mut s, KeyCode::Enter, KeyModifiers::NONE, false);
        assert_eq!(action, LoopAction::Submit("hello".to_string()));
        assert_eq!(s.input, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn handle_key_enter_on_blank_is_continue() {
        let mut s = TuiState::default();
        let action = handle_key(&mut s, KeyCode::Enter, KeyModifiers::NONE, false);
        assert_eq!(action, LoopAction::Continue);
    }

    #[test]
    fn handle_key_ctrl_c_quits() {
        let mut s = TuiState::default();
        let action = handle_key(&mut s, KeyCode::Char('c'), KeyModifiers::CONTROL, false);
        assert_eq!(action, LoopAction::Quit);
    }

    #[test]
    fn handle_key_esc_clears_input() {
        let mut s = TuiState::default();
        for c in "draft".chars() {
            s.insert_char(c);
        }
        let action = handle_key(&mut s, KeyCode::Esc, KeyModifiers::NONE, false);
        assert_eq!(action, LoopAction::Continue);
        assert_eq!(s.input, "");
    }

    #[test]
    fn handle_mouse_scroll_wheel() {
        let mut s = TuiState::default();
        // Two messages, each ~2 rows => content_rows ~4, give a small viewport.
        s.transcript = vec![
            MessageView { role: Role::User, content: "a".repeat(200) },
            MessageView { role: Role::Assistant, content: "b".repeat(200) },
        ];
        s.scroll = 0;
        let down = handle_mouse_scroll(&s, MouseEventKind::ScrollDown, 2);
        assert!(down >= s.scroll, "scroll down should not decrease offset");
        let mut s2 = s.clone();
        s2.scroll = 1;
        let up = handle_mouse_scroll(&s2, MouseEventKind::ScrollUp, 2);
        assert!(up < s2.scroll || up == 0, "scroll up should decrease toward 0");
    }

    #[test]
    fn streaming_preview_truncates_with_ellipsis() {
        assert_eq!(streaming_preview("", 10), "");
        assert_eq!(streaming_preview("hi   there", 80), "hi there");
        let long = "x".repeat(100);
        let p = streaming_preview(&long, 10);
        assert!(p.starts_with('…'));
        assert_eq!(p.chars().count(), 10);
    }

    #[test]
    fn sync_transcript_preserves_input_and_status() {
        let mut s = TuiState::default();
        s.insert_char('q');
        s.status = Some("Working…".to_string());
        s.scroll = 3;
        let history = vec![msg(Role::User, "hello"), msg(Role::Assistant, "hi")];
        s.sync_transcript(&history);
        assert_eq!(s.transcript.len(), 2);
        assert_eq!(s.input, "q");
        assert_eq!(s.cursor, 1);
        assert_eq!(s.status.as_deref(), Some("Working…"));
        assert_eq!(s.scroll, 3);
    }

    #[test]
    fn transcript_viewport_height_reserves_chrome() {
        assert_eq!(transcript_viewport_height(24), 18);
        // Never zero even on tiny terminals.
        assert_eq!(transcript_viewport_height(3), 1);
    }

    // --- Selection / clipboard --------------------------------------------

    #[test]
    fn normalize_selection_orders_row_major() {
        // Already ordered.
        assert_eq!(
            normalize_selection((2, 1), (5, 3)),
            ((2, 1), (5, 3))
        );
        // Reversed by row.
        assert_eq!(
            normalize_selection((5, 3), (2, 1)),
            ((2, 1), (5, 3))
        );
        // Same row, reversed by column.
        assert_eq!(
            normalize_selection((8, 2), (3, 2)),
            ((3, 2), (8, 2))
        );
    }

    #[test]
    fn extract_selection_single_line_subrange() {
        let rows = vec!["hello world".to_string()];
        // Columns 0..=4 inclusive on row 0 => "hello".
        let got = extract_selection(&rows, 11, (0, 0), (4, 0));
        assert_eq!(got, "hello");
        // Columns 6..=10 => "world".
        let got = extract_selection(&rows, 11, (6, 0), (10, 0));
        assert_eq!(got, "world");
    }

    #[test]
    fn extract_selection_reversed_start_end_normalizes() {
        let rows = vec!["hello world".to_string()];
        let forward = extract_selection(&rows, 11, (0, 0), (4, 0));
        let reversed = extract_selection(&rows, 11, (4, 0), (0, 0));
        assert_eq!(forward, reversed);
        assert_eq!(reversed, "hello");
    }

    #[test]
    fn extract_selection_multiline_column_rules() {
        // Three rows, each padded to width 10 (terminal buffer style).
        let rows = vec![
            "abcdefghij".to_string(), // row 0
            "klmnopqrst".to_string(), // row 1
            "uvwxyz    ".to_string(), // row 2 (trailing spaces)
        ];
        // Select from (3, row0) to (2, row2):
        //  row0: from col 3 to last col (9)  => "defghij"
        //  row1: full row (middle)           => "klmnopqrst"
        //  row2: from col 0 to col 2         => "uvw"
        let got = extract_selection(&rows, 10, (3, 0), (2, 2));
        assert_eq!(got, "defghij\nklmnopqrst\nuvw");
    }

    #[test]
    fn extract_selection_trims_trailing_whitespace() {
        // Row padded with spaces (as a real terminal buffer row would be).
        let rows = vec!["hi        ".to_string()];
        // Select the whole width; trailing blanks must be trimmed.
        let got = extract_selection(&rows, 10, (0, 0), (9, 0));
        assert_eq!(got, "hi");
    }

    #[test]
    fn extract_selection_empty_inputs() {
        assert_eq!(extract_selection(&[], 10, (0, 0), (5, 0)), "");
        let rows = vec!["abc".to_string()];
        assert_eq!(extract_selection(&rows, 0, (0, 0), (2, 0)), "");
    }

    #[test]
    fn osc52_encodes_known_string() {
        // base64("hi") == "aGk=".
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        // base64("") == "" -> sequence with empty payload.
        assert_eq!(osc52(""), "\x1b]52;c;\x07");
    }

    #[test]
    fn selection_highlight_marks_cells_reversed() {
        let mut state = TuiState::default();
        state.input = "select me".to_string();
        // Highlight a small range on row 0.
        state.selection = Some(Selection {
            anchor: (0, 0),
            cursor: (3, 0),
        });
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| draw(frame, &state))
            .expect("draw frame");
        let buffer = terminal.backend().buffer();
        // Cells (0,0)..=(3,0) should carry the REVERSED modifier.
        for x in 0u16..=3 {
            let cell = buffer.cell((x, 0)).expect("cell exists");
            assert!(
                cell.modifier.contains(Modifier::REVERSED),
                "cell ({x},0) should be reversed-highlighted"
            );
        }
        // A cell well outside the selection should not be highlighted.
        let outside = buffer.cell((10, 0)).expect("cell exists");
        assert!(
            !outside.modifier.contains(Modifier::REVERSED),
            "cell outside selection must not be highlighted"
        );
    }

    #[test]
    fn buffer_to_rows_matches_render() {
        let mut state = TuiState::default();
        state.input = "xyz".to_string();
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        let completed = terminal
            .draw(|frame| draw(frame, &state))
            .expect("draw frame");
        let rows = buffer_to_rows(completed.buffer);
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().any(|r| r.contains("xyz")), "input text missing");
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
