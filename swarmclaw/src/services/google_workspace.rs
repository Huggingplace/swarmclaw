use crate::services::google_workspace_store::{
    GoogleWorkspaceStore, StoredGoogleAccount, StoredGoogleSheetBinding,
};
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{Html, Json, Redirect},
    routing::{delete, get, post},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeSet, net::SocketAddr, path::Path, sync::Arc};
use tracing::{info, warn};
use uuid::Uuid;

const GOOGLE_WORKSPACE_UI_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>SwarmClaw Google Sheets</title>
    <style>
      :root {
        color-scheme: light;
        --bg: #f4efe6;
        --panel: #fffaf1;
        --line: #d8ccb6;
        --ink: #1f1f18;
        --muted: #6d695f;
        --accent: #165dff;
        --accent-2: #0b8a5a;
        --danger: #b3261e;
      }
      * { box-sizing: border-box; }
      body {
        margin: 0;
        font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        color: var(--ink);
        background:
          radial-gradient(circle at top left, rgba(22, 93, 255, 0.12), transparent 30%),
          radial-gradient(circle at top right, rgba(11, 138, 90, 0.12), transparent 30%),
          var(--bg);
      }
      .shell {
        max-width: 1100px;
        margin: 0 auto;
        padding: 32px 20px 64px;
      }
      .hero {
        display: grid;
        gap: 20px;
        grid-template-columns: 1.2fr 0.8fr;
        align-items: start;
      }
      .card {
        background: rgba(255, 250, 241, 0.86);
        border: 1px solid var(--line);
        border-radius: 22px;
        padding: 22px;
        box-shadow: 0 18px 44px rgba(31, 31, 24, 0.06);
        backdrop-filter: blur(14px);
      }
      h1 {
        margin: 0 0 12px;
        font-size: clamp(2rem, 4vw, 3.5rem);
        line-height: 0.95;
        letter-spacing: -0.04em;
      }
      h2 {
        margin: 0 0 14px;
        font-size: 1.05rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
      }
      p {
        margin: 0;
        color: var(--muted);
        line-height: 1.5;
      }
      .actions,
      .grid,
      .bindings {
        display: grid;
        gap: 14px;
      }
      .actions {
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
        margin-top: 20px;
      }
      .grid {
        grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        margin-top: 20px;
      }
      button,
      input,
      textarea {
        font: inherit;
      }
      button {
        border: 0;
        border-radius: 14px;
        padding: 12px 16px;
        background: var(--accent);
        color: white;
        cursor: pointer;
        font-weight: 600;
      }
      button.secondary {
        background: transparent;
        color: var(--ink);
        border: 1px solid var(--line);
      }
      button.ghost {
        background: rgba(22, 93, 255, 0.08);
        color: var(--accent);
      }
      button.danger {
        background: rgba(179, 38, 30, 0.1);
        color: var(--danger);
      }
      button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      label {
        display: grid;
        gap: 8px;
        font-size: 0.92rem;
      }
      input,
      textarea {
        width: 100%;
        border: 1px solid var(--line);
        border-radius: 14px;
        padding: 12px 14px;
        background: rgba(255, 255, 255, 0.72);
      }
      textarea {
        min-height: 110px;
        resize: vertical;
      }
      .pill {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        border-radius: 999px;
        padding: 8px 12px;
        background: rgba(11, 138, 90, 0.1);
        color: var(--accent-2);
        font-size: 0.85rem;
        font-weight: 600;
      }
      .pill.warn {
        background: rgba(179, 38, 30, 0.1);
        color: var(--danger);
      }
      .binding {
        border: 1px solid var(--line);
        border-radius: 18px;
        padding: 16px;
        background: rgba(255, 255, 255, 0.55);
      }
      .binding-head {
        display: flex;
        justify-content: space-between;
        gap: 12px;
        align-items: start;
      }
      .mono {
        font-family: ui-monospace, SFMono-Regular, SFMono-Regular, Menlo, monospace;
      }
      .muted {
        color: var(--muted);
      }
      .hidden {
        display: none;
      }
      .status {
        margin-top: 18px;
        padding: 14px 16px;
        border-radius: 16px;
        border: 1px solid var(--line);
        background: rgba(255, 255, 255, 0.5);
        white-space: pre-wrap;
      }
      @media (max-width: 820px) {
        .hero {
          grid-template-columns: 1fr;
        }
      }
    </style>
    <script src="https://apis.google.com/js/api.js" async defer></script>
  </head>
  <body>
    <main class="shell">
      <section class="hero">
        <div class="card">
          <div class="pill" id="connection-pill">Loading state…</div>
          <h1>Google Sheets<br />for SwarmClaw</h1>
          <p>
            Connect your Google account locally, choose the exact spreadsheets the agent may write,
            and expose them through a localhost MCP bridge.
          </p>
          <div class="actions">
            <button id="connect-button">Connect Google</button>
            <button id="picker-button" class="ghost" disabled>Choose Spreadsheet</button>
            <button id="refresh-button" class="secondary">Refresh State</button>
          </div>
          <div class="status" id="status-box">Waiting for local state…</div>
        </div>
        <div class="card">
          <h2>Current Account</h2>
          <div id="account-details" class="grid">
            <p class="muted">No Google Workspace account linked yet.</p>
          </div>
          <div style="margin-top: 18px;">
            <div class="pill" id="picker-pill">Picker status: unknown</div>
          </div>
        </div>
      </section>

      <section class="grid">
        <div class="card">
          <h2>Bind Spreadsheet</h2>
          <div class="grid">
            <label>
              Alias
              <input id="alias-input" placeholder="sales_leads" />
            </label>
            <label>
              Spreadsheet ID
              <input id="spreadsheet-id-input" placeholder="Paste a spreadsheet ID or choose from Picker" />
            </label>
            <label>
              Spreadsheet Name
              <input id="spreadsheet-title-input" placeholder="Autofilled after Picker selection" />
            </label>
            <label>
              Allowed Tabs
              <input id="allowed-tabs-input" placeholder="Sheet1, Intake, Export" />
            </label>
            <label>
              Allowed Ranges
              <textarea id="allowed-ranges-input" placeholder="One prefix per line, e.g. Sheet1!A:Z"></textarea>
            </label>
          </div>
          <div class="actions">
            <button id="bind-button">Save Binding</button>
            <button id="clear-selection-button" class="secondary">Clear Selection</button>
          </div>
        </div>
        <div class="card">
          <h2>Bound Sheets</h2>
          <div id="bindings" class="bindings">
            <p class="muted">No sheets have been bound yet.</p>
          </div>
        </div>
      </section>
    </main>

    <script>
      const statusBox = document.getElementById('status-box');
      const connectionPill = document.getElementById('connection-pill');
      const pickerPill = document.getElementById('picker-pill');
      const accountDetails = document.getElementById('account-details');
      const bindings = document.getElementById('bindings');
      const aliasInput = document.getElementById('alias-input');
      const spreadsheetIdInput = document.getElementById('spreadsheet-id-input');
      const spreadsheetTitleInput = document.getElementById('spreadsheet-title-input');
      const allowedTabsInput = document.getElementById('allowed-tabs-input');
      const allowedRangesInput = document.getElementById('allowed-ranges-input');
      const pickerButton = document.getElementById('picker-button');

      let currentState = null;
      let pickerApiLoaded = false;

      function setStatus(message) {
        statusBox.textContent = message;
      }

      function slugify(value) {
        return value
          .toLowerCase()
          .trim()
          .replace(/[^a-z0-9]+/g, '_')
          .replace(/^_+|_+$/g, '');
      }

      function parseCsv(input) {
        return input
          .split(',')
          .map(part => part.trim())
          .filter(Boolean);
      }

      function parseLines(input) {
        return input
          .split('\n')
          .map(part => part.trim())
          .filter(Boolean);
      }

      function renderState(state) {
        currentState = state;
        const connected = Boolean(state.connected);
        connectionPill.textContent = connected ? 'Google account connected' : 'Google account not connected';
        connectionPill.className = connected ? 'pill' : 'pill warn';
        pickerPill.textContent = state.picker_configured ? 'Picker ready' : 'Picker missing API key or app ID';
        pickerPill.className = state.picker_configured ? 'pill' : 'pill warn';
        pickerButton.disabled = !(connected && state.picker_configured);

        accountDetails.innerHTML = connected
          ? `
            <div class="binding">
              <div class="binding-head">
                <div>
                  <div><strong>${state.account_name || 'Google Workspace account'}</strong></div>
                  <div class="muted">${state.account_email || 'No account email available'}</div>
                </div>
                <div class="pill">Scopes granted</div>
              </div>
              <div class="muted" style="margin-top: 10px;">${state.scope || 'Unavailable'}</div>
            </div>
          `
          : '<p class="muted">No Google Workspace account linked yet.</p>';

        if (!state.bindings.length) {
          bindings.innerHTML = '<p class="muted">No sheets have been bound yet.</p>';
        } else {
          bindings.innerHTML = state.bindings.map(binding => `
            <article class="binding">
              <div class="binding-head">
                <div>
                  <div><strong>${binding.alias}</strong></div>
                  <div class="muted">${binding.spreadsheet_title}</div>
                  <div class="mono muted">${binding.spreadsheet_id}</div>
                </div>
                <button class="danger" data-binding-delete="${binding.id}">Remove</button>
              </div>
              <div class="muted" style="margin-top: 10px;">
                Tabs: ${binding.sheet_titles.join(', ') || 'unknown'}
              </div>
              <div class="muted" style="margin-top: 6px;">
                Allowed tabs: ${binding.allowed_tabs.length ? binding.allowed_tabs.join(', ') : 'all'}
              </div>
              <div class="muted" style="margin-top: 6px;">
                Allowed ranges: ${binding.allowed_ranges.length ? binding.allowed_ranges.join(', ') : 'all'}
              </div>
            </article>
          `).join('');

          document.querySelectorAll('[data-binding-delete]').forEach(button => {
            button.addEventListener('click', async () => {
              const bindingId = button.getAttribute('data-binding-delete');
              if (!bindingId) return;
              await fetch(`/api/sheets/${bindingId}`, { method: 'DELETE' });
              await refreshState();
            });
          });
        }

        setStatus(connected
          ? 'Google Workspace is connected locally. Bind spreadsheets by alias, then the MCP tools can write to them.'
          : 'Connect Google first. The auth flow and refresh token stay on this SwarmClaw host, not in the exportable skill.');
      }

      async function refreshState() {
        const response = await fetch('/api/status');
        const payload = await response.json();
        renderState(payload);
      }

      async function loadPickerApi() {
        if (pickerApiLoaded) return;
        await new Promise((resolve, reject) => {
          const tryLoad = () => {
            if (!window.gapi) {
              setTimeout(tryLoad, 100);
              return;
            }
            window.gapi.load('picker', {
              callback: () => {
                pickerApiLoaded = true;
                resolve();
              },
              onerror: () => reject(new Error('Failed to load Google Picker API')),
            });
          };
          tryLoad();
        });
      }

      async function openPicker() {
        if (!currentState?.connected) {
          setStatus('Connect Google before opening Picker.');
          return;
        }

        await loadPickerApi();
        const configRes = await fetch('/api/picker/config');
        const config = await configRes.json();
        if (!config.enabled) {
          setStatus('Picker is not configured. Set SWARMCLAW_GOOGLE_PICKER_API_KEY and SWARMCLAW_GOOGLE_PICKER_APP_ID.');
          return;
        }

        const tokenRes = await fetch('/api/picker/token', { method: 'POST' });
        const token = await tokenRes.json();
        const view = new google.picker.DocsView(google.picker.ViewId.SPREADSHEETS)
          .setIncludeFolders(false)
          .setSelectFolderEnabled(false);

        const picker = new google.picker.PickerBuilder()
          .setDeveloperKey(config.api_key)
          .setAppId(config.app_id)
          .setOAuthToken(token.access_token)
          .addView(view)
          .setCallback(data => {
            if (data.action !== google.picker.Action.PICKED || !data.docs?.length) {
              return;
            }
            const doc = data.docs[0];
            spreadsheetIdInput.value = doc.id || '';
            spreadsheetTitleInput.value = doc.name || '';
            if (!aliasInput.value.trim()) {
              aliasInput.value = slugify(doc.name || doc.id || 'google_sheet');
            }
            setStatus(`Selected spreadsheet ${doc.name || doc.id}. Review alias and constraints, then save binding.`);
          })
          .build();

        picker.setVisible(true);
      }

      document.getElementById('connect-button').addEventListener('click', () => {
        const popup = window.open('/api/connect/start', 'swarmclaw-google-connect', 'width=520,height=720');
        if (!popup) {
          setStatus('Popup blocked. Allow popups for localhost and try again.');
        }
      });

      document.getElementById('picker-button').addEventListener('click', () => {
        openPicker().catch(error => setStatus(error.message));
      });

      document.getElementById('refresh-button').addEventListener('click', () => {
        refreshState().catch(error => setStatus(error.message));
      });

      document.getElementById('clear-selection-button').addEventListener('click', () => {
        spreadsheetIdInput.value = '';
        spreadsheetTitleInput.value = '';
        aliasInput.value = '';
        allowedTabsInput.value = '';
        allowedRangesInput.value = '';
        setStatus('Binding form cleared.');
      });

      document.getElementById('bind-button').addEventListener('click', async () => {
        const alias = aliasInput.value.trim();
        const spreadsheetId = spreadsheetIdInput.value.trim();
        if (!alias || !spreadsheetId) {
          setStatus('Alias and spreadsheet ID are required.');
          return;
        }

        const payload = {
          alias,
          spreadsheet_id: spreadsheetId,
          allowed_tabs: parseCsv(allowedTabsInput.value),
          allowed_ranges: parseLines(allowedRangesInput.value),
        };

        const response = await fetch('/api/sheets', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload),
        });

        const body = await response.json();
        if (!response.ok) {
          setStatus(body.error || 'Failed to bind spreadsheet.');
          return;
        }

        spreadsheetTitleInput.value = body.spreadsheet_title || spreadsheetTitleInput.value;
        setStatus(`Saved binding ${body.alias} -> ${body.spreadsheet_title}.`);
        await refreshState();
      });

      window.addEventListener('message', async event => {
        if (event.origin !== window.location.origin) return;
        if (event.data?.type === 'swarmclaw-google-workspace-connected') {
          await refreshState();
        }
      });

      refreshState().catch(error => setStatus(error.message));
    </script>
  </body>
</html>
"#;

#[derive(Clone)]
pub struct GoogleWorkspaceService {
    bind_addr: SocketAddr,
    public_base_url: String,
    redirect_uri: String,
    state: Arc<GoogleWorkspaceState>,
}

#[derive(Clone)]
struct GoogleWorkspaceState {
    config: GoogleWorkspaceConfig,
    store: GoogleWorkspaceStore,
    http: reqwest::Client,
}

#[derive(Clone)]
struct GoogleWorkspaceConfig {
    client_id: String,
    client_secret: String,
    picker_api_key: Option<String>,
    picker_app_id: Option<String>,
    extra_scopes: Vec<String>,
    public_base_url: String,
    redirect_uri: String,
}

#[derive(Debug, Clone, Serialize)]
struct GoogleWorkspaceStatus {
    connected: bool,
    account_email: Option<String>,
    account_name: Option<String>,
    scope: Option<String>,
    picker_configured: bool,
    bindings: Vec<GoogleSheetBinding>,
}

#[derive(Debug, Clone, Serialize)]
struct GoogleSheetBinding {
    id: String,
    alias: String,
    spreadsheet_id: String,
    spreadsheet_title: String,
    sheet_titles: Vec<String>,
    allowed_tabs: Vec<String>,
    allowed_ranges: Vec<String>,
}

impl From<StoredGoogleSheetBinding> for GoogleSheetBinding {
    fn from(value: StoredGoogleSheetBinding) -> Self {
        Self {
            id: value.id,
            alias: value.alias,
            spreadsheet_id: value.spreadsheet_id,
            spreadsheet_title: value.spreadsheet_title,
            sheet_titles: value.sheet_titles,
            allowed_tabs: value.allowed_tabs,
            allowed_ranges: value.allowed_ranges,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: i64,
    scope: Option<String>,
    token_type: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfoResponse {
    email: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GoogleSpreadsheetMetadataResponse {
    #[serde(default)]
    properties: Option<GoogleSpreadsheetProperties>,
    #[serde(default)]
    sheets: Vec<GoogleSpreadsheetSheet>,
    #[serde(rename = "spreadsheetId")]
    spreadsheet_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct GoogleSpreadsheetProperties {
    title: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GoogleSpreadsheetSheet {
    #[serde(default)]
    properties: Option<GoogleSpreadsheetSheetProperties>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GoogleSpreadsheetSheetProperties {
    title: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GoogleDriveFileParentsResponse {
    #[serde(default)]
    parents: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BindGoogleSheetRequest {
    alias: String,
    spreadsheet_id: String,
    #[serde(default)]
    allowed_tabs: Vec<String>,
    #[serde(default)]
    allowed_ranges: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Value,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl GoogleWorkspaceService {
    pub async fn from_env(workspace_path: &Path) -> Result<Option<Self>> {
        let client_id = match std::env::var("SWARMCLAW_GOOGLE_CLIENT_ID") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(None),
        };
        let client_secret = std::env::var("SWARMCLAW_GOOGLE_CLIENT_SECRET").context(
            "SWARMCLAW_GOOGLE_CLIENT_SECRET is required when SWARMCLAW_GOOGLE_CLIENT_ID is set",
        )?;

        let bind_addr = std::env::var("SWARMCLAW_GOOGLE_WORKSPACE_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:4418".to_string())
            .parse::<SocketAddr>()
            .context("Failed to parse SWARMCLAW_GOOGLE_WORKSPACE_BIND_ADDR")?;
        let public_base_url = std::env::var("SWARMCLAW_GOOGLE_WORKSPACE_BASE_URL")
            .unwrap_or_else(|_| format!("http://{}", bind_addr));
        let redirect_uri =
            std::env::var("SWARMCLAW_GOOGLE_WORKSPACE_REDIRECT_URI").unwrap_or_else(|_| {
                format!(
                    "{}/api/connect/callback",
                    public_base_url.trim_end_matches('/')
                )
            });

        let store = GoogleWorkspaceStore::open(&workspace_path.join(".swarmclaw")).await?;

        let mut extra_scopes = std::env::var("SWARMCLAW_GOOGLE_EXTRA_SCOPES")
            .ok()
            .map(|value| parse_google_scope_list(&value))
            .unwrap_or_default();
        if env_var_truthy("SWARMCLAW_GOOGLE_ENABLE_FULL_GMAIL_ACCESS") {
            extra_scopes.push("https://mail.google.com/".to_string());
        }

        let config = GoogleWorkspaceConfig {
            client_id,
            client_secret,
            picker_api_key: std::env::var("SWARMCLAW_GOOGLE_PICKER_API_KEY").ok(),
            picker_app_id: std::env::var("SWARMCLAW_GOOGLE_PICKER_APP_ID").ok(),
            extra_scopes,
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
            redirect_uri,
        };

        let state = GoogleWorkspaceState {
            config: config.clone(),
            store,
            http: reqwest::Client::new(),
        };

        Ok(Some(Self {
            bind_addr,
            public_base_url: config.public_base_url.clone(),
            redirect_uri: config.redirect_uri.clone(),
            state: Arc::new(state),
        }))
    }

    pub fn ui_url(&self) -> String {
        self.public_base_url.clone()
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub fn mcp_endpoint(&self) -> String {
        format!("{}/mcp", self.public_base_url)
    }

    pub async fn start(self) -> Result<()> {
        let app = Router::new()
            .route("/", get(index))
            .route("/api/status", get(get_status))
            .route("/api/connect/start", get(start_google_connect))
            .route("/api/connect/callback", get(complete_google_connect))
            .route("/api/picker/config", get(get_picker_config))
            .route("/api/picker/token", post(create_picker_token))
            .route(
                "/api/sheets",
                get(list_google_sheets).post(bind_google_sheet),
            )
            .route("/api/sheets/{binding_id}", delete(delete_google_sheet))
            .route("/mcp", post(handle_mcp))
            .with_state(self.state.clone());

        let listener = tokio::net::TcpListener::bind(self.bind_addr)
            .await
            .with_context(|| {
                format!(
                    "Failed to bind Google Workspace service to {}",
                    self.bind_addr
                )
            })?;
        info!(
            "SwarmClaw Google Workspace service listening on {}",
            self.public_base_url
        );
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn index() -> Html<&'static str> {
    Html(GOOGLE_WORKSPACE_UI_HTML)
}

async fn get_status(
    State(state): State<Arc<GoogleWorkspaceState>>,
) -> Result<Json<GoogleWorkspaceStatus>, (StatusCode, Json<Value>)> {
    json_result(async move {
        let account = state.store.account().await?;
        let bindings = state
            .store
            .list_sheet_bindings()
            .await?
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();

        Ok(Json(GoogleWorkspaceStatus {
            connected: account.is_some(),
            account_email: account
                .as_ref()
                .and_then(|value| value.account_email.clone()),
            account_name: account
                .as_ref()
                .and_then(|value| value.account_name.clone()),
            scope: account.as_ref().map(|value| value.scope.clone()),
            picker_configured: state.config.picker_api_key.is_some()
                && state.config.picker_app_id.is_some(),
            bindings,
        }))
    })
    .await
}

async fn get_picker_config(State(state): State<Arc<GoogleWorkspaceState>>) -> Json<Value> {
    Json(json!({
        "enabled": state.config.picker_api_key.is_some() && state.config.picker_app_id.is_some(),
        "api_key": state.config.picker_api_key,
        "app_id": state.config.picker_app_id,
    }))
}

async fn list_google_sheets(
    State(state): State<Arc<GoogleWorkspaceState>>,
) -> Result<Json<Vec<GoogleSheetBinding>>, (StatusCode, Json<Value>)> {
    json_result(async move {
        Ok(Json(
            state
                .store
                .list_sheet_bindings()
                .await?
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>(),
        ))
    })
    .await
}

async fn start_google_connect(
    State(state): State<Arc<GoogleWorkspaceState>>,
) -> Result<Redirect, (StatusCode, Json<Value>)> {
    json_result(async move {
        let oauth_state = state.store.create_oauth_state().await?;
        let scopes = state.config.oauth_scopes().join(" ");

        let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
            .context("Failed to construct Google OAuth URL")?;
        url.query_pairs_mut()
            .append_pair("client_id", &state.config.client_id)
            .append_pair("redirect_uri", &state.config.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &scopes)
            .append_pair("access_type", "offline")
            .append_pair("include_granted_scopes", "true")
            .append_pair("prompt", "consent")
            .append_pair("state", &oauth_state);

        Ok(Redirect::temporary(url.as_str()))
    })
    .await
}

#[derive(Deserialize)]
struct GoogleConnectCallbackQuery {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

async fn complete_google_connect(
    State(state): State<Arc<GoogleWorkspaceState>>,
    axum::extract::Query(query): axum::extract::Query<GoogleConnectCallbackQuery>,
) -> Html<String> {
    let outcome = async {
        if !state.store.consume_oauth_state(&query.state).await? {
            bail!("OAuth state is invalid or expired");
        }

        if let Some(error) = query.error {
            bail!("Google OAuth error: {}", error);
        }

        let code = query
            .code
            .context("Google OAuth callback did not include a code")?;
        let token = exchange_google_code(&state, &code).await?;

        let mut account = state.store.account().await?.unwrap_or(StoredGoogleAccount {
            refresh_token: String::new(),
            scope: String::new(),
            account_email: None,
            account_name: None,
        });
        account.refresh_token = token
            .refresh_token
            .clone()
            .or_else(|| {
                if account.refresh_token.is_empty() {
                    None
                } else {
                    Some(account.refresh_token.clone())
                }
            })
            .context(
                "Google did not return a refresh token. Revoke the previous grant and reconnect.",
            )?;
        account.scope = token.scope.clone().unwrap_or_else(|| account.scope.clone());

        if let Ok(userinfo) = fetch_google_userinfo(&state, &token.access_token).await {
            account.account_email = userinfo.email.or(account.account_email);
            account.account_name = userinfo.name.or(account.account_name);
        }

        state.store.upsert_account(&account).await?;
        Result::<(), anyhow::Error>::Ok(())
    }
    .await;

    let (status, detail) = match outcome {
        Ok(()) => (
            "Google Workspace connected.",
            "You can close this window and return to the SwarmClaw Google Sheets page.",
        ),
        Err(ref error) => {
            warn!("Google Workspace connection failed: {}", error);
            (
                "Google Workspace connection failed.",
                "Check the terminal log and retry the connection flow.",
            )
        }
    };

    let notify = if outcome.is_ok() {
        "window.opener && window.opener.postMessage({ type: 'swarmclaw-google-workspace-connected' }, window.location.origin);"
    } else {
        ""
    };

    Html(format!(
        r#"<!doctype html>
        <html lang="en">
          <body style="font-family: sans-serif; background: #f4efe6; color: #1f1f18; display: grid; place-items: center; min-height: 100vh;">
            <main style="max-width: 420px; padding: 28px; border-radius: 18px; border: 1px solid #d8ccb6; background: rgba(255,250,241,0.92); text-align: center;">
              <h1 style="margin-top: 0;">{status}</h1>
              <p style="line-height: 1.5;">{detail}</p>
            </main>
            <script>
              {notify}
              setTimeout(() => window.close(), 900);
            </script>
          </body>
        </html>"#,
        status = status,
        detail = detail,
        notify = notify,
    ))
}

async fn create_picker_token(
    State(state): State<Arc<GoogleWorkspaceState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    json_result(async move {
        let account = require_google_account(&state).await?;
        let token = refresh_google_access_token(&state, &account.refresh_token).await?;
        Ok(Json(json!({
            "access_token": token.access_token,
            "expires_in": token.expires_in,
            "scope": token.scope,
            "token_type": token.token_type,
        })))
    })
    .await
}

async fn bind_google_sheet(
    State(state): State<Arc<GoogleWorkspaceState>>,
    Json(payload): Json<BindGoogleSheetRequest>,
) -> Result<Json<GoogleSheetBinding>, (StatusCode, Json<Value>)> {
    json_result(async move {
        validate_alias(&payload.alias)?;
        let account = require_google_account(&state).await?;
        let token = refresh_google_access_token(&state, &account.refresh_token).await?;
        let metadata =
            fetch_google_spreadsheet_metadata(&state, &token.access_token, &payload.spreadsheet_id)
                .await?;
        let binding = StoredGoogleSheetBinding {
            id: Uuid::new_v4().to_string(),
            alias: payload.alias.trim().to_string(),
            spreadsheet_id: metadata.spreadsheet_id,
            spreadsheet_title: metadata
                .properties
                .and_then(|properties| properties.title)
                .unwrap_or_else(|| "Untitled spreadsheet".to_string()),
            sheet_titles: metadata
                .sheets
                .into_iter()
                .filter_map(|sheet| sheet.properties.and_then(|properties| properties.title))
                .collect::<Vec<_>>(),
            allowed_tabs: payload
                .allowed_tabs
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>(),
            allowed_ranges: payload
                .allowed_ranges
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>(),
        };
        state.store.save_sheet_binding(&binding).await?;
        Ok(Json(binding.into()))
    })
    .await
}

async fn delete_google_sheet(
    State(state): State<Arc<GoogleWorkspaceState>>,
    AxumPath(binding_id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    json_result(async move {
        if !state.store.delete_sheet_binding(&binding_id).await? {
            bail!("Google Sheet binding not found");
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn handle_mcp(
    State(state): State<Arc<GoogleWorkspaceState>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let id = request.id.clone();
    let response = match dispatch_mcp(&state, request).await {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message: error.to_string(),
            }),
        },
    };

    Json(response)
}

async fn dispatch_mcp(state: &GoogleWorkspaceState, request: JsonRpcRequest) -> Result<Value> {
    match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "serverInfo": {
                "name": "swarmclaw-google-workspace",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {}
            }
        })),
        "tools/list" => Ok(json!({
            "tools": [
                {
                    "name": "list_bound_google_sheets",
                    "description": "List every spreadsheet alias currently bound in the local SwarmClaw Google Workspace store.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "get_google_sheet_metadata",
                    "description": "Fetch fresh spreadsheet metadata for a previously bound alias.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias"],
                        "properties": {
                            "alias": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "get_google_sheet_values",
                    "description": "Read values from a concrete A1 range inside a previously bound spreadsheet alias.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "range"],
                        "properties": {
                            "alias": { "type": "string" },
                            "range": { "type": "string" },
                            "majorDimension": {
                                "type": "string",
                                "enum": ["ROWS", "COLUMNS"]
                            },
                            "valueRenderOption": {
                                "type": "string",
                                "enum": ["FORMATTED_VALUE", "UNFORMATTED_VALUE", "FORMULA"]
                            },
                            "dateTimeRenderOption": {
                                "type": "string",
                                "enum": ["SERIAL_NUMBER", "FORMATTED_STRING"]
                            }
                        }
                    }
                },
                {
                    "name": "create_google_doc",
                    "description": "Create a new Google Doc with an optional initial body of text.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["title"],
                        "properties": {
                            "title": { "type": "string" },
                            "initialText": { "type": "string" },
                            "folderId": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "get_google_doc_content",
                    "description": "Fetch the content of a Google Doc by document id.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["documentId"],
                        "properties": {
                            "documentId": { "type": "string" },
                            "format": {
                                "type": "string",
                                "enum": ["plain_text", "markdown", "json"]
                            }
                        }
                    }
                },
                {
                    "name": "append_google_doc_text",
                    "description": "Append text to the end of an existing Google Doc.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["documentId", "text"],
                        "properties": {
                            "documentId": { "type": "string" },
                            "text": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "insert_google_doc_image",
                    "description": "Insert a publicly accessible image into an existing Google Doc.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["documentId", "imageUrl"],
                        "properties": {
                            "documentId": { "type": "string" },
                            "imageUrl": { "type": "string" },
                            "widthPt": { "type": "number" },
                            "heightPt": { "type": "number" },
                            "locationIndex": { "type": "integer" }
                        }
                    }
                },
                {
                    "name": "share_google_doc",
                    "description": "Share an existing Google Doc with one or more recipients by email.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["documentId"],
                        "properties": {
                            "documentId": { "type": "string" },
                            "email": { "type": "string" },
                            "emails": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "role": {
                                "type": "string",
                                "enum": ["reader", "commenter", "writer"]
                            },
                            "sendNotificationEmail": { "type": "boolean" },
                            "emailMessage": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "replace_google_doc_text",
                    "description": "Replace the body content of an existing Google Doc with new text.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["documentId", "text"],
                        "properties": {
                            "documentId": { "type": "string" },
                            "text": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "search_gmail",
                    "description": "Search Gmail messages for the connected Google account.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" },
                            "maxResults": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 100
                            },
                            "includeSpamTrash": { "type": "boolean" }
                        }
                    }
                },
                {
                    "name": "list_gmail_threads",
                    "description": "List Gmail threads for the connected Google account, optionally filtered by query.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" },
                            "maxResults": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 100
                            },
                            "includeSpamTrash": { "type": "boolean" }
                        }
                    }
                },
                {
                    "name": "get_gmail_message",
                    "description": "Fetch a Gmail message by id, including decoded body content when available.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["messageId"],
                        "properties": {
                            "messageId": { "type": "string" },
                            "format": {
                                "type": "string",
                                "enum": ["minimal", "metadata", "full", "raw"]
                            }
                        }
                    }
                },
                {
                    "name": "send_gmail_message",
                    "description": "Send an email from the connected Gmail account.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["to", "subject", "bodyText"],
                        "properties": {
                            "to": {
                                "oneOf": [
                                    { "type": "string" },
                                    { "type": "array", "items": { "type": "string" } }
                                ]
                            },
                            "cc": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "bcc": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "subject": { "type": "string" },
                            "bodyText": { "type": "string" },
                            "threadId": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "draft_gmail_message",
                    "description": "Create a Gmail draft from the connected Gmail account.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["to", "subject", "bodyText"],
                        "properties": {
                            "to": {
                                "oneOf": [
                                    { "type": "string" },
                                    { "type": "array", "items": { "type": "string" } }
                                ]
                            },
                            "cc": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "bcc": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "subject": { "type": "string" },
                            "bodyText": { "type": "string" },
                            "threadId": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "create_google_sheet_tab",
                    "description": "Create a new tab inside a previously bound spreadsheet alias.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "title"],
                        "properties": {
                            "alias": { "type": "string" },
                            "title": { "type": "string" },
                            "index": {
                                "type": "integer",
                                "minimum": 0
                            }
                        }
                    }
                },
                {
                    "name": "append_google_sheet_rows",
                    "description": "Append rows to a logical table inside a previously bound spreadsheet alias.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "range", "values"],
                        "properties": {
                            "alias": { "type": "string" },
                            "range": { "type": "string" },
                            "values": {
                                "type": "array",
                                "items": { "type": "array", "items": {} }
                            },
                            "valueInputOption": {
                                "type": "string",
                                "enum": ["RAW", "USER_ENTERED"]
                            },
                            "insertDataOption": {
                                "type": "string",
                                "enum": ["OVERWRITE", "INSERT_ROWS"]
                            }
                        }
                    }
                },
                {
                    "name": "update_google_sheet_values",
                    "description": "Overwrite a concrete A1 range inside a previously bound spreadsheet alias. Formulas (e.g. =HYPERLINK) are supported when valueInputOption is USER_ENTERED (default). WARNING: Ensure the target range exists within the sheet's current bounds (max rows/cols), otherwise it will fail with an exceeds grid limits error.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "range", "values"],
                        "properties": {
                            "alias": { "type": "string" },
                            "range": { "type": "string" },
                            "values": {
                                "description": "A 2D array of values (rows of columns), a 1D array (single row), or a single primitive value.",
                                "type": ["array", "string", "number", "boolean", "null"],
                                "items": { "type": "array", "items": {} }
                            },
                            "valueInputOption": {
                                "type": "string",
                                "enum": ["RAW", "USER_ENTERED"],
                                "default": "USER_ENTERED"
                            }
                        }
                    }
                },
                {
                    "name": "update_google_sheet_cell",
                    "description": "Update a single cell in a previously bound spreadsheet alias. Supports formulas. WARNING: Ensure the cell exists within current sheet bounds.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "range", "value"],
                        "properties": {
                            "alias": { "type": "string" },
                            "range": { "type": "string", "description": "A single cell A1 range, e.g. Sheet1!A1" },
                            "value": { "description": "The value or formula to insert." },
                            "valueInputOption": {
                                "type": "string",
                                "enum": ["RAW", "USER_ENTERED"],
                                "default": "USER_ENTERED"
                            }
                        }
                    }
                },
                {
                    "name": "batch_update_google_sheet_values",
                    "description": "Write multiple A1 ranges in a previously bound spreadsheet alias with one request. Formulas are supported. WARNING: Ensure ranges do not exceed current grid limits.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["alias", "data"],
                        "properties": {
                            "alias": { "type": "string" },
                            "valueInputOption": {
                                "type": "string",
                                "enum": ["RAW", "USER_ENTERED"],
                                "default": "USER_ENTERED"
                            },
                            "data": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "required": ["range", "values"],
                                    "properties": {
                                        "range": { "type": "string" },
                                        "values": {
                                            "description": "A 2D array, 1D array, or a single primitive value.",
                                            "type": ["array", "string", "number", "boolean", "null"]
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            ]
        })),
        "tools/call" => {
            let params = request.params.unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .context("Missing MCP tool name")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let structured = execute_google_tool(state, name, arguments).await?;
            Ok(json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&structured)?
                    }
                ],
                "structuredContent": structured,
                "isError": false
            }))
        }
        other => bail!("Unsupported MCP method: {}", other),
    }
}

async fn execute_google_tool(
    state: &GoogleWorkspaceState,
    name: &str,
    args: Value,
) -> Result<Value> {
    match name {
        "list_bound_google_sheets" => Ok(json!({
            "bindings": state
                .store
                .list_sheet_bindings()
                .await?
                .into_iter()
                .map(GoogleSheetBinding::from)
                .collect::<Vec<_>>()
        })),
        "get_google_sheet_metadata" => {
            let binding =
                require_binding_for_alias(state, required_string(&args, "alias")?).await?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let metadata = fetch_google_spreadsheet_metadata(
                state,
                &token.access_token,
                &binding.spreadsheet_id,
            )
            .await?;
            Ok(serde_json::to_value(metadata)?)
        }
        "get_google_sheet_values" => {
            let alias = required_string(&args, "alias")?;
            let range = required_string(&args, "range")?;
            let binding = require_binding_for_alias(state, alias).await?;
            enforce_binding_range_policy(&binding, range)?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let major_dimension = optional_string(&args, "majorDimension").unwrap_or("ROWS");
            let value_render_option =
                optional_string(&args, "valueRenderOption").unwrap_or("FORMATTED_VALUE");
            let date_time_render_option =
                optional_string(&args, "dateTimeRenderOption").unwrap_or("SERIAL_NUMBER");
            let encoded_range = encode_sheet_range(range);
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?majorDimension={}&valueRenderOption={}&dateTimeRenderOption={}",
                binding.spreadsheet_id,
                encoded_range,
                major_dimension,
                value_render_option,
                date_time_render_option,
            );
            google_get_json(state, &token.access_token, &url).await
        }
        "create_google_doc" => {
            let title = required_string(&args, "title")?;
            let initial_text = optional_string(&args, "initialText").unwrap_or("");
            let folder_id = optional_string(&args, "folderId");
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;

            let created = google_post_json(
                state,
                &token.access_token,
                "https://docs.googleapis.com/v1/documents",
                &json!({ "title": title }),
            )
            .await?;
            let document_id = created
                .get("documentId")
                .and_then(Value::as_str)
                .context("Google Docs create response did not include a documentId")?;

            if !initial_text.is_empty() {
                update_google_doc_text(
                    state,
                    &token.access_token,
                    document_id,
                    &json!([
                        {
                            "insertText": {
                                "location": { "index": 1 },
                                "text": initial_text,
                            }
                        }
                    ]),
                )
                .await?;
            }

            if let Some(folder_id) = folder_id {
                add_google_drive_parent(state, &token.access_token, document_id, folder_id).await?;
            }

            Ok(json!({
                "documentId": document_id,
                "title": created.get("title").and_then(Value::as_str).unwrap_or(title),
                "url": google_doc_url(document_id),
                "folderId": folder_id,
            }))
        }
        "get_google_doc_content" => {
            let document_id = required_string(&args, "documentId")?;
            let format = optional_string(&args, "format").unwrap_or("plain_text");
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let url = format!("https://docs.googleapis.com/v1/documents/{}", document_id);
            let document = google_get_json(state, &token.access_token, &url).await?;
            match format {
                "json" => Ok(document),
                "plain_text" | "markdown" => Ok(json!({
                    "documentId": document_id,
                    "title": document.get("title").and_then(Value::as_str),
                    "format": format,
                    "text": extract_google_doc_plain_text(&document),
                    "url": google_doc_url(document_id),
                })),
                other => bail!("Unsupported Google Doc format '{}'", other),
            }
        }
        "search_gmail" => {
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let query = optional_string(&args, "query");
            let max_results = optional_u64(&args, "maxResults")
                .unwrap_or(10)
                .clamp(1, 100);
            let include_spam_trash = optional_bool(&args, "includeSpamTrash").unwrap_or(false);
            let url = gmail_list_url("messages", query, max_results, include_spam_trash)?;
            google_get_json(state, &token.access_token, url.as_str()).await
        }
        "list_gmail_threads" => {
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let query = optional_string(&args, "query");
            let max_results = optional_u64(&args, "maxResults")
                .unwrap_or(10)
                .clamp(1, 100);
            let include_spam_trash = optional_bool(&args, "includeSpamTrash").unwrap_or(false);
            let url = gmail_list_url("threads", query, max_results, include_spam_trash)?;
            google_get_json(state, &token.access_token, url.as_str()).await
        }
        "get_gmail_message" => {
            let message_id = required_string(&args, "messageId")?;
            let format = optional_string(&args, "format").unwrap_or("full");
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}?format={}",
                message_id, format
            );
            let message = google_get_json(state, &token.access_token, &url).await?;
            if format == "raw" {
                return Ok(message);
            }
            Ok(normalize_gmail_message(&message))
        }
        "append_google_doc_text" => {
            let document_id = required_string(&args, "documentId")?;
            let text = required_string(&args, "text")?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let result = update_google_doc_text(
                state,
                &token.access_token,
                document_id,
                &json!([
                    {
                        "insertText": {
                            "endOfSegmentLocation": {},
                            "text": text,
                        }
                    }
                ]),
            )
            .await?;
            Ok(json!({
                "documentId": document_id,
                "url": google_doc_url(document_id),
                "result": result,
            }))
        }
        "insert_google_doc_image" => {
            let document_id = required_string(&args, "documentId")?;
            let image_url = required_string(&args, "imageUrl")?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;

            let mut insert_request = serde_json::Map::new();
            insert_request.insert("uri".to_string(), Value::String(image_url.to_string()));

            if let (Some(width), Some(height)) = (
                optional_f64(&args, "widthPt"),
                optional_f64(&args, "heightPt"),
            ) {
                insert_request.insert(
                    "objectSize".to_string(),
                    json!({
                        "width": { "magnitude": width, "unit": "PT" },
                        "height": { "magnitude": height, "unit": "PT" }
                    }),
                );
            }

            if let Some(location_index) = optional_u64(&args, "locationIndex") {
                insert_request.insert("location".to_string(), json!({ "index": location_index }));
            } else {
                insert_request.insert("endOfSegmentLocation".to_string(), json!({}));
            }

            let result = update_google_doc_text(
                state,
                &token.access_token,
                document_id,
                &json!([
                    {
                        "insertInlineImage": Value::Object(insert_request)
                    }
                ]),
            )
            .await?;
            Ok(json!({
                "documentId": document_id,
                "url": google_doc_url(document_id),
                "imageUrl": image_url,
                "result": result,
            }))
        }
        "share_google_doc" => {
            let document_id = required_string(&args, "documentId")?;
            let recipients = required_google_share_recipients(&args)?;
            let role = optional_string(&args, "role").unwrap_or("reader");
            if !matches!(role, "reader" | "commenter" | "writer") {
                bail!(
                    "Unsupported Google Doc share role '{}'. Expected one of reader, commenter, or writer.",
                    role
                );
            }
            let send_notification_email =
                optional_bool(&args, "sendNotificationEmail").unwrap_or(true);
            let email_message = optional_string(&args, "emailMessage")
                .map(sanitize_email_header_value)
                .filter(|value| !value.is_empty());
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;

            let mut permissions = Vec::with_capacity(recipients.len());
            for recipient in recipients {
                let permission = create_google_drive_permission(
                    state,
                    &token.access_token,
                    document_id,
                    &recipient,
                    role,
                    send_notification_email,
                    email_message.as_deref(),
                )
                .await?;
                permissions.push(permission);
            }

            Ok(json!({
                "documentId": document_id,
                "url": google_doc_url(document_id),
                "role": role,
                "sendNotificationEmail": send_notification_email,
                "permissions": permissions,
            }))
        }
        "replace_google_doc_text" => {
            let document_id = required_string(&args, "documentId")?;
            let text = required_string(&args, "text")?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let current = google_get_json(
                state,
                &token.access_token,
                &format!("https://docs.googleapis.com/v1/documents/{}", document_id),
            )
            .await?;
            let end_index = google_doc_body_end_index(&current);
            let mut requests = Vec::new();
            if end_index > 1 {
                requests.push(json!({
                    "deleteContentRange": {
                        "range": {
                            "startIndex": 1,
                            "endIndex": end_index - 1,
                        }
                    }
                }));
            }
            if !text.is_empty() {
                requests.push(json!({
                    "insertText": {
                        "location": { "index": 1 },
                        "text": text,
                    }
                }));
            }
            let result = update_google_doc_text(
                state,
                &token.access_token,
                document_id,
                &Value::Array(requests),
            )
            .await?;
            Ok(json!({
                "documentId": document_id,
                "url": google_doc_url(document_id),
                "result": result,
            }))
        }
        "send_gmail_message" => {
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let payload = gmail_outbound_payload(&args, false)?;
            google_post_json(
                state,
                &token.access_token,
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/send",
                &payload,
            )
            .await
        }
        "draft_gmail_message" => {
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let payload = gmail_outbound_payload(&args, true)?;
            google_post_json(
                state,
                &token.access_token,
                "https://gmail.googleapis.com/gmail/v1/users/me/drafts",
                &payload,
            )
            .await
        }
        "create_google_sheet_tab" => {
            let alias = required_string(&args, "alias")?;
            let title = required_string(&args, "title")?.trim();
            if title.is_empty() {
                bail!("Tab title is required");
            }
            let binding = require_binding_for_alias(state, alias).await?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let mut properties = serde_json::Map::new();
            properties.insert("title".to_string(), Value::String(title.to_string()));
            if let Some(index) = optional_u64(&args, "index") {
                properties.insert("index".to_string(), Value::Number(index.into()));
            }
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}:batchUpdate",
                binding.spreadsheet_id
            );
            google_post_json(
                state,
                &token.access_token,
                &url,
                &json!({
                    "requests": [
                        {
                            "addSheet": {
                                "properties": Value::Object(properties)
                            }
                        }
                    ]
                }),
            )
            .await
        }
        "append_google_sheet_rows" => {
            let alias = required_string(&args, "alias")?;
            let range = required_string(&args, "range")?;
            let binding = require_binding_for_alias(state, alias).await?;
            enforce_binding_range_policy(&binding, range)?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let value_input_option =
                optional_string(&args, "valueInputOption").unwrap_or("USER_ENTERED");
            let insert_data_option =
                optional_string(&args, "insertDataOption").unwrap_or("INSERT_ROWS");
            let values = normalize_sheet_values(args.get("values").cloned().context("Missing values")?);
            let encoded_range = encode_sheet_range(range);
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}:append?valueInputOption={}&insertDataOption={}",
                binding.spreadsheet_id,
                encoded_range,
                value_input_option,
                insert_data_option,
            );
            google_post_json(
                state,
                &token.access_token,
                &url,
                &json!({ "range": range, "majorDimension": "ROWS", "values": values }),
            )
            .await
        }
        "update_google_sheet_values" => {
            let alias = required_string(&args, "alias")?;
            let range = required_string(&args, "range")?;
            let binding = require_binding_for_alias(state, alias).await?;
            enforce_binding_range_policy(&binding, range)?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let value_input_option =
                optional_string(&args, "valueInputOption").unwrap_or("USER_ENTERED");
            let values = normalize_sheet_values(args.get("values").cloned().context("Missing values")?);
            let encoded_range = encode_sheet_range(range);
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?valueInputOption={}",
                binding.spreadsheet_id, encoded_range, value_input_option,
            );
            google_put_json(
                state,
                &token.access_token,
                &url,
                &json!({ "range": range, "majorDimension": "ROWS", "values": values }),
            )
            .await
        }
        "update_google_sheet_cell" => {
            let alias = required_string(&args, "alias")?;
            let range = required_string(&args, "range")?;
            let binding = require_binding_for_alias(state, alias).await?;
            enforce_binding_range_policy(&binding, range)?;
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let value_input_option =
                optional_string(&args, "valueInputOption").unwrap_or("USER_ENTERED");
            let value = args.get("value").cloned().context("Missing value")?;
            let values = json!([[value]]);
            let encoded_range = encode_sheet_range(range);
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?valueInputOption={}",
                binding.spreadsheet_id, encoded_range, value_input_option,
            );
            google_put_json(
                state,
                &token.access_token,
                &url,
                &json!({ "range": range, "majorDimension": "ROWS", "values": values }),
            )
            .await
        }
        "batch_update_google_sheet_values" => {
            let alias = required_string(&args, "alias")?;
            let binding = require_binding_for_alias(state, alias).await?;
            let mut data = args
                .get("data")
                .cloned()
                .context("Missing data payload for batch update")?;
            
            if let Some(items) = data.as_array_mut() {
                for item in items {
                    let range = item
                        .get("range")
                        .and_then(Value::as_str)
                        .context("Each batch update range requires a range string")?;
                    enforce_binding_range_policy(&binding, range)?;
                    if let Some(values) = item.get_mut("values") {
                        *values = normalize_sheet_values(values.clone());
                    }
                }
            }
            let account = require_google_account(state).await?;
            let token = refresh_google_access_token(state, &account.refresh_token).await?;
            let value_input_option =
                optional_string(&args, "valueInputOption").unwrap_or("USER_ENTERED");
            let url = format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values:batchUpdate",
                binding.spreadsheet_id
            );
            google_post_json(
                state,
                &token.access_token,
                &url,
                &json!({ "valueInputOption": value_input_option, "data": data }),
            )
            .await
        }
        other => bail!("Unsupported Google Workspace tool: {}", other),
    }
}

fn normalize_sheet_values(values: Value) -> Value {
    match values {
        Value::Array(items) => {
            if items.iter().all(|item| !item.is_array()) {
                // Wrap 1D array into 2D (a single row)
                Value::Array(vec![Value::Array(items)])
            } else {
                Value::Array(items)
            }
        }
        // Wrap single value into 2D array
        other => Value::Array(vec![Value::Array(vec![other])]),
    }
}

async fn exchange_google_code(
    state: &GoogleWorkspaceState,
    code: &str,
) -> Result<GoogleTokenResponse> {
    let response = state
        .http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code),
            ("client_id", state.config.client_id.as_str()),
            ("client_secret", state.config.client_secret.as_str()),
            ("redirect_uri", state.config.redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .context("Failed to exchange Google authorization code")?;
    parse_google_json_response(response).await
}

async fn refresh_google_access_token(
    state: &GoogleWorkspaceState,
    refresh_token: &str,
) -> Result<GoogleTokenResponse> {
    let response = state
        .http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", state.config.client_id.as_str()),
            ("client_secret", state.config.client_secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("Failed to refresh Google access token")?;
    parse_google_json_response(response).await
}

async fn update_google_doc_text(
    state: &GoogleWorkspaceState,
    access_token: &str,
    document_id: &str,
    requests: &Value,
) -> Result<Value> {
    google_post_json(
        state,
        access_token,
        &format!(
            "https://docs.googleapis.com/v1/documents/{}:batchUpdate",
            document_id
        ),
        &json!({ "requests": requests }),
    )
    .await
}

async fn add_google_drive_parent(
    state: &GoogleWorkspaceState,
    access_token: &str,
    file_id: &str,
    folder_id: &str,
) -> Result<Value> {
    let metadata: GoogleDriveFileParentsResponse = parse_google_json_response(
        state
            .http
            .get(format!(
                "https://www.googleapis.com/drive/v3/files/{}?fields=parents",
                file_id
            ))
            .bearer_auth(access_token)
            .send()
            .await
            .with_context(|| format!("Failed to fetch Google Drive parents for {}", file_id))?,
    )
    .await?;
    let mut url = Url::parse(&format!(
        "https://www.googleapis.com/drive/v3/files/{}",
        file_id
    ))
    .context("Failed to construct Google Drive update URL")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("addParents", folder_id);
        query.append_pair("fields", "id,name,parents");
        if !metadata.parents.is_empty() {
            query.append_pair("removeParents", &metadata.parents.join(","));
        }
    }
    google_patch_json(state, access_token, url.as_str(), &json!({})).await
}

async fn create_google_drive_permission(
    state: &GoogleWorkspaceState,
    access_token: &str,
    file_id: &str,
    email: &str,
    role: &str,
    send_notification_email: bool,
    email_message: Option<&str>,
) -> Result<Value> {
    let mut url = Url::parse(&format!(
        "https://www.googleapis.com/drive/v3/files/{}/permissions",
        file_id
    ))
    .context("Failed to construct Google Drive permissions URL")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair(
            "sendNotificationEmail",
            if send_notification_email {
                "true"
            } else {
                "false"
            },
        );
        query.append_pair("fields", "id,type,role,emailAddress");
        if let Some(message) = email_message {
            query.append_pair("emailMessage", message);
        }
    }

    google_post_json(
        state,
        access_token,
        url.as_str(),
        &json!({
            "role": role,
            "type": "user",
            "emailAddress": email,
        }),
    )
    .await
}

async fn fetch_google_userinfo(
    state: &GoogleWorkspaceState,
    access_token: &str,
) -> Result<GoogleUserInfoResponse> {
    let response = state
        .http
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .context("Failed to fetch Google user info")?;
    parse_google_json_response(response).await
}

async fn fetch_google_spreadsheet_metadata(
    state: &GoogleWorkspaceState,
    access_token: &str,
    spreadsheet_id: &str,
) -> Result<GoogleSpreadsheetMetadataResponse> {
    let url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}?includeGridData=false&fields=spreadsheetId,properties.title,sheets.properties.title",
        spreadsheet_id
    );
    let response = state
        .http
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .context("Failed to fetch Google spreadsheet metadata")?;
    parse_google_json_response(response).await
}

async fn google_post_json(
    state: &GoogleWorkspaceState,
    access_token: &str,
    url: &str,
    body: &Value,
) -> Result<Value> {
    let response = state
        .http
        .post(url)
        .bearer_auth(access_token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("Failed to POST Google Sheets request to {}", url))?;
    parse_google_json_response(response).await
}

async fn google_get_json(
    state: &GoogleWorkspaceState,
    access_token: &str,
    url: &str,
) -> Result<Value> {
    let response = state
        .http
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .with_context(|| format!("Failed to GET Google Sheets request from {}", url))?;
    parse_google_json_response(response).await
}

async fn google_patch_json(
    state: &GoogleWorkspaceState,
    access_token: &str,
    url: &str,
    body: &Value,
) -> Result<Value> {
    let response = state
        .http
        .patch(url)
        .bearer_auth(access_token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("Failed to PATCH Google request to {}", url))?;
    parse_google_json_response(response).await
}

async fn google_put_json(
    state: &GoogleWorkspaceState,
    access_token: &str,
    url: &str,
    body: &Value,
) -> Result<Value> {
    let response = state
        .http
        .put(url)
        .bearer_auth(access_token)
        .json(body)
        .send()
        .await
        .with_context(|| format!("Failed to PUT Google Sheets request to {}", url))?;
    parse_google_json_response(response).await
}

async fn parse_google_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read Google API response body")?;
    if !status.is_success() {
        bail!("Google API request failed with status {}: {}", status, body);
    }

    serde_json::from_str(&body)
        .with_context(|| format!("Failed to parse Google API response: {}", body))
}

async fn require_google_account(state: &GoogleWorkspaceState) -> Result<StoredGoogleAccount> {
    state
        .store
        .account()
        .await?
        .context("No Google Workspace account is connected. Open the local UI and complete the OAuth flow first.")
}

async fn require_binding_for_alias(
    state: &GoogleWorkspaceState,
    alias: &str,
) -> Result<StoredGoogleSheetBinding> {
    state
        .store
        .find_sheet_binding_by_alias(alias)
        .await?
        .with_context(|| format!("No Google Sheet binding found for alias '{}'", alias))
}

fn validate_alias(alias: &str) -> Result<()> {
    let trimmed = alias.trim();
    if trimmed.is_empty() {
        bail!("Alias is required");
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("Alias may only contain letters, numbers, '_' and '-'");
    }
    Ok(())
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("Missing required string field '{}'", key))
}

fn optional_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
    })
}

fn optional_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn extract_google_doc_plain_text(document: &Value) -> String {
    let mut text = String::new();
    for element in document
        .get("body")
        .and_then(|body| body.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(paragraph_elements) = element
            .get("paragraph")
            .and_then(|paragraph| paragraph.get("elements"))
            .and_then(Value::as_array)
        {
            for paragraph_element in paragraph_elements {
                if let Some(content) = paragraph_element
                    .get("textRun")
                    .and_then(|text_run| text_run.get("content"))
                    .and_then(Value::as_str)
                {
                    text.push_str(content);
                }
            }
        }
        if let Some(table_rows) = element
            .get("table")
            .and_then(|table| table.get("tableRows"))
            .and_then(Value::as_array)
        {
            for row in table_rows {
                if let Some(cells) = row.get("tableCells").and_then(Value::as_array) {
                    for cell in cells {
                        if let Some(content) = cell.get("content").and_then(Value::as_array) {
                            let nested = json!({ "body": { "content": content } });
                            text.push_str(&extract_google_doc_plain_text(&nested));
                            if !text.ends_with('\t') {
                                text.push('\t');
                            }
                        }
                    }
                    if text.ends_with('\t') {
                        text.pop();
                    }
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                }
            }
        }
    }
    text.trim_end().to_string()
}

fn google_doc_body_end_index(document: &Value) -> i64 {
    document
        .get("body")
        .and_then(|body| body.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| {
            content
                .iter()
                .filter_map(|element| element.get("endIndex").and_then(Value::as_i64))
                .max()
        })
        .unwrap_or(1)
}

fn google_doc_url(document_id: &str) -> String {
    format!("https://docs.google.com/document/d/{}/edit", document_id)
}

fn gmail_list_url(
    resource: &str,
    query: Option<&str>,
    max_results: u64,
    include_spam_trash: bool,
) -> Result<Url> {
    let mut url = Url::parse(&format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/{}",
        resource
    ))
    .context("Failed to construct Gmail list URL")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("maxResults", &max_results.to_string());
        if let Some(query) = query {
            pairs.append_pair("q", query);
        }
        if include_spam_trash {
            pairs.append_pair("includeSpamTrash", "true");
        }
    }
    Ok(url)
}

fn gmail_outbound_payload(args: &Value, as_draft: bool) -> Result<Value> {
    let to = required_recipients(args, "to")?;
    let cc = optional_string_list(args, "cc");
    let bcc = optional_string_list(args, "bcc");
    let subject = sanitize_email_header_value(required_string(args, "subject")?);
    let body_text = required_string(args, "bodyText")?;
    let thread_id = optional_string(args, "threadId");
    let raw = build_gmail_raw_message(&to, &cc, &bcc, &subject, body_text);
    let mut message = serde_json::Map::new();
    message.insert("raw".to_string(), Value::String(raw));
    if let Some(thread_id) = thread_id {
        message.insert("threadId".to_string(), Value::String(thread_id.to_string()));
    }
    if as_draft {
        Ok(json!({ "message": Value::Object(message) }))
    } else {
        Ok(Value::Object(message))
    }
}

fn build_gmail_raw_message(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body_text: &str,
) -> String {
    let mut mime = String::new();
    mime.push_str(&format!("To: {}\r\n", to.join(", ")));
    if !cc.is_empty() {
        mime.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
    }
    if !bcc.is_empty() {
        mime.push_str(&format!("Bcc: {}\r\n", bcc.join(", ")));
    }
    mime.push_str(&format!("Subject: {}\r\n", subject));
    mime.push_str("MIME-Version: 1.0\r\n");
    mime.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    mime.push_str("Content-Transfer-Encoding: 8bit\r\n");
    mime.push_str("\r\n");
    mime.push_str(body_text);
    URL_SAFE_NO_PAD.encode(mime.as_bytes())
}

fn required_google_share_recipients(args: &Value) -> Result<Vec<String>> {
    if args.get("emails").is_some() {
        return required_recipients(args, "emails");
    }
    required_recipients(args, "email")
}

fn required_recipients(args: &Value, key: &str) -> Result<Vec<String>> {
    let recipients = if let Some(value) = args.get(key) {
        if let Some(single) = value.as_str() {
            vec![single.trim().to_string()]
        } else if let Some(items) = value.as_array() {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.trim().to_string())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let recipients = recipients
        .into_iter()
        .filter(|item| !item.is_empty())
        .map(|item| sanitize_email_header_value(&item))
        .collect::<Vec<_>>();
    if recipients.is_empty() {
        bail!("Missing required recipient field '{}'", key);
    }
    Ok(recipients)
}

fn optional_string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| sanitize_email_header_value(item.trim()))
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn sanitize_email_header_value(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

fn normalize_gmail_message(message: &Value) -> Value {
    let payload = message.get("payload");
    json!({
        "id": message.get("id").and_then(Value::as_str),
        "threadId": message.get("threadId").and_then(Value::as_str),
        "labelIds": message.get("labelIds"),
        "snippet": message.get("snippet").and_then(Value::as_str),
        "internalDate": message.get("internalDate").and_then(Value::as_str),
        "headers": gmail_selected_headers(payload),
        "textBody": gmail_extract_body(payload, "text/plain"),
        "htmlBody": gmail_extract_body(payload, "text/html"),
        "rawPayload": message,
    })
}

fn gmail_selected_headers(payload: Option<&Value>) -> Value {
    let mut headers = serde_json::Map::new();
    for name in ["From", "To", "Cc", "Bcc", "Subject", "Date", "Reply-To"] {
        if let Some(value) = gmail_header_value(payload, name) {
            headers.insert(name.to_string(), Value::String(value));
        }
    }
    Value::Object(headers)
}

fn gmail_header_value(payload: Option<&Value>, name: &str) -> Option<String> {
    payload
        .and_then(|payload| payload.get("headers"))
        .and_then(Value::as_array)
        .and_then(|headers| {
            headers.iter().find_map(|header| {
                let header_name = header.get("name").and_then(Value::as_str)?;
                if header_name.eq_ignore_ascii_case(name) {
                    header
                        .get("value")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                } else {
                    None
                }
            })
        })
}

fn gmail_extract_body(payload: Option<&Value>, desired_mime: &str) -> Option<String> {
    let payload = payload?;
    let mime = payload
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if mime.eq_ignore_ascii_case(desired_mime) {
        if let Some(data) = payload
            .get("body")
            .and_then(|body| body.get("data"))
            .and_then(Value::as_str)
        {
            return decode_gmail_body(data);
        }
    }
    payload
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|part| gmail_extract_body(Some(part), desired_mime))
        })
}

fn decode_gmail_body(data: &str) -> Option<String> {
    let normalized = data.replace('-', "+").replace('_', "/");
    URL_SAFE_NO_PAD
        .decode(normalized.as_bytes())
        .or_else(|_| URL_SAFE_NO_PAD.decode(data.as_bytes()))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn parse_google_scope_list(value: &str) -> Vec<String> {
    value
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn env_var_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn enforce_binding_range_policy(binding: &StoredGoogleSheetBinding, range: &str) -> Result<()> {
    if !binding.allowed_tabs.is_empty() {
        let Some(sheet_name) = extract_sheet_name(range) else {
            bail!(
                "Range '{}' must include an explicit sheet name because alias '{}' is restricted to specific tabs",
                range,
                binding.alias
            );
        };
        if !binding.allowed_tabs.iter().any(|tab| tab == sheet_name) {
            bail!(
                "Range '{}' targets tab '{}' which is not allowed for alias '{}'",
                range,
                sheet_name,
                binding.alias
            );
        }
    }

    if !binding.allowed_ranges.is_empty()
        && !binding
            .allowed_ranges
            .iter()
            .any(|allowed| range.starts_with(allowed))
    {
        bail!(
            "Range '{}' is outside the allowed range prefixes for alias '{}'",
            range,
            binding.alias
        );
    }

    Ok(())
}

impl GoogleWorkspaceConfig {
    fn oauth_scopes(&self) -> Vec<String> {
        let mut scopes = vec![
            "https://www.googleapis.com/auth/drive.file".to_string(),
            "https://www.googleapis.com/auth/spreadsheets".to_string(),
            "https://www.googleapis.com/auth/documents".to_string(),
            "openid".to_string(),
            "email".to_string(),
            "profile".to_string(),
        ];
        scopes.extend(self.extra_scopes.clone());

        let mut seen = BTreeSet::new();
        scopes
            .into_iter()
            .filter(|scope| seen.insert(scope.clone()))
            .collect()
    }
}

fn extract_sheet_name(range: &str) -> Option<&str> {
    let (sheet_name, _) = range.split_once('!')?;
    Some(sheet_name.trim_matches('\''))
}

fn encode_sheet_range(range: &str) -> String {
    let mut encoded = String::with_capacity(range.len());
    for byte in range.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

async fn json_result<T, F>(fut: F) -> Result<T, (StatusCode, Json<Value>)>
where
    F: std::future::Future<Output = Result<T>>,
{
    fut.await.map_err(|error| {
        let message = error.to_string();
        let status = if message.contains("not found") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(json!({ "error": message })))
    })
}
