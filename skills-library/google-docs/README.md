# Google Docs Skill

`skill-google-docs` is an exportable WASM skill that proxies Google Docs tool calls through SwarmClaw's host-owned local Google Workspace service.

Architecture:

- Google OAuth, refresh tokens, and document operations live on the SwarmClaw host.
- The WASM skill is intentionally thin: it uses the host HTTP capability to call the local MCP endpoint and never owns long-lived Google credentials.
- This skill is aimed at copy workflows such as drafting ad copy, outreach templates, and controlled content revisions inside Google Docs.

Build:

```bash
cargo build -p skill-google-docs --target wasm32-wasip1 --release
```

Install into a workspace:

```bash
mkdir -p /path/to/workspace/skills
cp target/wasm32-wasip1/release/skill_google_docs.wasm /path/to/workspace/skills/
```

Optional local AOT cache:

```bash
cargo run -p swarmclaw -- repackage --input /path/to/workspace/skills/skill_google_docs.wasm
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

- The host now requests Google Docs scope by default during the local Google connect flow.
- Existing Google refresh tokens need a reconnect to pick up newly requested scopes.
- Full Gmail access is opt-in and only requested when `SWARMCLAW_GOOGLE_ENABLE_FULL_GMAIL_ACCESS=true`.
- The Google Cloud project backing the OAuth client must have the Google Docs API enabled before Docs tool calls will succeed.

Expected host runtime behavior:

- SwarmClaw starts the local Google Workspace service on boot.
- The local MCP service should expose Google Docs tools such as `create_google_doc`, `get_google_doc_content`, `append_google_doc_text`, `insert_google_doc_image`, `share_google_doc`, and `replace_google_doc_text`.
- The skill defaults to `http://127.0.0.1:4418/mcp`, but individual tool calls may override the endpoint with `__mcp_url`, `mcp_url`, `__service_url`, or `service_url`.
