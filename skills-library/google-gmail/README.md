# Google Gmail Skill

`skill-google-gmail` is an exportable WASM skill that proxies Gmail tool calls through SwarmClaw's host-owned local Google Workspace service.

Architecture:

- Google OAuth, refresh tokens, and Gmail API operations live on the SwarmClaw host.
- The WASM skill is intentionally thin: it uses the host HTTP capability to call the local MCP endpoint and never owns long-lived Google credentials.
- This skill is aimed at mail workflows such as inbox search, thread inspection, drafting outreach, and sending controlled messages from the connected Gmail account.

Build:

```bash
cargo build -p skill-google-gmail --target wasm32-wasip1 --release
```

Install into a workspace:

```bash
mkdir -p /path/to/workspace/skills
cp target/wasm32-wasip1/release/skill_google_gmail.wasm /path/to/workspace/skills/
```

Optional local AOT cache:

```bash
cargo run -p swarmclaw -- repackage --input /path/to/workspace/skills/skill_google_gmail.wasm
```

Required host configuration:

- `SWARMCLAW_GOOGLE_CLIENT_ID`
- `SWARMCLAW_GOOGLE_CLIENT_SECRET`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_BIND_ADDR`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_BASE_URL`
- Optional: `SWARMCLAW_GOOGLE_WORKSPACE_REDIRECT_URI`
- Optional: `SWARMCLAW_GOOGLE_EXTRA_SCOPES`
- Optional: `SWARMCLAW_GOOGLE_ENABLE_FULL_GMAIL_ACCESS=true`

Runtime notes:

- Gmail API access requires a reconnect if the existing Google grant was created before Gmail scopes were requested.
- Full Gmail access is opt-in and only requested when `SWARMCLAW_GOOGLE_ENABLE_FULL_GMAIL_ACCESS=true`.
- The Google Cloud project backing the OAuth client must have the Gmail API enabled before Gmail tool calls will succeed.

Expected host runtime behavior:

- SwarmClaw starts the local Google Workspace service on boot.
- The local MCP service should expose Gmail tools such as `search_gmail`, `list_gmail_threads`, `get_gmail_message`, `send_gmail_message`, and `draft_gmail_message`.
- The skill defaults to `http://127.0.0.1:4418/mcp`, but individual tool calls may override the endpoint with `__mcp_url`, `mcp_url`, `__service_url`, or `service_url`.
