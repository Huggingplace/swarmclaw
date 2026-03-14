# SwarmClaw Implementation Strategy: Next-Gen Features

This document outlines the architectural approach for porting the most powerful features from the original OpenClaw project into our high-performance, Rust-native `huggingplace-swarmclaw` agent. 

Our core philosophy remains: **Zero Bloat, Webhook-First, and Mothership-Integrated.**

---

## 1. Secret Redaction & Security Hardening
**The Goal:** Prevent the agent from accidentally leaking API keys, passwords, or Mothership tokens into chat platforms (Discord/Telegram), local SQLite logs, or the centralized Postgres sync database.

**The Strategy (Zero-Bloat Regex Middleware):**
Instead of relying on heavy third-party scrubbing libraries, we will implement a lightweight, pre-compiled Regex interception layer directly inside our `sync_engine.rs` and `outbox.rs`.

*   **Implementation:** 
    *   Create a `security::redactor` module.
    *   Initialize a static `once_cell::Lazy<RegexSet>` containing common secret patterns (e.g., `sk-[a-zA-Z0-9]{32,}`, `xoxb-[0-9]{11,}`, `mship_[a-f0-9-]{36}`).
    *   Before `enqueue_message()` writes to SQLite, or before `sync_to_postgres()` fires, the payload is passed through the redactor.
    *   **Result:** `{"text": "My AWS key is AKIAIOSFODNN7EXAMPLE"}` becomes `{"text": "My AWS key is [REDACTED_AWS_KEY]"}`.

---

## 2. Real-Time Streaming Replies
**The Goal:** Deliver responses word-by-word to Discord and Telegram, drastically reducing perceived latency for the user.

**The Strategy (Async Chunks + Rate-Limited PATCH):**
Because we abandoned heavy WebSockets (`serenity`/`teloxide`) for pure Webhooks + `reqwest`, streaming requires a specific HTTP pattern.

*   **Implementation:**
    1.  **The Initial ACK:** When the Webhook hits `axum`, we immediately return an HTTP 200 OK (Telegram) or a `type: 5` "Thinking" response (Discord) to satisfy the 3-second timeout.
    2.  **LLM Stream:** We switch our `llm::openai` client to use `stream: true`. It will return an `async_stream` of text chunks.
    3.  **The Debouncer:** Chat platforms will rate-limit you if you make an HTTP request for every single word. We will introduce a Tokio `Interval` ticker (e.g., 1000ms).
    4.  **The PATCH Loop:** As words arrive from the LLM, we append them to a local buffer. Every 1000ms, if the buffer has changed, we use `reqwest` to `PATCH` the original message on Discord/Telegram with the current buffer state.

---

## 3. Nested Sub-Agents (Agent Swarms via Mothership Fleet)
**The Goal:** Allow a primary orchestrator agent to spawn specialized sub-agents to handle complex, parallel tasks, gathering their results later.

**The Strategy (Mothership gRPC Integration):**
Original OpenClaw runs sub-agents as local child processes. We have a massive advantage: **Mothership Fleet**. We can spawn sub-agents on entirely different cloud nodes.

*   **Implementation:**
    *   Create a native skill called `DelegateTaskTool`.
    *   When the LLM decides a task is too complex (e.g., "Scrape these 50 URLs"), it invokes `DelegateTaskTool`.
    *   This tool constructs a `FleetJobRequest` and connects to the `mothership-engine` via gRPC (`submit_fleet_job`).
    *   Mothership provisions a temporary Spot VM, deploys a clone of the `swarmclaw` container, passes the sub-task as an env var, and executes it.
    *   The primary agent uses the SQLite Outbox to "pause" its state. When the sub-agent finishes, it fires a Webhook back to the primary agent's `axum` server with the results, resuming the conversation.

### The Wasmtime + Spot Instance Synergy
Because Mothership leverages Spot Instances (which are 90% cheaper but can be killed with 30-second notice), SwarmClaw's Wasmtime architecture is critical for maximizing ROI:
*   **Instant Cold Starts:** When a replacement Spot VM boots, Wasmtime mmaps the `.cwasm` AOT cache, bringing the new sub-agent online and executing skills in milliseconds, completely bypassing the 30-60 second boot times of traditional Docker/Python containers.
*   **Deterministic Epoch Interruption:** When the cloud provider signals a termination, Wasmtime's "Epoch Interruption" forcefully and safely halts the WASM skill at the next CPU instruction. This allows the SwarmClaw agent to cleanly save its state to the `LocalStore` and exit gracefully before the VM is destroyed, preventing corrupted artifacts.
*   **Extreme Density:** Because Wasmtime uses Instance Pooling and shares compiled code across sandboxes, Mothership can safely pack hundreds of concurrent SwarmClaw agents onto a single, cheap 2-core Spot Instance.

---

## 4. Interactive UI Components (Discord/Slack v2)
**The Goal:** Allow the agent to send interactive buttons or dropdown menus instead of relying purely on natural language processing.

**The Strategy (Webhook Payload Injection):**
*   **Implementation:**
    *   Extend the `OutboxMessage` struct to optionally include a `ui_components` JSON payload.
    *   When the agent wants to ask a multiple-choice question, it returns a structured JSON tool call.
    *   Our `reqwest` outbox worker maps this to Discord's specific `components` array syntax during the `POST`/`PATCH` request.
    *   When a user clicks a button, Discord sends a new Webhook with type `MESSAGE_COMPONENT` (Type 3). Our `axum` router intercepts this, extracts the `custom_id` of the button, and feeds it straight back into the agent's LLM context as a user reply.

---

## 5. Proactive Automation (Heartbeats & Cron)
**The Goal:** The agent wakes up autonomously to check things, rather than only responding to user messages.

**The Strategy (Tokio Cron Worker):**
*   **Implementation:**
    *   Introduce a `cron_worker.rs` that spawns alongside the `axum` server and `outbox_worker`.
    *   It reads a `schedule.yaml` or checks the SQLite DB for registered tasks.
    *   Using `tokio::time::interval`, it wakes up, injects a synthetic "System Prompt" into the agent's memory (e.g., "SYSTEM: It is 9 AM. Check the server logs."), and triggers an LLM generation cycle.
    *   If the agent finds something interesting, it generates a response, which drops into the SQLite Outbox and is seamlessly sent to Telegram/Discord via our standard Webhook flow.
