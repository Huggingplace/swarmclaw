use anyhow::{Result, Context};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use ed25519_dalek::{Verifier, VerifyingKey, Signature};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpListener;
use std::collections::HashMap;

// Discord Interaction Types
const PING: u8 = 1;
const APPLICATION_COMMAND: u8 = 2;

#[derive(Deserialize, Debug)]
pub struct Interaction {
    #[serde(rename = "type")]
    pub kind: u8,
    pub token: Option<String>,
    pub application_id: Option<String>,
    pub channel_id: Option<String>,
    pub data: Option<InteractionData>,
}

#[derive(Deserialize, Debug)]
pub struct InteractionData {
    pub resolved: Option<ResolvedData>,
}

#[derive(Deserialize, Debug)]
pub struct ResolvedData {
    pub attachments: Option<HashMap<String, Attachment>>,
}

#[derive(Deserialize, Debug)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub url: String,
}

#[derive(Serialize)]
pub struct InteractionResponse {
    #[serde(rename = "type")]
    pub kind: u8,
}

struct DiscordState {
    public_key: VerifyingKey,
}

pub struct DiscordWebhookGateway {
    port: u16,
}

impl DiscordWebhookGateway {
    pub fn new() -> Result<Self> {
        let port = std::env::var("DISCORD_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8081".to_string())
            .parse()?;
        Ok(Self { port })
    }

    pub async fn start(&self) -> Result<()> {
        let pk_hex = std::env::var("DISCORD_PUBLIC_KEY").context("DISCORD_PUBLIC_KEY not set")?;
        let pk_bytes = hex::decode(pk_hex).context("Invalid hex in DISCORD_PUBLIC_KEY")?;
        let public_key = VerifyingKey::try_from(pk_bytes.as_slice()).context("Invalid ed25519 public key")?;

        let state = Arc::new(DiscordState { public_key });

        let app = Router::new()
            .route("/discord/interactions", post(handle_interaction))
            .with_state(state);

        let addr = format!("0.0.0.0:{}", self.port);
        println!("Starting Discord Webhook server on {}", addr);
        
        let listener = TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        
        Ok(())
    }
}

async fn handle_interaction(
    State(state): State<Arc<DiscordState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    // 1. Verify Signature
    let signature_hex = headers.get("x-signature-ed25519")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
        
    let timestamp = headers.get("x-signature-timestamp")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let sig_bytes = hex::decode(signature_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let mut msg = timestamp.as_bytes().to_vec();
    msg.extend_from_slice(&body);

    if state.public_key.verify(&msg, &signature).is_err() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 2. Parse Interaction
    let interaction: Interaction = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // 3. Handle Ping
    if interaction.kind == PING {
        return Ok(Json(InteractionResponse { kind: 1 })); // PONG
    }

    // 4. Handle Application Command
    if interaction.kind == APPLICATION_COMMAND {
        // Immediately acknowledge with "Thinking..." to beat the 3-second timeout
        let response = InteractionResponse { kind: 5 }; // DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
        
        let token = interaction.token.clone().unwrap_or_default();
        let app_id = interaction.application_id.clone().unwrap_or_default();
        let channel_id = interaction.channel_id.clone().unwrap_or_default();
        
        // Extract attachment URLs if any
        let mut attachment_urls = Vec::new();
        if let Some(data) = interaction.data {
            if let Some(resolved) = data.resolved {
                if let Some(attachments) = resolved.attachments {
                    for att in attachments.values() {
                        attachment_urls.push((att.filename.clone(), att.url.clone()));
                    }
                }
            }
        }
        
        // 5. Spawn background task to process the LLM request
        tokio::spawn(async move {
            process_discord_message(app_id, token, channel_id, attachment_urls).await;
        });

        return Ok(Json(response));
    }

    Err(StatusCode::BAD_REQUEST)
}

async fn process_discord_message(app_id: String, token: String, channel_id: String, attachments: Vec<(String, String)>) {
    println!("Processing message in background...");
    
    let client = reqwest::Client::new();
    
    // --- RECEIVING FILES ---
    // If the user uploaded files, we download them using reqwest
    for (filename, url) in &attachments {
        println!("Downloading attached file: {} from {}", filename, url);
        if let Ok(response) = client.get(url).send().await {
            if let Ok(bytes) = response.bytes().await {
                println!("Downloaded {} bytes for {}", bytes.len(), filename);
                // Save to disk or pass to LLM (e.g. vision model)
            }
        }
    }
    
    // Simulate thinking delay
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    let answer = "Hello! I am SwarmClaw. I have finished processing your request and analyzing your files.";
    
    // We queue the message into our robust local SQLite Outbox instead of sending it directly
    let json_payload = serde_json::json!({
        "content": answer
    }).to_string();

    let msg = crate::outbox::OutboxMessage {
        id: uuid::Uuid::new_v4().to_string(),
        platform: "discord".to_string(),
        channel_id: channel_id.clone(),
        token: token.clone(),
        app_id: Some(app_id.clone()),
        payload: json_payload,
        ui_components: None,
        created_at: chrono::Utc::now().timestamp_millis(),
        sync_status: "pending".to_string(),
    };

    if let Err(e) = crate::outbox::enqueue_message(msg) {
        println!("Failed to enqueue Discord message: {}", e);
    } else {
        println!("Successfully enqueued Discord message for channel {}", channel_id);
    }
}
