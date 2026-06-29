#!/usr/bin/env python3
"""
PTY smoke test for SwarmClaw's fullscreen ratatui TUI.

WHY
---
SwarmClaw now defaults to a fullscreen TUI (ratatui: alternate screen + raw
mode, see swarmclaw/src/tui.rs and core::agent::Agent::run). That path cannot be
exercised by the existing unit tests, which only cover the *pure* rendering/state
logic. This harness drives the *real* compiled binary through a pseudo-terminal
(PTY) so we can headlessly assert the full launch + teardown lifecycle that a
human would otherwise have to eyeball in a real terminal.

WHAT IT ASSERTS
---------------
Case 1 (DEFAULT, fullscreen):
  1. The process enters the alternate screen        -> emits ESC [ ? 1049 h
  2. The ratatui fullscreen UI actually renders      -> the bordered input box
     title " INPUT " appears (a marker unique to tui.rs::draw, never printed by
     the classic CLI) and/or a role chip.
  3. On `exit\r` (with Ctrl+C = \x03 as a fallback) the process exits, and...
  4. ...it exits CLEANLY (status 0) AND restores the terminal -> emits the
     alt-screen LEAVE sequence ESC [ ? 1049 l.

Case 2 (opt-out, SWARMCLAW_FULLSCREEN_TUI=0, classic CLI):
  - The classic line-based CLI banner "SwarmClaw CLI" renders and the program
    exits cleanly, and the fullscreen-only " INPUT " box title is NOT present.
  NOTE: the classic CLI *also* uses an alternate screen (see
  core::agent::TerminalUiGuard::enter -> EnterAlternateScreen), so alt-screen
  enter is NOT a fullscreen-vs-classic discriminator. The discriminator used
  here is the ratatui " INPUT " box title vs the classic "SwarmClaw CLI" banner.

ENVIRONMENT NEEDED TO REACH THE INTERACTIVE LOOP
------------------------------------------------
main.rs's run_agent() prompts interactively for a provider + API key unless a
provider can be inferred from the environment. Setting OLLAMA_HOST makes
ProviderKind::infer_from_env() resolve to Ollama, which needs no API key
(read_api_key() == "not_needed"). We also:
  - run in a fresh temp workspace (via --workspace) so no stray AGENTS.md /
    sessions interfere and all state files land in the temp dir,
  - drop an empty .env into that workspace dir (dotenv().ok() is best-effort),
  - set HUGGINGPLACE_MEMORY_ENABLED=false to skip the memory opt-in prompt,
  - pre-create <workspace>/models/<model>.gguf so the Ollama "ensure_model"
    step short-circuits instead of attempting a (slow, doomed) network fetch.

HEADLESS DISPLAY (Xvfb)
-----------------------
run_agent() unconditionally constructs DesktopSkill, whose ::new() calls
Enigo::new(...).unwrap() (swarmclaw/src/skills/desktop.rs:226). Enigo needs an
X11/Wayland display; with no DISPLAY it returns Err and the unwrap PANICS before
the interactive loop is ever reached. To exercise the real binary headlessly we
start a throwaway Xvfb virtual display and export DISPLAY to the child. This is a
harness-only workaround (no Rust changes). If Xvfb is unavailable, the harness
runs without it and will honestly report the desktop-skill panic as the blocker.

EVERYTHING HAS A TIMEOUT
------------------------
We never block forever: os.read is gated by select() with absolute deadlines,
and on any timeout the child is killed (SIGTERM then SIGKILL). Wrap your own
invocations in `timeout` too if you like; the harness already self-bounds.

USAGE
-----
  python3 scripts/tui_smoke.py [--binary PATH] [--startup-timeout S]
                               [--exit-timeout S] [--skip-classic] [-v]

  Env overrides:
    SWARMCLAW_BIN          path to the binary (default: target/debug/swarmclaw)
    TUI_SMOKE_STARTUP_TIMEOUT   seconds to wait for fullscreen markers (def 25)
    TUI_SMOKE_EXIT_TIMEOUT      seconds to wait for clean exit       (def 20)
    OLLAMA_HOST           forwarded to the child (default http://127.0.0.1:11434)

  Exit code 0 on PASS, non-zero on FAIL.
"""

import argparse
import errno
import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time

# Escape sequences we look for in the PTY byte stream.
ALT_ENTER = b"\x1b[?1049h"   # switch to alternate screen buffer
ALT_LEAVE = b"\x1b[?1049l"   # restore the primary screen buffer
# Marker unique to the ratatui fullscreen draw() (the bordered input box title).
FULLSCREEN_MARKER = b"INPUT"
# Marker printed only by the classic line-based CLI intro.
CLASSIC_MARKER = b"SwarmClaw CLI"
# Settle pause after the startup marker appears, before typing, so the
# interactive event loop is definitely reading (markers can lead reads slightly).
SETTLE_SECS = float(os.environ.get("TUI_SMOKE_SETTLE", "1.5"))

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class Xvfb:
    """Best-effort throwaway X virtual framebuffer so the headless child can
    construct DesktopSkill (Enigo) without panicking. No-op if Xvfb is missing
    or a DISPLAY is already set."""

    def __init__(self, verbose=False):
        self.verbose = verbose
        self.proc_pid = None
        self.display = None

    def start(self):
        if os.environ.get("DISPLAY"):
            vlog(self.verbose, f"  [xvfb] DISPLAY already set ({os.environ['DISPLAY']}); skipping")
            self.display = os.environ["DISPLAY"]
            return self.display
        if not shutil.which("Xvfb"):
            log("  [xvfb] Xvfb not found; child will likely panic in DesktopSkill::new")
            return None
        # Pick a display number unlikely to collide.
        for num in range(99, 130):
            sock = f"/tmp/.X11-unix/X{num}"
            if os.path.exists(sock):
                continue
            disp = f":{num}"
            try:
                pid = os.fork()
            except OSError:
                return None
            if pid == 0:  # child: become Xvfb
                try:
                    devnull = os.open(os.devnull, os.O_RDWR)
                    os.dup2(devnull, 1)
                    os.dup2(devnull, 2)
                    os.execvp(
                        "Xvfb",
                        ["Xvfb", disp, "-screen", "0", "1024x768x24", "-nolisten", "tcp"],
                    )
                except Exception:
                    os._exit(127)
            # parent: wait for the socket to appear (bounded).
            deadline = time.monotonic() + 5.0
            while time.monotonic() < deadline:
                # Did Xvfb die immediately?
                try:
                    wpid, _ = os.waitpid(pid, os.WNOHANG)
                    if wpid == pid:
                        break  # try next display number
                except ChildProcessError:
                    break
                if os.path.exists(sock):
                    self.proc_pid = pid
                    self.display = disp
                    log(f"  [xvfb] started virtual display {disp} (pid {pid})")
                    return disp
                time.sleep(0.05)
            # cleanup failed attempt
            try:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            except (ProcessLookupError, ChildProcessError, OSError):
                pass
        log("  [xvfb] could not start a virtual display")
        return None

    def stop(self):
        if self.proc_pid is None:
            return
        try:
            os.kill(self.proc_pid, signal.SIGTERM)
            time.sleep(0.2)
            os.kill(self.proc_pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.proc_pid, 0)
        except (ChildProcessError, OSError):
            pass
        self.proc_pid = None


def log(msg):
    print(msg, flush=True)


def vlog(verbose, msg):
    if verbose:
        print(msg, flush=True)


def set_winsize(fd, rows=40, cols=120):
    """Give the PTY a sane fixed size so ratatui has room to draw."""
    try:
        winsize = struct.pack("HHHH", rows, cols, 0, 0)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)
    except OSError:
        pass


def make_workspace(model="llama3"):
    """Create an isolated temp workspace pre-seeded to avoid prompts/fetches."""
    ws = tempfile.mkdtemp(prefix="swarmclaw_tui_smoke_")
    # Empty .env so dotenv has something to read; no secrets needed.
    open(os.path.join(ws, ".env"), "w").close()
    # Pre-create the model file so ensure_model() short-circuits (no network).
    models_dir = os.path.join(ws, "models")
    os.makedirs(models_dir, exist_ok=True)
    open(os.path.join(models_dir, f"{model}.gguf"), "w").close()
    return ws


def child_env(workspace, display=None):
    env = dict(os.environ)
    # X display for DesktopSkill/Enigo (avoids the ::new() unwrap panic).
    if display:
        env["DISPLAY"] = display
    # Resolve provider to Ollama without prompting for a key.
    env.setdefault("OLLAMA_HOST", "http://127.0.0.1:11434")
    env["LLM_PROVIDER"] = "ollama"
    # Skip the HuggingPlace memory opt-in prompt.
    env["HUGGINGPLACE_MEMORY_ENABLED"] = "false"
    # Keep the model stable/known so the pre-seeded .gguf matches.
    env.setdefault("AGENT_ID", "default")
    # Make sure nothing forces classic mode by accident in case 1.
    env.pop("SWARMCLAW_FULLSCREEN_TUI", None)
    # A plausible TERM so crossterm/ratatui behave.
    env.setdefault("TERM", "xterm-256color")
    return env


def spawn(binary, workspace, env):
    """Fork a child running `binary` with its stdio attached to a new PTY.

    Returns (pid, master_fd).
    """
    pid, master_fd = pty.fork()
    if pid == 0:
        # Child: exec the binary. cwd = workspace so any relative files
        # (.env, swarmclaw.log, web tool sockets) stay in the temp dir.
        try:
            os.chdir(workspace)
            os.execve(binary, [binary, "--workspace", workspace], env)
        except Exception as exc:  # pragma: no cover - child error path
            os.write(2, f"exec failed: {exc}\n".encode())
            os._exit(127)
    # Parent.
    set_winsize(master_fd)
    return pid, master_fd


def read_until(master_fd, deadline, needles, accum, verbose):
    """Read from the PTY until every byte-string in `needles` is seen in the
    cumulative output, or `deadline` (absolute monotonic time) passes, or EOF.

    Appends raw bytes to the `accum` bytearray. Returns the set of needles found.
    """
    remaining = set(needles)
    while remaining and time.monotonic() < deadline:
        timeout = max(0.0, deadline - time.monotonic())
        try:
            rlist, _, _ = select.select([master_fd], [], [], min(timeout, 0.5))
        except (OSError, ValueError):
            break
        if not rlist:
            continue
        try:
            chunk = os.read(master_fd, 65536)
        except OSError as exc:
            # EIO is the normal "child closed the PTY" signal on Linux.
            if exc.errno in (errno.EIO, errno.EBADF):
                break
            raise
        if not chunk:
            break  # EOF
        accum.extend(chunk)
        for n in list(remaining):
            if n in accum:
                remaining.discard(n)
                vlog(verbose, f"    [seen] {n!r}")
    return set(needles) - remaining


def drain(master_fd, deadline, accum):
    """Keep reading whatever the child emits (e.g. the alt-screen LEAVE on
    teardown) until EOF or deadline."""
    while time.monotonic() < deadline:
        timeout = max(0.0, deadline - time.monotonic())
        try:
            rlist, _, _ = select.select([master_fd], [], [], min(timeout, 0.3))
        except (OSError, ValueError):
            break
        if not rlist:
            continue
        try:
            chunk = os.read(master_fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        accum.extend(chunk)


def wait_exit(pid, deadline):
    """Reap `pid` until it exits or `deadline`. Returns (exited, status) where
    status is the raw waitpid status (or None if it never exited)."""
    while time.monotonic() < deadline:
        try:
            wpid, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return True, 0
        if wpid == pid:
            return True, status
        time.sleep(0.05)
    return False, None


def kill_child(pid, master_fd):
    """SIGTERM then SIGKILL the child; close the PTY master."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            break
        # Give it a moment to die between signals.
        end = time.monotonic() + 2.0
        while time.monotonic() < end:
            try:
                wpid, _ = os.waitpid(pid, os.WNOHANG)
                if wpid == pid:
                    break
            except ChildProcessError:
                break
            time.sleep(0.05)
    try:
        os.close(master_fd)
    except OSError:
        pass


def describe_status(status):
    if status is None:
        return "did not exit"
    if os.WIFEXITED(status):
        return f"exited code {os.WIFEXITED(status) and os.WEXITSTATUS(status)}"
    if os.WIFSIGNALED(status):
        return f"killed by signal {os.WTERMSIG(status)}"
    return f"raw status {status}"


def run_case_fullscreen(binary, startup_timeout, exit_timeout, verbose, display):
    log("=== Case 1: DEFAULT fullscreen TUI ===")
    workspace = make_workspace()
    env = child_env(workspace, display)
    accum = bytearray()
    failures = []
    pid = master_fd = None
    try:
        pid, master_fd = spawn(binary, workspace, env)
        log(f"  spawned pid={pid}, workspace={workspace}")

        # (1)+(2) wait for alt-screen enter AND the ratatui fullscreen marker.
        deadline = time.monotonic() + startup_timeout
        found = read_until(
            master_fd, deadline, [ALT_ENTER, FULLSCREEN_MARKER], accum, verbose
        )
        entered_alt = ALT_ENTER in found
        rendered = FULLSCREEN_MARKER in found

        if entered_alt:
            log("  [PASS] entered alternate screen (ESC[?1049h)")
        else:
            failures.append("never saw alt-screen enter (ESC[?1049h)")
            log("  [FAIL] alt-screen enter not observed")
        if rendered:
            log("  [PASS] fullscreen ratatui UI rendered (' INPUT ' box title)")
        else:
            failures.append("fullscreen ' INPUT ' box marker not observed")
            log("  [FAIL] fullscreen ' INPUT ' marker not observed")

        # (3) ask it to exit; fall back to Ctrl+C.
        # Brief settle so the interactive event loop is definitely reading
        # before we type (the markers can appear a beat before read starts).
        drain(master_fd, time.monotonic() + SETTLE_SECS, accum)
        log("  sending: exit\\r")
        try:
            os.write(master_fd, b"exit\r")
        except OSError:
            pass

        exited, status = wait_exit(pid, time.monotonic() + min(exit_timeout, 8))
        if not exited:
            log("  exit not seen yet; sending Ctrl+C (\\x03)")
            try:
                os.write(master_fd, b"\x03")
            except OSError:
                pass
            # Drain so we capture the teardown bytes while waiting.
            drain(master_fd, time.monotonic() + 1.0, accum)
            exited, status = wait_exit(pid, time.monotonic() + exit_timeout)

        # Capture any remaining teardown output (alt-screen LEAVE).
        drain(master_fd, time.monotonic() + 2.0, accum)

        # (4) clean exit + terminal restored.
        if exited:
            if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0:
                log("  [PASS] process exited cleanly (status 0)")
            else:
                failures.append(f"unclean exit: {describe_status(status)}")
                log(f"  [FAIL] unclean exit: {describe_status(status)}")
        else:
            failures.append("process did not exit within timeout")
            log("  [FAIL] process did not exit within timeout")

        if ALT_LEAVE in accum:
            log("  [PASS] terminal restored (alt-screen leave ESC[?1049l)")
        else:
            failures.append("alt-screen leave (ESC[?1049l) not observed")
            log("  [FAIL] alt-screen leave not observed")

    finally:
        if pid is not None:
            # Ensure the child is gone even on assertion paths.
            try:
                os.kill(pid, 0)
                kill_child(pid, master_fd)
            except (ProcessLookupError, OSError):
                if master_fd is not None:
                    try:
                        os.close(master_fd)
                    except OSError:
                        pass
        shutil.rmtree(workspace, ignore_errors=True)

    if verbose:
        log("  --- captured output (repr, truncated) ---")
        log("  " + repr(bytes(accum)[:1200]))
    return failures


def run_case_classic(binary, startup_timeout, exit_timeout, verbose, display):
    log("=== Case 2: opt-out classic CLI (SWARMCLAW_FULLSCREEN_TUI=0) ===")
    workspace = make_workspace()
    env = child_env(workspace, display)
    env["SWARMCLAW_FULLSCREEN_TUI"] = "0"
    accum = bytearray()
    failures = []
    pid = master_fd = None
    try:
        pid, master_fd = spawn(binary, workspace, env)
        log(f"  spawned pid={pid}, workspace={workspace}")

        deadline = time.monotonic() + startup_timeout
        found = read_until(master_fd, deadline, [CLASSIC_MARKER], accum, verbose)
        if CLASSIC_MARKER in found:
            log("  [PASS] classic CLI banner rendered ('SwarmClaw CLI')")
        else:
            failures.append("classic CLI banner not observed")
            log("  [FAIL] classic CLI banner not observed")

        # The fullscreen-only box title must NOT appear in classic mode.
        if FULLSCREEN_MARKER in accum:
            failures.append("unexpected fullscreen ' INPUT ' marker in classic mode")
            log("  [FAIL] unexpected fullscreen marker present in classic mode")
        else:
            log("  [PASS] no fullscreen ' INPUT ' box marker (as expected)")

        # Brief settle so the classic crossterm event loop is reading before
        # we type (the banner is printed before read_cli_input starts).
        drain(master_fd, time.monotonic() + SETTLE_SECS, accum)
        log("  sending: exit\\r")
        try:
            os.write(master_fd, b"exit\r")
        except OSError:
            pass
        exited, status = wait_exit(pid, time.monotonic() + min(exit_timeout, 8))
        if not exited:
            log("  exit not seen yet; sending Ctrl+C (\\x03)")
            try:
                os.write(master_fd, b"\x03")
            except OSError:
                pass
            exited, status = wait_exit(pid, time.monotonic() + exit_timeout)
        drain(master_fd, time.monotonic() + 1.0, accum)

        if exited:
            if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0:
                log("  [PASS] classic CLI exited cleanly (status 0)")
            else:
                failures.append(f"classic unclean exit: {describe_status(status)}")
                log(f"  [FAIL] classic unclean exit: {describe_status(status)}")
        else:
            failures.append("classic CLI did not exit within timeout")
            log("  [FAIL] classic CLI did not exit within timeout")

    finally:
        if pid is not None:
            try:
                os.kill(pid, 0)
                kill_child(pid, master_fd)
            except (ProcessLookupError, OSError):
                if master_fd is not None:
                    try:
                        os.close(master_fd)
                    except OSError:
                        pass
        shutil.rmtree(workspace, ignore_errors=True)

    if verbose:
        log("  --- captured output (repr, truncated) ---")
        log("  " + repr(bytes(accum)[:1200]))
    return failures


def main():
    default_bin = os.environ.get(
        "SWARMCLAW_BIN", os.path.join(REPO_ROOT, "target", "debug", "swarmclaw")
    )
    parser = argparse.ArgumentParser(description="SwarmClaw fullscreen TUI PTY smoke test")
    parser.add_argument("--binary", default=default_bin, help="path to swarmclaw binary")
    parser.add_argument(
        "--startup-timeout",
        type=float,
        default=float(os.environ.get("TUI_SMOKE_STARTUP_TIMEOUT", "25")),
        help="seconds to wait for startup markers",
    )
    parser.add_argument(
        "--exit-timeout",
        type=float,
        default=float(os.environ.get("TUI_SMOKE_EXIT_TIMEOUT", "20")),
        help="seconds to wait for a clean exit",
    )
    parser.add_argument("--skip-classic", action="store_true", help="skip the opt-out case")
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        log(f"FAIL: binary not found: {binary}")
        log("Build it first: cargo build -p swarmclaw")
        return 2
    if not os.access(binary, os.X_OK):
        log(f"FAIL: binary not executable: {binary}")
        return 2

    log(f"binary: {binary}")
    log(f"startup_timeout={args.startup_timeout}s exit_timeout={args.exit_timeout}s")
    log("")

    xvfb = Xvfb(verbose=args.verbose)
    display = xvfb.start()
    log("")

    all_failures = []
    try:
        fs_failures = run_case_fullscreen(
            binary, args.startup_timeout, args.exit_timeout, args.verbose, display
        )
        all_failures += [("fullscreen", f) for f in fs_failures]
        log("")

        if not args.skip_classic:
            classic_failures = run_case_classic(
                binary, args.startup_timeout, args.exit_timeout, args.verbose, display
            )
            all_failures += [("classic", f) for f in classic_failures]
            log("")
    finally:
        xvfb.stop()

    log("=== SUMMARY ===")
    if not all_failures:
        log("PASS: SwarmClaw fullscreen TUI launched and tore down cleanly.")
        return 0
    log("FAIL:")
    for case, msg in all_failures:
        log(f"  [{case}] {msg}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
