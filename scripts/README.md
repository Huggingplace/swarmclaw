# SwarmClaw dev scripts

## `tui_smoke.py` — fullscreen TUI PTY smoke test

A self-contained, std-lib-only Python pseudo-terminal (PTY) harness that drives
the **real** compiled `swarmclaw` binary headlessly to validate the fullscreen
ratatui TUI's launch and clean teardown. The existing Rust unit tests only cover
the pure rendering/state logic (`swarmclaw/src/tui.rs`); this exercises the live
alt-screen + raw-mode lifecycle that otherwise needs a human eyeballing a real
terminal.

### Run

```sh
# Build the binary first (can take a few minutes):
cargo build -p swarmclaw

# Then run the smoke test:
python3 scripts/tui_smoke.py
# verbose (prints captured PTY bytes + per-marker hits):
python3 scripts/tui_smoke.py -v
```

Exit code is `0` on PASS, non-zero on FAIL. It self-bounds with timeouts and
always kills the child (SIGTERM then SIGKILL) on timeout — it never hangs.

### Options / env

| flag / env | default | meaning |
|---|---|---|
| `--binary PATH` / `SWARMCLAW_BIN` | `target/debug/swarmclaw` | binary under test |
| `--startup-timeout S` / `TUI_SMOKE_STARTUP_TIMEOUT` | `25` | wait for startup markers |
| `--exit-timeout S` / `TUI_SMOKE_EXIT_TIMEOUT` | `20` | wait for clean exit |
| `TUI_SMOKE_SETTLE` | `1.5` | pause after marker before typing |
| `--skip-classic` | off | run only the fullscreen case |
| `-v` / `--verbose` | off | dump captured PTY output |

### What it asserts

**Case 1 — DEFAULT (fullscreen):**
1. Enters the alternate screen — emits `ESC [ ? 1049 h`.
2. The ratatui UI actually renders — the bordered input-box title `INPUT`
   (a marker unique to `tui.rs::draw`) appears.
3. On `exit\r` (fallback: Ctrl+C = `\x03`) the process exits.
4. It exits **cleanly (status 0)** and **restores the terminal** — emits the
   alt-screen leave `ESC [ ? 1049 l`.

**Case 2 — opt-out classic CLI (`SWARMCLAW_FULLSCREEN_TUI=0`):**
- The classic banner `SwarmClaw CLI` renders, the fullscreen-only `INPUT` box
  marker is absent, and the program exits cleanly on `exit\r`.

### How it reaches the interactive loop headlessly

- **Provider:** sets `LLM_PROVIDER=ollama` + `OLLAMA_HOST` so `main.rs` resolves a
  provider with no API key and skips the first-run setup prompts.
- **Memory prompt:** sets `HUGGINGPLACE_MEMORY_ENABLED=false`.
- **Workspace:** a fresh temp dir via `--workspace`, with an empty `.env` and a
  pre-created `models/<model>.gguf` so the model "ensure" step short-circuits
  instead of attempting a network fetch.
- **Display (Xvfb):** `run_agent()` unconditionally builds `DesktopSkill`, whose
  `::new()` does `Enigo::new(...).unwrap()` (`swarmclaw/src/skills/desktop.rs:226`).
  Enigo needs an X11/Wayland display; with no `DISPLAY` it returns `Err` and the
  unwrap **panics before the loop is reached**. The harness starts a throwaway
  **Xvfb** virtual display and exports `DISPLAY` to the child (harness-only; no
  Rust changes). If `Xvfb` is missing, the harness runs without it and honestly
  reports the desktop-skill panic as the blocker.

### Notes / gotchas

- The classic CLI path **also** enters the alternate screen
  (`core::agent::TerminalUiGuard::enter`), so alt-screen enter is *not* a
  fullscreen-vs-classic discriminator. The discriminator used is the ratatui
  `INPUT` box title vs the classic `SwarmClaw CLI` banner.
- A short settle pause (`TUI_SMOKE_SETTLE`) after the startup marker is needed
  because the marker/banner can be emitted a beat before the interactive event
  loop starts reading input.
