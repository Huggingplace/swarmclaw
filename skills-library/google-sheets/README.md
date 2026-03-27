# Google Sheets Skill

`skill-google-sheets` is an exportable WASM skill that proxies Google Sheets tool calls through SwarmClaw's host-owned local Google Workspace service.

Architecture:

- Google OAuth, refresh tokens, picker token minting, and sheet bindings live on the SwarmClaw host.
- The local SwarmClaw Google Workspace service stores encrypted refresh tokens in a SeaORM-backed SQLite database under the workspace's `.swarmclaw/` directory.
- `skill_google_sheets.wasm` is intentionally thin: it uses the host HTTP capability to call the local MCP endpoint and never owns long-lived Google credentials.

Build:

```bash
cargo build -p skill-google-sheets --target wasm32-wasip1 --release
```

Install into a workspace:

```bash
mkdir -p /path/to/workspace/skills
cp target/wasm32-wasip1/release/skill_google_sheets.wasm /path/to/workspace/skills/
```

Optional local AOT cache:

```bash
cargo run -p swarmclaw -- repackage --input /path/to/workspace/skills/skill_google_sheets.wasm
```

Required host configuration:

- `SWARMCLAW_GOOGLE_CLIENT_ID`
- `SWARMCLAW_GOOGLE_CLIENT_SECRET`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_BIND_ADDR`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_BASE_URL`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_REDIRECT_URI`
- Optional: `SWARMCLAW_GOOGLE_PICKER_API_KEY`
- Optional: `SWARMCLAW_GOOGLE_PICKER_APP_ID`

Runtime behavior:

- SwarmClaw starts the local Google Workspace service on boot.
- If `skill_google_sheets.wasm` is present in `workspace/skills`, SwarmClaw loads that exportable skill and skips the native Google Sheets MCP registration to avoid duplicate tool names.
- The skill defaults to `http://127.0.0.1:4418/mcp`, but individual tool calls may override the endpoint with `__mcp_url`, `mcp_url`, `__service_url`, or `service_url`.
