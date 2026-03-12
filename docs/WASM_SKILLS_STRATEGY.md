# SwarmClaw Native WASM Library Strategy

**Goal:** Build a high-performance, Zero-Copy FlatBuffers compatible WASM library specifically designed for SwarmClaw, completely decoupling from legacy JSON-based/IronClaw binaries.

## 1. The Strategy

### Phase 1: The `swarmclaw-sdk` Crate (Developer Experience)
Before writing a hundred skills, we must make writing *one* skill effortless. We will build a lightweight Rust SDK (`swarmclaw-sdk`) that provides the low-level memory allocation and FlatBuffers ABI wrappers.

*   **Goal:** Skill authors should only have to write their business logic and define their FlatBuffer schemas. The SDK handles `claw_malloc`, `claw_execute`, and memory marshaling automatically.
*   **Result:** A developer writes a standard Rust function, and the SDK exposes it to the SwarmClaw host via the required `wasm32-wasi` interface.

### Phase 2: The "Standard Library" (Seeding the Ecosystem)
We will create a `skills-library/` directory containing independent Cargo projects compiled to `wasm32-wasi`. These will serve as the foundational tools for the LLM agent.

*   **Initial Skills:**
    *   `http-fetch.wasm`: Generic REST client (utilizing host capabilities).
    *   `json-jq.wasm`: JSON slicing/filtering within the sandbox to save prompt context.
    *   `github-api.wasm`: Agentic PR reviews and code fetching.
    *   `crypto-hash.wasm`: Verifying file checksums locally.

### Phase 3: The "ClawHub" Registry (Distribution)
Instead of a heavy backend, we will use an OCI (Open Container Initiative) Registry or GitHub Releases to distribute compiled `.wasm` files.
*   `ClawHubSkill` inside SwarmClaw will fetch a static `registry.json` index and download raw `.wasm` files directly into the agent's `/workspace/skills/` directory on demand.

### Phase 4: Multi-Language Support
Once the Rust FlatBuffers ABI is stable, we will map it to a **TypeScript SDK** (via Javy/Extism) and a **Go SDK**, allowing web developers to write native-speed skills for SwarmClaw.

---

## 2. The Implementation Scope: Core vs. Long Tail

To achieve full feature parity with the OpenClaw/ClawHub ecosystem (curated in `awesome-openclaw-skills`), we are dividing the workload into two distinct tracks: the **IronClaw Core** and the **Long Tail**.

### Track A: The "IronClaw Core" (~15 Skills)
These are the foundational orchestration, infrastructure, and security skills that IronClaw natively supports. Because these skills require deep host integration and strict capability bounding, we will manually write them in **Rust** using our `swarmclaw-sdk` and compile them to `wasm32-wasi`.

**The Core Roadmap:**
- [ ] **`agent-browser`**: Fast Rust-based headless browser automation.
- [ ] **`agent-builder`**: End-to-end toolchain for building new SwarmClaw agents.
- [ ] **`agent-step-sequencer`**: Multi-step scheduler for handling long-running or delayed agent requests.
- [ ] **`git-manager`**: Direct handling of PRs, issues, and commits via the `gh` CLI.
- [ ] **`clauditor`**: Tamper-resistant audit watchdog for logging and monitoring agent actions.
- [ ] **`sql-client`**: Safe, sandboxed database connectors (Postgres/SQLite/MySQL).
- [ ] **`search-searxng`**: Private, self-hosted web search integration.
- [ ] **`rss-reader`**: Real-time parsing and summarization of news/dev feeds.
- [ ] **`wikipedia-arxiv`**: Factual lookup tools for research and technical documentation.
- [ ] **`python-repl`**: Sandboxed Python interpreter (Pyodide) for data science tasks.

### Track B: The "Long Tail" (5,000+ Community Skills)
The broader OpenClaw ecosystem contains thousands of simple API wrappers (e.g., fetching weather, sending Slack messages, purchasing domains via `agentns`).

**The Strategy:** We will **not** rewrite these manually in Rust. Instead, once Phase 4 (Multi-Language Support) is complete, we will use a **TypeScript to WASM Transpiler** (like Javy). This will allow us to ingest the existing 5,000+ JavaScript/Python skills from the OpenClaw community, automatically compile them into our Zero-Copy FlatBuffers WASM format, and run them natively inside SwarmClaw's secure sandbox.

---

*Note: The goal is to provide a `swarmclaw-sdk` that makes porting these 5,000+ skills as simple as wrapping the original logic in our FlatBuffers ABI.*

---

*Note: This SDK and the resulting `.wasm` binaries are designed to eventually be spun out into their own repository (e.g., `huggingplace/swarmclaw-sdk`) to foster community development.*