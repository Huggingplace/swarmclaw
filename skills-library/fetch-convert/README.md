# Fetch Convert Skill

`skill-fetch-convert` is an exportable WASM skill that proxies page fetch and readable-text extraction through SwarmClaw's local host-owned web tools service.

Architecture:

- Raw HTTP fetches, HTML parsing, and optional headless browser fallback stay on the SwarmClaw host.
- `skill_fetch_convert.wasm` is intentionally thin: it calls the local MCP endpoint over the host HTTP bridge and does not embed its own crawler stack.
- The existing native browser skill remains available separately. This skill adds a cleaner fetch-to-text path.

Build:

```bash
cargo build -p skill-fetch-convert --target wasm32-wasip1 --release
```

Install into a workspace:

```bash
mkdir -p /path/to/workspace/skills
cp target/wasm32-wasip1/release/skill_fetch_convert.wasm /path/to/workspace/skills/
```

Optional local AOT cache:

```bash
cargo run -p swarmclaw -- repackage --input /path/to/workspace/skills/skill_fetch_convert.wasm
```

Relevant host configuration:

- Optional: `SWARMCLAW_WEB_TOOLS_BIND_ADDR`
- Optional: `SWARMCLAW_WEB_TOOLS_BASE_URL`
- Optional: `SWARMCLAW_WEB_TOOLS_TIMEOUT_SECS`
- Optional: `SWARMCLAW_WEB_TOOLS_USER_AGENT`
- Optional for JS-heavy fallback: enable the `headless_chrome` feature in the host build

Runtime behavior:

- SwarmClaw starts the local web tools service on boot.
- If `skill_fetch_convert.wasm` is present in `workspace/skills`, SwarmClaw loads that exportable skill and skips native MCP registration for `fetch_convert` to avoid duplicate tool names.
- The skill defaults to `http://127.0.0.1:4419/mcp/fetch`, but individual tool calls may override the endpoint with `__mcp_url`, `mcp_url`, `__service_url`, or `service_url`.
