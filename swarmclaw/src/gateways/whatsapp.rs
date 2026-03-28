use crate::core::{agent::ChannelInfo, Agent};
use crate::gateways::common::{
    download_attachment_to_workspace, mark_webhook_event_once, StoredAttachment,
};
use crate::gateways::ChatGateway;
use crate::outbox::enqueue_gateway_text_message;
use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use serde_urlencoded;
use sha1::Sha1;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, info_span, Instrument};

type HmacSha1 = Hmac<Sha1>;

struct WhatsAppState {
    account_sid: String,
    auth_token: String,
    default_from: Option<String>,
    webhook_url: Option<String>,
    agent_template: Arc<Agent>,
}

pub struct WhatsAppWebhookGateway {
    port: u16,
    agent_template: Arc<Agent>,
}

impl WhatsAppWebhookGateway {
    pub fn new(agent_template: Arc<Agent>) -> Result<Self> {
        let port = std::env::var("WHATSAPP_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8084".to_string())
            .parse()?;
        Ok(Self {
            port,
            agent_template,
        })
    }

    pub fn router(
        account_sid: String,
        auth_token: String,
        default_from: Option<String>,
        webhook_url: Option<String>,
        agent_template: Arc<Agent>,
    ) -> Router {
        let state = Arc::new(WhatsAppState {
            account_sid,
            auth_token,
            default_from,
            webhook_url,
            agent_template,
        });

        Router::new()
            .route("/twilio/whatsapp", post(handle_webhook))
            .with_state(state)
    }

    async fn run_server(&self) -> Result<()> {
        let account_sid =
            std::env::var("TWILIO_ACCOUNT_SID").context("TWILIO_ACCOUNT_SID not set")?;
        let auth_token = std::env::var("TWILIO_AUTH_TOKEN").context("TWILIO_AUTH_TOKEN not set")?;
        let default_from = std::env::var("TWILIO_WHATSAPP_FROM").ok();
        let webhook_url = std::env::var("WHATSAPP_WEBHOOK_URL").ok();

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Starting WhatsApp Twilio webhook server on {}", addr);

        let listener = TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            Self::router(
                account_sid,
                auth_token,
                default_from,
                webhook_url,
                self.agent_template.clone(),
            ),
        )
        .await?;

        Ok(())
    }
}

#[async_trait]
impl ChatGateway for WhatsAppWebhookGateway {
    async fn start(&self) -> Result<()> {
        self.run_server().await
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}

async fn handle_webhook(
    State(state): State<Arc<WhatsAppState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, StatusCode> {
    let params = parse_form_body(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let request_url = resolve_request_url(&state, &headers, &uri);
    verify_twilio_signature(&headers, &request_url, &params, &state.auth_token)?;

    let message_sid = params
        .get("MessageSid")
        .cloned()
        .or_else(|| params.get("SmsMessageSid").cloned())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let from = params.get("From").cloned().unwrap_or_default();
    let to = params
        .get("To")
        .cloned()
        .or_else(|| state.default_from.clone())
        .unwrap_or_default();

    let request_span = info_span!(
        "gateway_ingress",
        request_id = %format!("whatsapp-{}", message_sid),
        platform = "whatsapp",
        message_sid = %message_sid,
        from = %from,
        to = %to
    );

    let is_new_event = mark_webhook_event_once("whatsapp", &message_sid)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !is_new_event {
        let _guard = request_span.enter();
        info!("Ignoring duplicate WhatsApp webhook");
        return Ok(empty_twiml_response());
    }

    if !should_process_whatsapp_message(&params) {
        let _guard = request_span.enter();
        info!("Ignoring unsupported WhatsApp webhook payload");
        return Ok(empty_twiml_response());
    }

    {
        let _guard = request_span.enter();
        info!("Accepted WhatsApp webhook");
    }

    let agent_template = state.agent_template.clone();
    let account_sid = state.account_sid.clone();
    let auth_token = state.auth_token.clone();
    let default_from = state.default_from.clone();
    tokio::spawn(
        async move {
            process_whatsapp_message(
                agent_template,
                account_sid,
                auth_token,
                default_from,
                params,
            )
            .await;
        }
        .instrument(request_span),
    );

    Ok(empty_twiml_response())
}

async fn process_whatsapp_message(
    agent_template: Arc<Agent>,
    account_sid: String,
    auth_token: String,
    default_from: Option<String>,
    params: HashMap<String, String>,
) {
    let from = params.get("From").cloned().unwrap_or_default();
    let to = params
        .get("To")
        .cloned()
        .or(default_from)
        .unwrap_or_default();
    let session_id = format!("whatsapp-{}", normalize_sender_id(&from));
    let mut agent = agent_template.spawn_session(session_id.clone());

    let prompt = match build_whatsapp_prompt(
        &agent,
        &account_sid,
        &auth_token,
        &session_id,
        &params,
    )
    .await
    {
        Ok(prompt) => prompt,
        Err(error) => {
            error!(from = %from, "Failed to prepare WhatsApp media context: {}", error);
            format!(
                "A WhatsApp message was received, but media preparation failed: {}",
                error
            )
        }
    };

    let channel_info = ChannelInfo::new(
        "whatsapp",
        from.clone(),
        auth_token.clone(),
        Some(account_sid.clone()),
    )
    .with_delivery_context(serde_json::json!({
        "From": to,
    }));

    info!(
        session_id = %session_id,
        sender = %from,
        "Processing WhatsApp message through shared agent loop"
    );

    if let Err(error) = agent
        .handle_gateway_turn(&prompt, channel_info.clone())
        .await
    {
        error!(sender = %from, "WhatsApp gateway turn failed: {}", error);
        let _ = enqueue_gateway_text_message(
            "whatsapp",
            &from,
            &auth_token,
            Some(account_sid),
            channel_info.delivery_context.clone(),
            &format!("SwarmClaw error: {error}"),
        );
    }
}

fn parse_form_body(body: &[u8]) -> Result<HashMap<String, String>> {
    let pairs = serde_urlencoded::from_bytes::<Vec<(String, String)>>(body)?;
    Ok(pairs.into_iter().collect())
}

fn resolve_request_url(
    state: &WhatsAppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> String {
    if let Some(webhook_url) = state.webhook_url.as_deref() {
        return webhook_url.to_string();
    }

    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{}{}", scheme, host, uri)
}

fn verify_twilio_signature(
    headers: &HeaderMap,
    request_url: &str,
    params: &HashMap<String, String>,
    auth_token: &str,
) -> std::result::Result<(), StatusCode> {
    let signature = headers
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let provided = BASE64
        .decode(signature)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let mut entries = params.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut data = request_url.to_string();
    for (key, value) in entries {
        data.push_str(key);
        data.push_str(value);
    }

    let mut mac =
        HmacSha1::new_from_slice(auth_token.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(data.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

fn should_process_whatsapp_message(params: &HashMap<String, String>) -> bool {
    let Some(from) = params.get("From") else {
        return false;
    };
    if !from.starts_with("whatsapp:") {
        return false;
    }

    params.get("MessageStatus").is_none()
}

async fn build_whatsapp_prompt(
    agent: &Agent,
    account_sid: &str,
    auth_token: &str,
    session_id: &str,
    params: &HashMap<String, String>,
) -> Result<String> {
    let mut sections = Vec::new();

    if let Some(body) = params.get("Body") {
        let trimmed = body.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_string());
        }
    }

    for media in whatsapp_media_items(params) {
        let stored =
            stage_whatsapp_attachment(agent, account_sid, auth_token, session_id, &media).await?;
        sections.push(render_whatsapp_media_section(&media, stored.as_ref()));
    }

    if sections.is_empty() {
        sections.push("A WhatsApp message was received without text.".to_string());
    }

    Ok(sections.join("\n\n"))
}

#[derive(Debug, Clone)]
struct WhatsAppMedia {
    url: String,
    content_type: Option<String>,
    filename: String,
}

fn whatsapp_media_items(params: &HashMap<String, String>) -> Vec<WhatsAppMedia> {
    let count = params
        .get("NumMedia")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut items = Vec::new();

    for index in 0..count {
        let Some(url) = params.get(&format!("MediaUrl{}", index)).cloned() else {
            continue;
        };
        let content_type = params.get(&format!("MediaContentType{}", index)).cloned();
        let extension = content_type
            .as_deref()
            .and_then(media_extension)
            .unwrap_or("bin");

        items.push(WhatsAppMedia {
            url,
            content_type,
            filename: format!("whatsapp-media-{}.{}", index, extension),
        });
    }

    items
}

async fn stage_whatsapp_attachment(
    agent: &Agent,
    account_sid: &str,
    auth_token: &str,
    session_id: &str,
    media: &WhatsAppMedia,
) -> Result<Option<StoredAttachment>> {
    let basic = BASE64.encode(format!("{}:{}", account_sid, auth_token));
    let auth_header = [("Authorization".to_string(), format!("Basic {}", basic))];

    download_attachment_to_workspace(
        agent,
        "whatsapp",
        session_id,
        &media.filename,
        &media.url,
        &auth_header,
    )
    .await
}

fn render_whatsapp_media_section(
    media: &WhatsAppMedia,
    stored: Option<&StoredAttachment>,
) -> String {
    let mut lines = vec![format!("WhatsApp attachment: {}", media.filename)];
    if let Some(content_type) = media.content_type.as_deref() {
        lines.push(format!("mime_type: {}", content_type));
    }
    if let Some(stored) = stored {
        lines.push(format!("workspace_path: {}", stored.relative_path));
    } else {
        lines.push("workspace_path: unavailable".to_string());
    }
    lines.join("\n")
}

fn media_extension(content_type: &str) -> Option<&'static str> {
    match content_type {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "audio/ogg" => Some("ogg"),
        "audio/mpeg" => Some("mp3"),
        "video/mp4" => Some("mp4"),
        "application/pdf" => Some("pdf"),
        _ => None,
    }
}

fn normalize_sender_id(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn empty_twiml_response() -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/xml; charset=utf-8")],
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateways::test_support::{test_agent_template, wait_for_outbox_message};
    use crate::outbox::{reset_local_db_for_tests, test_db_lock};
    use anyhow::Result;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn accepts_signed_whatsapp_webhook_and_enqueues_reply() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        let params = vec![
            ("AccountSid".to_string(), "AC123".to_string()),
            ("MessageSid".to_string(), "SM123".to_string()),
            ("From".to_string(), "whatsapp:+15550002222".to_string()),
            ("To".to_string(), "whatsapp:+15550001111".to_string()),
            ("Body".to_string(), "hello from whatsapp".to_string()),
            ("NumMedia".to_string(), "0".to_string()),
        ];
        let body = serde_urlencoded::to_string(&params)?;
        let signature =
            twilio_signature("https://example.com/twilio/whatsapp", &params, "auth-token");

        let response = WhatsAppWebhookGateway::router(
            "AC123".to_string(),
            "auth-token".to_string(),
            Some("whatsapp:+15550001111".to_string()),
            Some("https://example.com/twilio/whatsapp".to_string()),
            test_agent_template(),
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/twilio/whatsapp")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("x-twilio-signature", signature)
                .body(Body::from(body))?,
        )
        .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        assert!(std::str::from_utf8(&body)?.contains("<Response></Response>"));

        let message = wait_for_outbox_message("whatsapp", "whatsapp:+15550002222").await?;
        assert!(message.payload_preview.contains("gateway ok"));
        Ok(())
    }

    fn twilio_signature(
        request_url: &str,
        params: &[(String, String)],
        auth_token: &str,
    ) -> String {
        let mut sorted = params.iter().collect::<Vec<_>>();
        sorted.sort_by(|(left_key, _), (right_key, _)| left_key.cmp(right_key));

        let mut data = request_url.to_string();
        for (key, value) in sorted {
            data.push_str(key);
            data.push_str(value);
        }

        let mut mac = HmacSha1::new_from_slice(auth_token.as_bytes()).expect("hmac");
        mac.update(data.as_bytes());
        BASE64.encode(mac.finalize().into_bytes())
    }
}
