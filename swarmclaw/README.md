# SwarmClaw 🦀🤖

<div align="center">

[![Deploy to Mothership](https://img.shields.io/badge/Deploy%20to-Mothership-6A0dad?style=for-the-badge&logo=rocket)](https://mothershipdeploy.com/?repo=github.com/huggingplace/openclaw-rs)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen?style=flat-square)](https://github.com/huggingplace/openclaw-rs/actions)
[![License](https://img.shields.io/badge/license-MIT%2FApache-blue?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.81%2B-orange?style=flat-square)](https://www.rust-lang.org)

**The High-Performance, Model-Native AI Agent Runtime.**

*10x Faster Startup • 10x Leaner Memory • Zero-Copy Serialization*

</div>

---

**SwarmClaw** is a next-generation AI agent runtime built for scale. Inspired by the flexibility of [OpenClaw](https://open-claw.org) but re-engineered from the ground up in Rust, it is designed to run anywhere—from edge devices to massive serverless fleets—with minimal overhead.

## 🚀 Why SwarmClaw?

Traditional Node.js/Python agents are heavy. SwarmClaw is built for the **Mothership Architecture**, treating the agent as a lightweight "Brain" that offloads heavy lifting to specialized microservices.

| Metric | Traditional Agent (Node.js) | SwarmClaw (Native) | Impact |
| :--- | :--- | :--- | :--- |
| **Startup Time** | ~1.5s - 3s | **~10ms** | Instant cold starts for serverless/edge. |
| **Idle RAM** | ~150MB | **~15MB** | Run 10x more agents per server. |
| **Communication** | JSON (Parsing overhead) | **FlatBuffers** | Zero-copy access to agent state. |
| **Security** | `npm` dependency hell | **WASM Sandbox** | Run untrusted skills safely. |

## ✨ Key Features

*   **🧠 Model-Native:** Built-in OpenAI/LLM client with native function calling support.
*   **⚡ FlatBuffers State:** Internal memory and IPC use FlatBuffers for O(1) access speeds and zero-copy serialization.
*   **🧩 WASM Extensibility:** Write skills in **Rust, Go, C++, or TypeScript (via Javy)** and load them dynamically. The `wasmtime` runtime ensures total isolation.
*   **🛡️ Native Skills:**
    *   **FileSystem:** Secure read/write operations within a workspace.
    *   **Shell:** Execute system commands (sandboxed).
    *   **Browser:** Headless Chrome integration for web reading (feature-gated).
*   **☁️ Mothership Ready:** One-click deployment to the Mothership Platform with granular resource scaling (CPU/RAM/GPU).

## 🛠️ Installation

### Prerequisites
*   Rust 1.81+ (`rustup update`)
*   `flatc` (FlatBuffers Compiler)

### Building from Source
```bash
cd swarmclaw_core
cargo build --release -p swarmclaw
```

## 🎮 Usage

### Interactive REPL
Chat with the agent directly in your terminal.

```bash
export LLM_PROVIDER=openai
export OPENAI_API_KEY=sk-your-key...
cargo run -p swarmclaw -- run
```

### Provider Selection
SwarmClaw supports `openai`, `groq`, `grok`, `anthropic`, `gemini`, and `ollama`.
Provider capabilities are enforced explicitly by the runtime, so adapters that do not yet support local tool calling run in text-only mode instead of silently pretending tools executed.

```bash
export LLM_PROVIDER=groq
export GROQ_API_KEY=your-groq-key
cargo run -p swarmclaw -- run

export LLM_PROVIDER=grok
export XAI_API_KEY=your-xai-key
cargo run -p swarmclaw -- run
```

### Health Check
Use this for deployment probes or non-interactive sanity checks.

```bash
cargo run -p swarmclaw -- status
```

### Session Inspection
Inspect persisted session summaries and recent transcript history from the local SQLite store.

```bash
cargo run -p swarmclaw -- sessions --limit 20
cargo run -p swarmclaw -- history --session slack-C123456-thread-1712345678-1234 --limit 50
cargo run -p swarmclaw -- outbox --status failed --limit 50
```

### Webhook Channels
SwarmClaw can run webhook listeners for Slack, Discord, Telegram, and WhatsApp via Twilio alongside the CLI.

```bash
export SLACK_BOT_TOKEN=xoxb-your-bot-token
export SLACK_SIGNING_SECRET=your-signing-secret
export SLACK_WEBHOOK_PORT=8083

export DISCORD_PUBLIC_KEY=your-discord-public-key
export TELEGRAM_TOKEN=your-telegram-bot-token
export TWILIO_ACCOUNT_SID=AC...
export TWILIO_AUTH_TOKEN=your-twilio-auth-token
export WHATSAPP_WEBHOOK_URL=https://your-public-host/twilio/whatsapp

cargo run -p swarmclaw -- run
```

Slack ingress uses the Events API webhook path at `/slack/events`, WhatsApp uses the Twilio webhook path at `/twilio/whatsapp`, and all webhook channels reply through the same persisted session + outbox runtime path.

### Admin API
Set `SWARMCLAW_ADMIN_PORT` to expose read-only JSON endpoints for session and outbox inspection while the agent is running.

```bash
export SWARMCLAW_ADMIN_PORT=8787
cargo run -p swarmclaw -- run

curl http://127.0.0.1:8787/admin/health
curl http://127.0.0.1:8787/admin/sessions?limit=20
curl http://127.0.0.1:8787/admin/outbox?status=failed&limit=20
```

### Dynamic Skills (WASM)
SwarmClaw automatically loads any `.wasm` files found in your `workspace/skills` directory.

**1. Create a Skill (Rust):**
```rust
use anyhow::Result;
use serde_json::Value;
use swarmclaw_sdk::{SwarmClawSkill, export_execute, export_manifest};

struct MySkill;

impl SwarmClawSkill for MySkill {
    fn name(&self) -> &str { "my_skill" }
    fn description(&self) -> &str { "Example skill" }
    fn execute(&self, _args: Value) -> Result<String> { Ok("ok".to_string()) }
}

#[no_mangle]
pub extern "C" fn claw_get_manifest() -> i64 {
    export_manifest(&MySkill)
}

#[no_mangle]
pub extern "C" fn claw_execute(ptr: *const u8, len: usize) -> i64 {
    export_execute(&MySkill, ptr, len)
}
```

**2. Compile & Optimize:**
```bash
cargo build --target wasm32-wasip1 --release
# Optional: Pre-compile for faster startup
cargo run -p swarmclaw -- repackage --input target/wasm32-wasip1/release/my_skill.wasm
```

## 🏗️ Architecture

SwarmClaw follows a **"Brain vs. Body"** separation of concerns:

1.  **The Brain (This Repo):** Holds the conversation state, decision logic, and tool dispatch. It is kept as small as possible.
2.  **The Body (Services):** Heavy tasks (Browser rendering, Vector Search, Media Transcoding) are offloaded to shared Mothership services via gRPC.

> **Note:** For standalone usage, we provide "Monolithic Parity" features (`--features "headless_chrome serenity image"`) that compile these capabilities directly into the binary.

See [OFFLOAD_STRATEGY.md](docs/OFFLOAD_STRATEGY.md) for the detailed architectural vision.

## 🚢 Deployment

**Recommended:** Deploy to [Mothership Deploy](https://mothershipdeploy.com) for managed scaling, persistent storage, and zero-config networking.

### Mothership Configuration (`mothership.yaml`)
```yaml
resources:
  cpu: 2
  memory_mb: 4096
scaling:
  mode: granular
  gpu_options: ["nvidia-t4"]
```

## 🗺️ Roadmap

- [x] **Core Runtime:** FlatBuffers state, LLM loop.
- [x] **Native Skills:** Shell, FileSystem.
- [x] **WASM Runtime:** Dynamic skill loading.
- [x] **Parity:** Browser & Chat integrations.
- [ ] **Supply Cloud:** Marketplace for renting private compute nodes.
- [ ] **Automated Migration:** JS-to-Rust skill transpiler.

## 🤝 Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to build skills or improve the core runtime.

## 📄 License

Dual-licensed under MIT and Apache 2.0.
