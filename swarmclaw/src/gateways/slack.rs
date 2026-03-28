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
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, info_span, Instrument};

type HmacSha256 = Hmac<Sha256>;

struct SlackState {
    bot_token: String,
    signing_secret: String,
    agent_template: Arc<Agent>,
}

pub struct SlackWebhookGateway {
    port: u16,
    agent_template: Arc<Agent>,
}

#[derive(Deserialize, Debug)]
struct SlackEnvelope {
    #[serde(rename = "type")]
    kind: String,
    challenge: Option<String>,
    event_id: Option<String>,
    event: Option<SlackEvent>,
}

#[derive(Deserialize, Debug, Clone)]
struct SlackEvent {
    #[serde(rename = "type")]
    kind: String,
    channel: Option<String>,
    channel_type: Option<String>,
    user: Option<String>,
    text: Option<String>,
    ts: Option<String>,
    thread_ts: Option<String>,
    subtype: Option<String>,
    bot_id: Option<String>,
    files: Option<Vec<SlackFile>>,
}

#[derive(Deserialize, Debug, Clone)]
struct SlackFile {
    id: String,
    name: Option<String>,
    title: Option<String>,
    mimetype: Option<String>,
    url_private: Option<String>,
    url_private_download: Option<String>,
}

impl SlackWebhookGateway {
    pub fn new(agent_template: Arc<Agent>) -> Result<Self> {
        let port = std::env::var("SLACK_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8083".to_string())
            .parse()?;
        Ok(Self {
            port,
            agent_template,
        })
    }

    pub fn router(bot_token: String, signing_secret: String, agent_template: Arc<Agent>) -> Router {
        let state = Arc::new(SlackState {
            bot_token,
            signing_secret,
            agent_template,
        });

        Router::new()
            .route("/slack/events", post(handle_event))
            .with_state(state)
    }

    async fn run_server(&self) -> Result<()> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN").context("SLACK_BOT_TOKEN not set")?;
        let signing_secret =
            std::env::var("SLACK_SIGNING_SECRET").context("SLACK_SIGNING_SECRET not set")?;

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Starting Slack webhook server on {}", addr);

        let listener = TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            Self::router(bot_token, signing_secret, self.agent_template.clone()),
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl ChatGateway for SlackWebhookGateway {
    async fn start(&self) -> Result<()> {
        self.run_server().await
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}

async fn handle_event(
    State(state): State<Arc<SlackState>>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, StatusCode> {
    verify_slack_request(&headers, &body, &state.signing_secret)?;

    let envelope: SlackEnvelope =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    match envelope.kind.as_str() {
        "url_verification" => {
            let challenge = envelope.challenge.ok_or(StatusCode::BAD_REQUEST)?;
            Ok(challenge.into_response())
        }
        "event_callback" => {
            let event_id = envelope.event_id.ok_or(StatusCode::BAD_REQUEST)?;
            let Some(event) = envelope.event else {
                return Ok(StatusCode::OK.into_response());
            };

            let channel_id = event.channel.clone().unwrap_or_default();
            let request_span = info_span!(
                "gateway_ingress",
                request_id = %format!("slack-{}", event_id),
                platform = "slack",
                event_id = %event_id,
                channel_id = %channel_id,
                event_type = %event.kind
            );

            let is_new_event = mark_webhook_event_once("slack", &event_id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if !is_new_event {
                let _guard = request_span.enter();
                info!("Ignoring duplicate Slack event");
                return Ok(StatusCode::OK.into_response());
            }

            if !should_process_slack_event(&event) {
                let _guard = request_span.enter();
                info!("Ignoring unsupported Slack event");
                return Ok(StatusCode::OK.into_response());
            }

            {
                let _guard = request_span.enter();
                info!("Accepted Slack event");
            }

            let agent_template = state.agent_template.clone();
            let bot_token = state.bot_token.clone();
            tokio::spawn(
                async move {
                    process_slack_event(agent_template, bot_token, event).await;
                }
                .instrument(request_span),
            );

            Ok(StatusCode::OK.into_response())
        }
        _ => Ok(StatusCode::OK.into_response()),
    }
}

async fn process_slack_event(agent_template: Arc<Agent>, bot_token: String, event: SlackEvent) {
    let Some(channel_id) = event.channel.clone() else {
        error!("Slack event missing channel id");
        return;
    };
    let session_id = slack_session_id(&event);
    let mut agent = agent_template.spawn_session(session_id.clone());
    let prompt = match build_slack_prompt(&agent, &bot_token, &session_id, &event).await {
        Ok(prompt) => prompt,
        Err(error) => {
            error!(
                channel_id = %channel_id,
                "Failed to prepare Slack attachment context: {}",
                error
            );
            format!(
                "A Slack event was received, but attachment preparation failed: {}",
                error
            )
        }
    };

    let channel_info = slack_channel_info(&channel_id, &bot_token, &event);
    info!(
        channel_id = %channel_id,
        session_id = %session_id,
        "Processing Slack message through shared agent loop"
    );

    if let Err(error) = agent
        .handle_gateway_turn(&prompt, channel_info.clone())
        .await
    {
        error!(channel_id = %channel_id, "Slack gateway turn failed: {}", error);
        let _ = enqueue_gateway_text_message(
            "slack",
            &channel_id,
            &bot_token,
            None,
            channel_info.delivery_context.clone(),
            &format!("SwarmClaw error: {error}"),
        );
    }
}

fn verify_slack_request(
    headers: &HeaderMap,
    body: &[u8],
    signing_secret: &str,
) -> std::result::Result<(), StatusCode> {
    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let signature = headers
        .get("x-slack-signature")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let request_ts: i64 = timestamp.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    if (Utc::now().timestamp() - request_ts).abs() > 60 * 5 {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let provided = signature
        .strip_prefix("v0=")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let provided_bytes = hex::decode(provided).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes())
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    mac.verify_slice(&provided_bytes)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

fn should_process_slack_event(event: &SlackEvent) -> bool {
    if event.channel.is_none() || event.user.is_none() || event.bot_id.is_some() {
        return false;
    }

    match event.kind.as_str() {
        "app_mention" => true,
        "message" => {
            if event.channel_type.as_deref() != Some("im") {
                return false;
            }

            matches!(event.subtype.as_deref(), None | Some("file_share"))
        }
        _ => false,
    }
}

fn slack_channel_info(channel_id: &str, bot_token: &str, event: &SlackEvent) -> ChannelInfo {
    let mut channel_info = ChannelInfo::new("slack", channel_id, bot_token, None);
    if let Some(thread_ts) = slack_reply_thread_ts(event) {
        channel_info = channel_info.with_delivery_context(json!({
            "thread_ts": thread_ts,
        }));
    }
    channel_info
}

fn slack_reply_thread_ts(event: &SlackEvent) -> Option<String> {
    match event.kind.as_str() {
        "app_mention" => event.thread_ts.clone().or_else(|| event.ts.clone()),
        _ => event.thread_ts.clone(),
    }
}

fn slack_session_id(event: &SlackEvent) -> String {
    let channel_id = event.channel.as_deref().unwrap_or("unknown");
    if let Some(thread_ts) = slack_reply_thread_ts(event) {
        format!(
            "slack-{}-thread-{}",
            channel_id,
            normalize_ts_component(&thread_ts)
        )
    } else {
        format!("slack-{}", channel_id)
    }
}

fn normalize_ts_component(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

async fn build_slack_prompt(
    agent: &Agent,
    bot_token: &str,
    session_id: &str,
    event: &SlackEvent,
) -> Result<String> {
    let mut sections = Vec::new();

    if let Some(text) = event.text.as_deref() {
        let trimmed = if event.kind == "app_mention" {
            strip_slack_mentions(text)
        } else {
            text.trim().to_string()
        };
        if !trimmed.is_empty() {
            sections.push(trimmed);
        }
    }

    if let Some(files) = event.files.as_ref() {
        for file in files {
            let stored = stage_slack_attachment(agent, bot_token, session_id, file).await?;
            sections.push(render_slack_file_section(file, stored.as_ref()));
        }
    }

    if sections.is_empty() {
        sections.push("A Slack event was received without text.".to_string());
    }

    Ok(sections.join("\n\n"))
}

async fn stage_slack_attachment(
    agent: &Agent,
    bot_token: &str,
    session_id: &str,
    file: &SlackFile,
) -> Result<Option<StoredAttachment>> {
    let Some(source_url) = file
        .url_private_download
        .as_deref()
        .or(file.url_private.as_deref())
    else {
        return Ok(None);
    };

    let file_name = file
        .name
        .as_deref()
        .or(file.title.as_deref())
        .unwrap_or("slack-file");
    let auth_header = [("Authorization".to_string(), format!("Bearer {}", bot_token))];

    download_attachment_to_workspace(
        agent,
        "slack",
        session_id,
        file_name,
        source_url,
        &auth_header,
    )
    .await
}

fn render_slack_file_section(file: &SlackFile, stored: Option<&StoredAttachment>) -> String {
    let label = file
        .name
        .as_deref()
        .or(file.title.as_deref())
        .unwrap_or("slack-file");
    let mut lines = vec![format!("Slack attachment: {}", label)];
    lines.push(format!("file_id: {}", file.id));

    if let Some(mimetype) = file.mimetype.as_deref() {
        lines.push(format!("mime_type: {}", mimetype));
    }

    if let Some(stored) = stored {
        lines.push(format!("workspace_path: {}", stored.relative_path));
    } else {
        lines.push("workspace_path: unavailable".to_string());
    }

    lines.join("\n")
}

fn strip_slack_mentions(text: &str) -> String {
    text.split_whitespace()
        .filter(|part| !(part.starts_with("<@") && part.ends_with('>')))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateways::test_support::{test_agent_template, wait_for_outbox_message};
    use crate::outbox::{list_outbox_messages, reset_local_db_for_tests, test_db_lock};
    use anyhow::Result;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn accepts_signed_app_mention_and_dedupes_replay() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        let body = serde_json::json!({
            "type": "event_callback",
            "event_id": "Ev_slack_123",
            "event": {
                "type": "app_mention",
                "channel": "C123",
                "user": "U123",
                "text": "<@Ubot> hello from slack",
                "ts": "1712345678.1234"
            }
        })
        .to_string();
        let timestamp = Utc::now().timestamp().to_string();
        let signature = slack_signature("slack-secret", &timestamp, &body);

        let router = SlackWebhookGateway::router(
            "xoxb-test".to_string(),
            "slack-secret".to_string(),
            test_agent_template(),
        );

        let request = || {
            Request::builder()
                .method("POST")
                .uri("/slack/events")
                .header("content-type", "application/json")
                .header("x-slack-request-timestamp", timestamp.as_str())
                .header("x-slack-signature", signature.as_str())
                .body(Body::from(body.clone()))
        };

        let first = router.clone().oneshot(request()?).await?;
        assert_eq!(first.status(), StatusCode::OK);
        let message = wait_for_outbox_message("slack", "C123").await?;
        assert!(message.payload_preview.contains("gateway ok"));

        let second = router.oneshot(request()?).await?;
        assert_eq!(second.status(), StatusCode::OK);

        let slack_messages = list_outbox_messages(Some("pending"), 20)?
            .into_iter()
            .filter(|message| message.platform == "slack" && message.channel_id == "C123")
            .count();
        assert_eq!(slack_messages, 1);

        Ok(())
    }

    fn slack_signature(secret: &str, timestamp: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac");
        mac.update(format!("v0:{timestamp}:{body}").as_bytes());
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }
}
