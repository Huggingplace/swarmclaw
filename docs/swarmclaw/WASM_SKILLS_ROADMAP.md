# SwarmClaw WASM Skills Roadmap

Based on the rapidly expanding Model Context Protocol (MCP), AutoGPT, and OpenClaw ecosystems, here is a comprehensive list of standard AI agent skills that we need to convert into securely sandboxed WebAssembly (WASM) modules for the SwarmClaw engine.

By porting these to WASM, we guarantee that when a SwarmClaw agent executes a community-provided skill, it cannot break out of its cloud worktree or steal secrets.

## Phase 1: Core "DevTools" (High Priority)
These are the foundational skills required to make SwarmClaw a competent software engineer, heavily inspired by Aider and Claude Code.

*   [ ] **`git-manager.wasm`**: Tools for `status`, `diff`, `commit`, `checkout`, and `push`. (Must be strictly sandboxed to the `MOTHERSHIP_WORKTREE_PATH`).
*   [ ] **`fs-extended.wasm`**: Advanced file system operations beyond standard read/write (e.g., fuzzy searching, globbing, regex replace across multiple files).
*   [ ] **`bash-executor.wasm`**: A restricted shell executor that only allows non-interactive commands (like `npm test` or `cargo check`) and streams the output back as a JSON artifact.
*   [ ] **`lsp-bridge.wasm`**: Connects to local Language Server Protocols (LSP) to allow the agent to run "Go to Definition" or "Find References" before editing code.

## Phase 2: Web & Automation (The "Fox" Swarm)
Skills required for agents to interact with the outside internet, inspired by OpenClaw and Puppeteer MCPs.

*   [ ] **`fetch-convert.wasm`**: Safely fetches a URL and converts the HTML into clean Markdown for the LLM context window.
*   [ ] **`browser-automation.wasm`**: A bridge to the `00fox` backend. Allows the agent to send click, type, and scroll commands to a headless browser and receive DOM snapshots back.
*   [ ] **`github-api.wasm`**: Manages PRs, reads issues, and leaves comments. (Requires integration with `MothershipFleetStore` to securely inject the user's `GITHUB_TOKEN`).

## Phase 3: Productivity & Data (Enterprise Tier)
Skills required to make SwarmClaw useful for data scientists and project managers.

*   [ ] **`sqlite-query.wasm`**: Allows the agent to run read-only `SELECT` queries against local `.db` files within its worktree.
*   [ ] **`slack-bridge.wasm`**: Read threads and post updates to Slack channels.
*   [ ] **`linear-manager.wasm`**: Transition issue states and read project requirements from Linear.
*   [ ] **`google-workspace.wasm`**: Read Calendar events and draft Google Docs.

## Phase 4: Advanced Reasoning
Skills inspired by the official Anthropic MCP reference servers to enhance the LLM's cognitive loop.

*   [ ] **`sequential-thinking.wasm`**: Provides a structured scratchpad for the agent to write down multi-step thoughts and backtrack if it makes a mistake.
*   [ ] **`memory-graph.wasm`**: A specialized tool that allows the agent to read/write persistent facts to a local SQLite knowledge graph, enabling long-term memory across different Swarm jobs.
