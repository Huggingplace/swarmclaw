use crate::core::{agent::ChannelInfo, Agent};
use crate::gateways::common::{
    download_attachment_to_workspace, mark_webhook_event_once, StoredAttachment,
};
use crate::gateways::ChatGateway;
use crate::outbox::enqueue_gateway_text_message;
use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, info_span, Instrument};

#[derive(Deserialize, Debug)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
}

#[derive(Deserialize, Debug)]
pub struct TelegramMessage {
    pub chat: TelegramChat,
    pub text: Option<String>,
    pub document: Option<TelegramDocument>,
    pub photo: Option<Vec<TelegramPhotoSize>>,
}

#[derive(Deserialize, Debug)]
pub struct TelegramChat {
    pub id: i64,
}

#[derive(Deserialize, Debug)]
pub struct TelegramDocument {
    pub file_id: String,
    pub file_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct TelegramPhotoSize {
    pub file_id: String,
    pub width: u32,
    pub height: u32,
    pub file_size: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct TelegramFileResponse {
    pub ok: bool,
    pub result: Option<TelegramFile>,
}

#[derive(Deserialize, Debug)]
struct TelegramFile {
    pub file_path: String,
}

struct TelegramState {
    token: String,
    secret_token: Option<String>,
    agent_template: Arc<Agent>,
}

pub struct TelegramWebhookGateway {
    port: u16,
    agent_template: Arc<Agent>,
}

impl TelegramWebhookGateway {
    pub fn new(agent_template: Arc<Agent>) -> Result<Self> {
        let port = std::env::var("TELEGRAM_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8082".to_string())
            .parse()?;
        Ok(Self {
            port,
            agent_template,
        })
    }

    pub fn router(
        token: String,
        secret_token: Option<String>,
        agent_template: Arc<Agent>,
    ) -> Router {
        let state = Arc::new(TelegramState {
            token: token.clone(),
            secret_token,
            agent_template,
        });
        let secret_path = format!("/telegram/{}", token);

        Router::new()
            .route(&secret_path, post(handle_update))
            .with_state(state)
    }

    async fn run_server(&self) -> Result<()> {
        let token = std::env::var("TELEGRAM_TOKEN").context("TELEGRAM_TOKEN not set")?;
        let secret_token = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok();

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Starting Telegram webhook server on {}", addr);

        let listener = TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            Self::router(token, secret_token, self.agent_template.clone()),
        )
        .await?;

        Ok(())
    }
}

#[async_trait]
impl ChatGateway for TelegramWebhookGateway {
    async fn start(&self) -> Result<()> {
        self.run_server().await
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}

async fn handle_update(
    State(state): State<Arc<TelegramState>>,
    headers: HeaderMap,
    Json(update): Json<TelegramUpdate>,
) -> Result<impl IntoResponse, StatusCode> {
    let chat_id_for_span = update
        .message
        .as_ref()
        .map(|message| message.chat.id.to_string())
        .unwrap_or_default();
    let request_span = info_span!(
        "gateway_ingress",
        request_id = %format!("telegram-{}", update.update_id),
        platform = "telegram",
        update_id = update.update_id,
        chat_id = %chat_id_for_span
    );

    if let Some(expected_secret) = state.secret_token.as_deref() {
        let header_secret = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|value| value.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;

        if header_secret != expected_secret {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let is_new_event = mark_webhook_event_once("telegram", &update.update_id.to_string())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !is_new_event {
        let _guard = request_span.enter();
        info!("Ignoring duplicate Telegram update");
        return Ok(StatusCode::OK);
    }

    if let Some(message) = update.message {
        let chat_id = message.chat.id;
        let token = state.token.clone();
        let agent_template = state.agent_template.clone();

        {
            let _guard = request_span.enter();
            info!("Accepted Telegram update");
        }

        tokio::spawn(
            async move {
                process_telegram_message(agent_template, token, chat_id, message).await;
            }
            .instrument(request_span),
        );
    } else {
        let _guard = request_span.enter();
        info!("Ignoring Telegram update without a message body");
    }

    Ok(StatusCode::OK)
}

async fn process_telegram_message(
    agent_template: Arc<Agent>,
    token: String,
    chat_id: i64,
    message: TelegramMessage,
) {
    let session_id = format!("telegram-{}", chat_id);
    let mut agent = agent_template.spawn_session(session_id);
    let prompt = match build_telegram_prompt(&agent, &token, &message).await {
        Ok(prompt) => prompt,
        Err(error) => {
            error!(
                chat_id = chat_id,
                "Failed to prepare Telegram attachment context: {}", error
            );
            format!(
                "A Telegram message was received, but attachment preparation failed: {}",
                error
            )
        }
    };

    info!(
        chat_id = chat_id,
        "Processing Telegram message through shared agent loop"
    );

    if let Err(error) = agent
        .handle_gateway_turn(
            &prompt,
            ChannelInfo::new("telegram", chat_id.to_string(), token.clone(), None),
        )
        .await
    {
        error!(chat_id = chat_id, "Telegram gateway turn failed: {}", error);
        let _ = enqueue_gateway_text_message(
            "telegram",
            &chat_id.to_string(),
            &token,
            None,
            None,
            &format!("SwarmClaw error: {error}"),
        );
    }
}

async fn build_telegram_prompt(
    agent: &Agent,
    token: &str,
    message: &TelegramMessage,
) -> Result<String> {
    let mut sections = Vec::new();
    let session_id = format!("telegram-{}", message.chat.id);

    if let Some(text) = message.text.as_deref() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_string());
        }
    }

    if let Some(document) = message.document.as_ref() {
        let filename = document.file_name.as_deref().unwrap_or("telegram-document");
        let stored =
            stage_telegram_attachment(agent, token, &session_id, &document.file_id, filename)
                .await?;
        sections.push(render_document_section(filename, stored.as_ref()));
    }

    if let Some(photos) = message.photo.as_ref() {
        if let Some(photo) = best_telegram_photo(photos) {
            let filename = format!("telegram-photo-{}x{}.jpg", photo.width, photo.height);
            let stored =
                stage_telegram_attachment(agent, token, &session_id, &photo.file_id, &filename)
                    .await?;
            sections.push(render_photo_section(photos.len(), stored.as_ref()));
        }
    }

    if sections.is_empty() {
        Ok("A Telegram message was received without any text body.".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

async fn stage_telegram_attachment(
    agent: &Agent,
    token: &str,
    session_id: &str,
    file_id: &str,
    suggested_name: &str,
) -> Result<Option<StoredAttachment>> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await
        .with_context(|| format!("failed to fetch Telegram file metadata for {}", file_id))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Telegram getFile failed with status {} for attachment {}",
            response.status(),
            file_id
        );
    }

    let file_response = response
        .json::<TelegramFileResponse>()
        .await
        .context("failed to parse Telegram getFile response")?;
    if !file_response.ok {
        anyhow::bail!(
            "Telegram getFile returned ok=false for attachment {}",
            file_id
        );
    }

    let file = file_response
        .result
        .context("Telegram getFile response did not include a file path")?;
    let derived_name = Path::new(&file.file_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(suggested_name);
    let source_url = format!(
        "https://api.telegram.org/file/bot{token}/{}",
        file.file_path.trim_start_matches('/')
    );

    download_attachment_to_workspace(
        agent,
        "telegram",
        session_id,
        derived_name,
        &source_url,
        &[],
    )
    .await
}

fn render_document_section(filename: &str, stored: Option<&StoredAttachment>) -> String {
    match stored {
        Some(stored) => format!(
            "Attached document: {filename}\nSaved attachment in workspace: {}",
            stored.relative_path
        ),
        None => format!("Attached document: {filename}"),
    }
}

fn render_photo_section(photo_count: usize, stored: Option<&StoredAttachment>) -> String {
    match stored {
        Some(stored) => format!(
            "Attached {photo_count} photo(s).\nSaved highest-resolution photo in workspace: {}",
            stored.relative_path
        ),
        None => format!("Attached {photo_count} photo(s)."),
    }
}

fn best_telegram_photo(photos: &[TelegramPhotoSize]) -> Option<&TelegramPhotoSize> {
    photos.iter().max_by_key(|photo| {
        photo
            .file_size
            .unwrap_or((photo.width as u64) * (photo.height as u64))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateways::test_support::{test_agent_template, wait_for_outbox_message};
    use crate::outbox::{reset_local_db_for_tests, test_db_lock};
    use anyhow::Result;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn accepts_signed_update_and_enqueues_reply() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        let response = TelegramWebhookGateway::router(
            "telegram-token".to_string(),
            Some("telegram-secret".to_string()),
            test_agent_template(),
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/telegram/telegram-token")
                .header("content-type", "application/json")
                .header("x-telegram-bot-api-secret-token", "telegram-secret")
                .body(Body::from(
                    serde_json::json!({
                        "update_id": 101,
                        "message": {
                            "chat": { "id": 42 },
                            "text": "hello from telegram"
                        }
                    })
                    .to_string(),
                ))?,
        )
        .await?;

        assert_eq!(response.status(), StatusCode::OK);

        let message = wait_for_outbox_message("telegram", "42").await?;
        assert!(message.payload_preview.contains("gateway ok"));
        Ok(())
    }
}
