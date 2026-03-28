use crate::services::browser::BrowserService;
use anyhow::{bail, Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use reqwest::{
    header::{ACCEPT, CONTENT_TYPE},
    Url,
};
use scraper::{Html, Selector};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};
use tracing::{info, warn};

const DEFAULT_WEB_TOOLS_USER_AGENT: &str =
    "SwarmClawWebTools/1.0 (+https://github.com/Huggingplace/swarmclaw)";

#[derive(Clone)]
pub struct WebToolsService {
    bind_addr: SocketAddr,
    public_base_url: String,
    state: Arc<WebToolsState>,
}

#[derive(Clone)]
struct WebToolsState {
    config: WebToolsConfig,
    http: reqwest::Client,
}

#[derive(Clone)]
struct WebToolsConfig {
    public_base_url: String,
    default_search_provider: Option<SearchProviderKind>,
    google: Option<GoogleSearchConfig>,
    brave: Option<BraveSearchConfig>,
    searxng: Option<SearxngSearchConfig>,
}

#[derive(Clone)]
struct GoogleSearchConfig {
    api_key: String,
    cse_id: String,
}

#[derive(Clone)]
struct BraveSearchConfig {
    api_key: String,
}

#[derive(Clone)]
struct SearxngSearchConfig {
    base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchProviderKind {
    Google,
    Brave,
    Searxng,
}

impl SearchProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Brave => "brave",
            Self::Searxng => "searxng",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Google => "Google Programmable Search JSON API.",
            Self::Brave => "Brave Search API.",
            Self::Searxng => "Self-hosted SearXNG search instance.",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "google" | "google_api" | "google-programmable-search" => Some(Self::Google),
            "brave" | "brave_search" | "brave-search" => Some(Self::Brave),
            "searxng" | "searx" | "searx-ng" => Some(Self::Searxng),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
struct WebToolsStatus {
    fetch_available: bool,
    browser_fallback_available: bool,
    default_search_provider: Option<String>,
    providers: Vec<SearchProviderStatus>,
}

#[derive(Debug, Serialize)]
struct SearchProviderStatus {
    provider: String,
    configured: bool,
    is_default: bool,
    description: &'static str,
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

#[derive(Debug, Serialize)]
struct SearchResultItem {
    title: String,
    url: String,
    snippet: String,
    source: Option<String>,
}

#[derive(Debug)]
struct ExtractedPage {
    title: Option<String>,
    text: String,
    selector: Option<String>,
    js_heavy_hint: bool,
}

#[derive(Debug, Deserialize)]
struct GoogleSearchResponse {
    #[serde(default)]
    items: Vec<GoogleSearchItem>,
}

#[derive(Debug, Deserialize)]
struct GoogleSearchItem {
    title: String,
    link: String,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default, rename = "displayLink")]
    display_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveSearchItem>,
}

#[derive(Debug, Deserialize)]
struct BraveSearchItem {
    title: String,
    url: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearxngSearchResponse {
    #[serde(default)]
    results: Vec<SearxngSearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearxngSearchItem {
    title: String,
    url: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    engine: Option<String>,
}

impl WebToolsService {
    pub fn from_env() -> Result<Self> {
        let bind_addr = std::env::var("SWARMCLAW_WEB_TOOLS_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:4419".to_string())
            .parse::<SocketAddr>()
            .context("Failed to parse SWARMCLAW_WEB_TOOLS_BIND_ADDR")?;
        let public_base_url = std::env::var("SWARMCLAW_WEB_TOOLS_BASE_URL")
            .unwrap_or_else(|_| format!("http://{}", bind_addr));
        let timeout_secs = std::env::var("SWARMCLAW_WEB_TOOLS_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(20);
        let user_agent = std::env::var("SWARMCLAW_WEB_TOOLS_USER_AGENT")
            .unwrap_or_else(|_| DEFAULT_WEB_TOOLS_USER_AGENT.to_string());

        let default_search_provider = match std::env::var("SWARMCLAW_WEB_SEARCH_DEFAULT_PROVIDER") {
            Ok(value) if !value.trim().is_empty() => SearchProviderKind::parse(&value)
                .with_context(|| {
                    format!(
                        "Unsupported SWARMCLAW_WEB_SEARCH_DEFAULT_PROVIDER '{}'. Valid values: google, brave, searxng.",
                        value
                    )
                })
                .map(Some)?,
            _ => None,
        };

        let google = match (
            std::env::var("SWARMCLAW_WEB_SEARCH_GOOGLE_API_KEY").ok(),
            std::env::var("SWARMCLAW_WEB_SEARCH_GOOGLE_CSE_ID").ok(),
        ) {
            (Some(api_key), Some(cse_id))
                if !api_key.trim().is_empty() && !cse_id.trim().is_empty() =>
            {
                Some(GoogleSearchConfig { api_key, cse_id })
            }
            _ => None,
        };

        let brave = std::env::var("SWARMCLAW_WEB_SEARCH_BRAVE_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|api_key| BraveSearchConfig { api_key });

        let searxng = std::env::var("SWARMCLAW_WEB_SEARCH_SEARXNG_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|base_url| SearxngSearchConfig {
                base_url: base_url.trim_end_matches('/').to_string(),
            });

        let config = WebToolsConfig {
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
            default_search_provider,
            google,
            brave,
            searxng,
        };

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(user_agent)
            .build()
            .context("Failed to build WebTools HTTP client")?;

        Ok(Self {
            bind_addr,
            public_base_url: config.public_base_url.clone(),
            state: Arc::new(WebToolsState { config, http }),
        })
    }

    pub fn base_url(&self) -> String {
        self.public_base_url.clone()
    }

    pub fn fetch_mcp_endpoint(&self) -> String {
        format!("{}/mcp/fetch", self.public_base_url)
    }

    pub fn search_mcp_endpoint(&self) -> String {
        format!("{}/mcp/search", self.public_base_url)
    }

    pub async fn start(self) -> Result<()> {
        let app = Router::new()
            .route("/api/status", get(get_status))
            .route("/mcp/fetch", post(handle_fetch_mcp))
            .route("/mcp/search", post(handle_search_mcp))
            .with_state(self.state.clone());

        let listener = tokio::net::TcpListener::bind(self.bind_addr)
            .await
            .with_context(|| format!("Failed to bind Web Tools service to {}", self.bind_addr))?;

        info!(
            "SwarmClaw Web Tools service listening on {}",
            self.public_base_url
        );
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn get_status(
    State(state): State<Arc<WebToolsState>>,
) -> Result<Json<WebToolsStatus>, (StatusCode, Json<Value>)> {
    json_result(async move {
        Ok(Json(WebToolsStatus {
            fetch_available: true,
            browser_fallback_available: cfg!(feature = "headless_chrome"),
            default_search_provider: state
                .config
                .default_search_provider
                .map(|value| value.as_str().to_string()),
            providers: supported_providers(&state.config),
        }))
    })
    .await
}

async fn handle_fetch_mcp(
    State(state): State<Arc<WebToolsState>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    handle_mcp_request(state, request, dispatch_fetch_mcp).await
}

async fn handle_search_mcp(
    State(state): State<Arc<WebToolsState>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    handle_mcp_request(state, request, dispatch_search_mcp).await
}

async fn handle_mcp_request<F, Fut>(
    state: Arc<WebToolsState>,
    request: JsonRpcRequest,
    dispatch: F,
) -> Json<JsonRpcResponse>
where
    F: FnOnce(Arc<WebToolsState>, JsonRpcRequest) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let id = request.id.clone();
    let response = match dispatch(state, request).await {
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

async fn dispatch_fetch_mcp(state: Arc<WebToolsState>, request: JsonRpcRequest) -> Result<Value> {
    match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "serverInfo": {
                "name": "swarmclaw-web-fetch",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {}
            }
        })),
        "tools/list" => Ok(json!({
            "tools": [
                {
                    "name": "fetch_convert_page",
                    "description": "Fetch a URL, extract readable text, and optionally fall back to the headless browser service for JS-heavy pages.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["url"],
                        "properties": {
                            "url": { "type": "string" },
                            "render_js": { "type": "boolean" },
                            "auto_render_js": { "type": "boolean" },
                            "max_chars": { "type": "integer", "minimum": 500, "maximum": 200000 }
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
                .context("Missing MCP fetch tool name")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let structured = match name {
                "fetch_convert_page" => execute_fetch_convert_tool(&state, arguments).await?,
                other => bail!("Unsupported fetch tool: {}", other),
            };

            Ok(mcp_success(structured)?)
        }
        other => bail!("Unsupported MCP method: {}", other),
    }
}

async fn dispatch_search_mcp(state: Arc<WebToolsState>, request: JsonRpcRequest) -> Result<Value> {
    match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "serverInfo": {
                "name": "swarmclaw-web-search",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {}
            }
        })),
        "tools/list" => Ok(json!({
            "tools": [
                {
                    "name": "list_search_providers",
                    "description": "List every configured search provider available on this SwarmClaw host.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "search_web",
                    "description": "Search the web using the default or requested provider.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["query"],
                        "properties": {
                            "query": { "type": "string" },
                            "provider": {
                                "type": "string",
                                "enum": ["google", "brave", "searxng"]
                            },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 10 },
                            "safe_search": { "type": "boolean" }
                        }
                    }
                },
                {
                    "name": "search_google_web",
                    "description": "Search the web through Google Programmable Search JSON API on the host.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["query"],
                        "properties": {
                            "query": { "type": "string" },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 10 },
                            "safe_search": { "type": "boolean" }
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
                .context("Missing MCP search tool name")?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let structured = match name {
                "list_search_providers" => json!({
                    "default_provider": state
                        .config
                        .default_search_provider
                        .map(|value| value.as_str().to_string()),
                    "providers": supported_providers(&state.config)
                }),
                "search_web" => execute_search_tool(&state, arguments, None).await?,
                "search_google_web" => {
                    execute_search_tool(&state, arguments, Some(SearchProviderKind::Google)).await?
                }
                other => bail!("Unsupported search tool: {}", other),
            };

            Ok(mcp_success(structured)?)
        }
        other => bail!("Unsupported MCP method: {}", other),
    }
}

async fn execute_fetch_convert_tool(state: &WebToolsState, args: Value) -> Result<Value> {
    let url = required_string(&args, "url")?;
    let render_js = optional_bool(&args, "render_js").unwrap_or(false);
    let auto_render_js = optional_bool(&args, "auto_render_js").unwrap_or(true);
    let max_chars = optional_usize(&args, "max_chars")
        .unwrap_or(12_000)
        .clamp(500, 200_000);

    let response = state
        .http
        .get(url)
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,text/plain,application/json;q=0.9,*/*;q=0.1",
        )
        .send()
        .await
        .with_context(|| format!("Failed to fetch {}", url))?;

    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response
        .bytes()
        .await
        .context("Failed to read fetch response body")?;

    if !status.is_success() {
        let detail = String::from_utf8_lossy(&body)
            .chars()
            .take(280)
            .collect::<String>();
        bail!(
            "Fetch request failed with HTTP {} for {}: {}",
            status,
            final_url,
            detail
        );
    }

    let mut extracted = if content_type.contains("text/html")
        || content_type.contains("application/xhtml+xml")
        || looks_like_html(&body)
    {
        extract_page_from_html(&String::from_utf8_lossy(&body))?
    } else {
        ExtractedPage {
            title: None,
            text: normalize_text(&String::from_utf8(body.to_vec()).context(
                "Fetched response was not UTF-8 text. Only text and HTML content are supported.",
            )?),
            selector: None,
            js_heavy_hint: false,
        }
    };

    let should_try_browser = render_js
        || (auto_render_js && extracted.js_heavy_hint && extracted.text.chars().count() < 1200);
    let mut browser_attempted = false;
    let mut browser_used = false;

    if should_try_browser {
        browser_attempted = true;
        match BrowserService::render_page(&final_url).await {
            Ok(browser_text) => {
                let normalized = normalize_text(&browser_text);
                if normalized.chars().count() > extracted.text.chars().count() {
                    extracted.text = normalized;
                    extracted.selector = Some("browser_service".to_string());
                    browser_used = true;
                }
            }
            Err(error) => {
                warn!(
                    "WebTools browser fallback failed for {}: {}",
                    final_url, error
                );
            }
        }
    }

    let total_chars = extracted.text.chars().count();
    let truncated = total_chars > max_chars;
    let content = truncate_chars(&extracted.text, max_chars);

    Ok(json!({
        "url": final_url,
        "title": extracted.title,
        "content_type": if content_type.is_empty() { Value::Null } else { Value::String(content_type) },
        "content": content,
        "excerpt": excerpt(&extracted.text, 280),
        "word_count": count_words(&extracted.text),
        "selector": extracted.selector,
        "js_heavy_hint": extracted.js_heavy_hint,
        "browser_attempted": browser_attempted,
        "browser_used": browser_used,
        "truncated": truncated,
        "total_chars": total_chars,
    }))
}

async fn execute_search_tool(
    state: &WebToolsState,
    args: Value,
    forced_provider: Option<SearchProviderKind>,
) -> Result<Value> {
    let query = required_string(&args, "query")?;
    let limit = optional_usize(&args, "limit").unwrap_or(5).clamp(1, 10);
    let safe_search = optional_bool(&args, "safe_search").unwrap_or(false);
    let provider = match forced_provider.or(requested_provider(&args)?) {
        Some(provider) => provider,
        None => resolve_default_provider(&state.config)?,
    };

    let results = match provider {
        SearchProviderKind::Google => search_google(state, query, limit, safe_search).await?,
        SearchProviderKind::Brave => search_brave(state, query, limit).await?,
        SearchProviderKind::Searxng => search_searxng(state, query, limit, safe_search).await?,
    };

    Ok(json!({
        "provider": provider.as_str(),
        "query": query,
        "result_count": results.len(),
        "results": results,
    }))
}

async fn search_google(
    state: &WebToolsState,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<SearchResultItem>> {
    let config = state
        .config
        .google
        .as_ref()
        .context("Google search is not configured. Set SWARMCLAW_WEB_SEARCH_GOOGLE_API_KEY and SWARMCLAW_WEB_SEARCH_GOOGLE_CSE_ID.")?;
    let safe = if safe_search { "active" } else { "off" };

    let mut url = Url::parse("https://customsearch.googleapis.com/customsearch/v1")
        .context("Failed to build Google search URL")?;
    url.query_pairs_mut()
        .append_pair("key", &config.api_key)
        .append_pair("cx", &config.cse_id)
        .append_pair("q", query)
        .append_pair("num", &limit.min(10).to_string())
        .append_pair("safe", safe);

    let response = state
        .http
        .get(url)
        .send()
        .await
        .context("Failed to reach Google search API")?;
    let payload: GoogleSearchResponse = parse_json_response(response, "Google search API").await?;

    Ok(payload
        .items
        .into_iter()
        .take(limit)
        .map(|item| SearchResultItem {
            title: item.title,
            url: item.link,
            snippet: item.snippet.unwrap_or_default(),
            source: item.display_link,
        })
        .collect())
}

async fn search_brave(
    state: &WebToolsState,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResultItem>> {
    let config = state
        .config
        .brave
        .as_ref()
        .context("Brave search is not configured. Set SWARMCLAW_WEB_SEARCH_BRAVE_API_KEY.")?;

    let response = state
        .http
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", &config.api_key)
        .query(&[("q", query), ("count", &limit.to_string())])
        .send()
        .await
        .context("Failed to reach Brave search API")?;
    let payload: BraveSearchResponse = parse_json_response(response, "Brave search API").await?;

    Ok(payload
        .web
        .map(|web| web.results)
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .map(|item| SearchResultItem {
            title: item.title,
            url: item.url,
            snippet: item.description.unwrap_or_default(),
            source: Some("brave".to_string()),
        })
        .collect())
}

async fn search_searxng(
    state: &WebToolsState,
    query: &str,
    limit: usize,
    safe_search: bool,
) -> Result<Vec<SearchResultItem>> {
    let config =
        state.config.searxng.as_ref().context(
            "SearXNG search is not configured. Set SWARMCLAW_WEB_SEARCH_SEARXNG_BASE_URL.",
        )?;
    let mut url = searxng_search_url(&config.base_url)?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json")
        .append_pair("safesearch", if safe_search { "1" } else { "0" });

    let response = state
        .http
        .get(url)
        .send()
        .await
        .context("Failed to reach SearXNG instance")?;
    let payload: SearxngSearchResponse = parse_json_response(response, "SearXNG").await?;

    Ok(payload
        .results
        .into_iter()
        .take(limit)
        .map(|item| SearchResultItem {
            title: item.title,
            url: item.url,
            snippet: item.content.unwrap_or_default(),
            source: item.engine,
        })
        .collect())
}

fn supported_providers(config: &WebToolsConfig) -> Vec<SearchProviderStatus> {
    [
        SearchProviderKind::Google,
        SearchProviderKind::Brave,
        SearchProviderKind::Searxng,
    ]
    .into_iter()
    .map(|provider| SearchProviderStatus {
        provider: provider.as_str().to_string(),
        configured: provider_configured(config, provider),
        is_default: config.default_search_provider == Some(provider),
        description: provider.description(),
    })
    .collect()
}

fn provider_configured(config: &WebToolsConfig, provider: SearchProviderKind) -> bool {
    match provider {
        SearchProviderKind::Google => config.google.is_some(),
        SearchProviderKind::Brave => config.brave.is_some(),
        SearchProviderKind::Searxng => config.searxng.is_some(),
    }
}

fn resolve_default_provider(config: &WebToolsConfig) -> Result<SearchProviderKind> {
    if let Some(provider) = config.default_search_provider {
        if provider_configured(config, provider) {
            return Ok(provider);
        }
    }

    [SearchProviderKind::Google, SearchProviderKind::Brave, SearchProviderKind::Searxng]
        .into_iter()
        .find(|provider| provider_configured(config, *provider))
        .context(
            "No search providers are configured. Set Google, Brave, or SearXNG env vars before using web search.",
        )
}

fn searxng_search_url(base_url: &str) -> Result<Url> {
    let normalized = if base_url.ends_with("/search") {
        base_url.to_string()
    } else {
        format!("{}/search", base_url.trim_end_matches('/'))
    };
    Url::parse(&normalized).context("Failed to parse SearXNG base URL")
}

fn extract_page_from_html(html: &str) -> Result<ExtractedPage> {
    let document = Html::parse_document(html);
    let title = first_text(&document, &["meta[property=\"og:title\"]", "title"]);

    let candidate = candidate_html_fragment(&document)
        .or_else(|| first_html_fragment(&document, "body"))
        .unwrap_or_else(|| ("document".to_string(), html.to_string()));

    let text = normalize_text(
        &html2text::from_read(candidate.1.as_bytes(), 100)
            .context("Failed to convert fetched HTML into readable text")?,
    );
    let fallback_text =
        normalize_text(&document.root_element().text().collect::<Vec<_>>().join(" "));
    let final_text = if text.chars().count() >= 160 {
        text
    } else {
        fallback_text
    };

    Ok(ExtractedPage {
        title,
        text: final_text.clone(),
        selector: Some(candidate.0),
        js_heavy_hint: looks_js_heavy(html, &final_text),
    })
}

fn candidate_html_fragment(document: &Html) -> Option<(String, String)> {
    let candidates = [
        "article",
        "main",
        "[role=\"main\"]",
        "#content",
        ".content",
        ".post",
        ".article",
        ".main-content",
    ];

    let mut best: Option<(String, String, usize)> = None;
    for selector_raw in candidates {
        let Ok(selector) = Selector::parse(selector_raw) else {
            continue;
        };
        for element in document.select(&selector) {
            let text = normalize_text(&element.text().collect::<Vec<_>>().join(" "));
            let score = text.chars().count();
            if score < 160 {
                continue;
            }

            match &best {
                Some((_, _, best_score)) if *best_score >= score => {}
                _ => {
                    best = Some((selector_raw.to_string(), element.html(), score));
                }
            }
        }
    }

    best.map(|(selector, html, _)| (selector, html))
}

fn first_html_fragment(document: &Html, selector_raw: &str) -> Option<(String, String)> {
    let selector = Selector::parse(selector_raw).ok()?;
    document
        .select(&selector)
        .next()
        .map(|element| (selector_raw.to_string(), element.html()))
}

fn first_text(document: &Html, selectors: &[&str]) -> Option<String> {
    for selector_raw in selectors {
        let Ok(selector) = Selector::parse(selector_raw) else {
            continue;
        };
        if let Some(element) = document.select(&selector).next() {
            let value = if selector_raw.starts_with("meta[") {
                element
                    .value()
                    .attr("content")
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                normalize_text(&element.text().collect::<Vec<_>>().join(" "))
            };
            if !value.is_empty() {
                return Some(value);
            }
        }
    }

    None
}

fn looks_js_heavy(html: &str, extracted_text: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let script_count = lower.matches("<script").count();
    let markers = [
        "__next_data__",
        "id=\"__next\"",
        "data-reactroot",
        "window.__nuxt",
        "id=\"root\"",
        "id=\"app\"",
        "ng-version",
    ];

    extracted_text.chars().count() < 1200
        && (script_count > 8 || markers.iter().any(|marker| lower.contains(marker)))
}

fn looks_like_html(body: &[u8]) -> bool {
    let sample = String::from_utf8_lossy(body);
    let trimmed = sample.trim_start().to_ascii_lowercase();
    trimmed.starts_with("<!doctype html")
        || trimmed.starts_with("<html")
        || trimmed.contains("<body")
}

fn normalize_text(input: &str) -> String {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();

    for line in input.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = collapsed.trim();
        if trimmed.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
            continue;
        }
        current.push(trimmed.to_string());
    }

    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }

    paragraphs.join("\n\n").trim().to_string()
}

fn count_words(input: &str) -> usize {
    input.split_whitespace().count()
}

fn excerpt(input: &str, max_chars: usize) -> String {
    truncate_chars(input, max_chars)
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut output = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn mcp_success(structured: Value) -> Result<Value> {
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

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("Missing {}", key))
}

fn requested_provider(args: &Value) -> Result<Option<SearchProviderKind>> {
    match args.get("provider").and_then(Value::as_str) {
        Some(raw) => SearchProviderKind::parse(raw)
            .map(Some)
            .with_context(|| format!("Unsupported search provider '{}'", raw)),
        None => Ok(None),
    }
}

fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

async fn parse_json_response<T: DeserializeOwned>(
    response: reqwest::Response,
    label: &str,
) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("Failed to read {} response body", label))?;
    if !status.is_success() {
        bail!(
            "{} returned HTTP {}: {}",
            label,
            status,
            truncate_chars(&body, 280)
        );
    }

    serde_json::from_str(&body)
        .with_context(|| format!("Failed to decode {} response JSON: {}", label, body))
}

async fn json_result<T, F>(future: F) -> Result<T, (StatusCode, Json<Value>)>
where
    F: Future<Output = Result<T>>,
{
    match future.await {
        Ok(value) => Ok(value),
        Err(error) => {
            warn!("WebTools API error: {}", error);
            Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": error.to_string(),
                })),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_text_collapses_wrapped_lines() {
        let input = "Hello world\nfrom swarmclaw\n\nSecond paragraph\nhere";
        assert_eq!(
            normalize_text(input),
            "Hello world from swarmclaw\n\nSecond paragraph here"
        );
    }

    #[test]
    fn detects_js_heavy_shells() {
        let html = r#"<html><body><div id="__next"></div><script></script><script></script><script></script><script></script><script></script><script></script><script></script><script></script><script></script></body></html>"#;
        assert!(looks_js_heavy(html, "short"));
    }

    #[test]
    fn resolves_searxng_url() {
        let url = searxng_search_url("https://search.example.com").unwrap();
        assert_eq!(url.as_str(), "https://search.example.com/search");
    }
}
