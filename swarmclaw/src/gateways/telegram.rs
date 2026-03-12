use anyhow::{Result, Context};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Deserialize, Debug)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
}

#[derive(Deserialize, Debug)]
pub struct TelegramMessage {
    pub message_id: i64,
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
    pub file_size: Option<i64>,
}

#[derive(Deserialize, Debug)]
struct TelegramFileResponse {
    ok: bool,
    result: Option<TelegramFile>,
}

#[derive(Deserialize, Debug)]
struct TelegramFile {
    file_path: String,
}

struct TelegramState {
    token: String,
}

pub struct TelegramWebhookGateway {
    port: u16,
}

impl TelegramWebhookGateway {
    pub fn new() -> Result<Self> {
        let port = std::env::var("TELEGRAM_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8082".to_string())
            .parse()?;
        Ok(Self { port })
    }

    pub async fn start(&self) -> Result<()> {
        let token = std::env::var("TELEGRAM_TOKEN").context("TELEGRAM_TOKEN not set")?;
        let state = Arc::new(TelegramState { token });
        let secret_path = format!("/telegram/{}", state.token);
        
        let app = Router::new()
            .route(&secret_path, post(handle_update))
            .with_state(state);

        let addr = format!("0.0.0.0:{}", self.port);
        println!("Starting Telegram Webhook server on {}", addr);
        
        let listener = TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        
        Ok(())
    }
}

async fn handle_update(
    State(state): State<Arc<TelegramState>>,
    Json(update): Json<TelegramUpdate>,
) -> Result<impl IntoResponse, StatusCode> {
    if let Some(msg) = update.message {
        let chat_id = msg.chat.id;
        let token = state.token.clone();
        
        // Spawn background task so we immediately return HTTP 200 OK
        tokio::spawn(async move {
            process_telegram_message(token, chat_id, msg).await;
        });
    }

    Ok(StatusCode::OK)
}

async fn process_telegram_message(token: String, chat_id: i64, msg: TelegramMessage) {
    let client = reqwest::Client::new();
    
    // --- RECEIVING FILES ---
    // If the user uploaded a document, we must first fetch the file path, then download it
    if let Some(doc) = msg.document {
        println!("Received document: {:?}", doc.file_name);
        
        // 1. Get File Path
        let get_file_url = format!("https://api.telegram.org/bot{}/getFile?file_id={}", token, doc.file_id);
        if let Ok(res) = client.get(&get_file_url).send().await {
            if let Ok(file_info) = res.json::<TelegramFileResponse>().await {
                if let Some(result) = file_info.result {
                    // 2. Download File
                    let download_url = format!("https://api.telegram.org/file/bot{}/{}", token, result.file_path);
                    if let Ok(file_res) = client.get(&download_url).send().await {
                        if let Ok(bytes) = file_res.bytes().await {
                            println!("Downloaded {} bytes from Telegram", bytes.len());
                            // Process file here...
                        }
                    }
                }
            }
        }
    }

    // Simulate thinking delay
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    let text_content = msg.text.unwrap_or_else(|| "a file".to_string());
    let answer = format!("You sent: {}. I am SwarmClaw running via a Webhook.", text_content);
    
    // We queue the text message into our robust local SQLite Outbox
    let json_payload = serde_json::json!({
        "chat_id": chat_id,
        "text": answer
    }).to_string();

    let msg_obj = crate::outbox::OutboxMessage {
        id: uuid::Uuid::new_v4().to_string(),
        platform: "telegram".to_string(),
        channel_id: chat_id.to_string(),
        token: token.clone(),
        app_id: None,
        payload: json_payload,
        ui_components: None,
        created_at: chrono::Utc::now().timestamp_millis(),
        sync_status: "pending".to_string(),
    };

    if let Err(e) = crate::outbox::enqueue_message(msg_obj) {
        println!("Failed to enqueue Telegram message: {}", e);
    } else {
        println!("Successfully enqueued Telegram message for chat {}", chat_id);
    }
}
