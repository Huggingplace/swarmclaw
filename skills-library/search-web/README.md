# Search Web Skill

`skill-search-web` is an exportable WASM skill that proxies web search calls through SwarmClaw's local host-owned web tools service.

Architecture:

- Provider credentials and outbound search requests stay on the SwarmClaw host.
- The current host service supports Google Programmable Search JSON API, Brave Search API, and SearXNG.
- `skill_search_web.wasm` is intentionally thin: it forwards tool calls to the local MCP endpoint over the host HTTP bridge.

Build:

```bash
cargo build -p skill-search-web --target wasm32-wasip1 --release
```

Install into a workspace:

```bash
mkdir -p /path/to/workspace/skills
cp target/wasm32-wasip1/release/skill_search_web.wasm /path/to/workspace/skills/
```

Optional local AOT cache:

```bash
cargo run -p swarmclaw -- repackage --input /path/to/workspace/skills/skill_search_web.wasm
```

Relevant host configuration:

- Optional: `SWARMCLAW_WEB_TOOLS_BIND_ADDR`
- Optional: `SWARMCLAW_WEB_TOOLS_BASE_URL`
- Optional: `SWARMCLAW_WEB_TOOLS_TIMEOUT_SECS`
- Optional: `SWARMCLAW_WEB_TOOLS_USER_AGENT`
- Google: `SWARMCLAW_WEB_SEARCH_GOOGLE_API_KEY`
- Google: `SWARMCLAW_WEB_SEARCH_GOOGLE_CSE_ID`
- Brave: `SWARMCLAW_WEB_SEARCH_BRAVE_API_KEY`
- SearXNG: `SWARMCLAW_WEB_SEARCH_SEARXNG_BASE_URL`
- Optional: `SWARMCLAW_WEB_SEARCH_DEFAULT_PROVIDER`

Runtime behavior:

- SwarmClaw starts the local web tools service on boot.
- If `skill_search_web.wasm` is present in `workspace/skills`, SwarmClaw loads that exportable skill and skips native MCP registration for `search_web` to avoid duplicate tool names.
- The skill defaults to `http://127.0.0.1:4419/mcp/search`, but individual tool calls may override the endpoint with `__mcp_url`, `mcp_url`, `__service_url`, or `service_url`.
