use anyhow::{Result, Context};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tracing::{info, error, warn, debug};
use uuid::Uuid;
use crate::security::Redactor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxMessage {
    pub id: String,
    pub platform: String, // "discord", "telegram", or "internal"
    pub channel_id: String,
    pub token: String,
    pub app_id: Option<String>,
    pub payload: String, // JSON payload representing the message to send
    pub ui_components: Option<serde_json::Value>, // Interactive elements (buttons, menus)
    pub created_at: i64,
    pub sync_status: String, // "pending", "synced"
}

// Global local database instance for the edge agent
pub static LOCAL_DB: Lazy<Arc<Mutex<Connection>>> = Lazy::new(|| {
    // In production, this would be in ~/.swarmclaw/data.db or similar
    let db_path = std::env::current_dir().unwrap().join("swarmclaw_local.db");
    let conn = Connection::open(&db_path).expect("Failed to open local SQLite DB");
    
    conn.execute(
        "CREATE TABLE IF NOT EXISTS outbox (
            id TEXT PRIMARY KEY,
            platform TEXT NOT NULL,
            channel_id TEXT NOT NULL,
            token TEXT NOT NULL,
            app_id TEXT,
            payload TEXT NOT NULL,
            ui_components TEXT,
            created_at INTEGER NOT NULL,
            sync_status TEXT NOT NULL
        )",
        [],
    ).expect("Failed to initialize outbox table");
    
    Arc::new(Mutex::new(conn))
});

pub fn enqueue_message(mut msg: OutboxMessage) -> Result<()> {
    // Redact sensitive info from the payload before enqueuing
    msg.payload = Redactor::redact(&msg.payload);

    let ui_comp_json = msg.ui_components.as_ref().map(|v| v.to_string());

    let guard = LOCAL_DB.lock().unwrap();
    guard.execute(
        "INSERT INTO outbox (id, platform, channel_id, token, app_id, payload, ui_components, created_at, sync_status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![msg.id, msg.platform, msg.channel_id, msg.token, msg.app_id, msg.payload, ui_comp_json, msg.created_at, msg.sync_status],
    ).context("Failed to insert into outbox")?;
    
    info!("Enqueued outbound message {} to {}", msg.id, msg.platform);
    Ok(())
}

pub fn get_pending_messages() -> Result<Vec<OutboxMessage>> {
    let guard = LOCAL_DB.lock().unwrap();
    let mut stmt = guard.prepare("SELECT id, platform, channel_id, token, app_id, payload, ui_components, created_at, sync_status FROM outbox WHERE sync_status = 'pending' ORDER BY created_at ASC LIMIT 50")?;
    
    let iter = stmt.query_map([], |row| {
        let ui_comp_str: Option<String> = row.get(6)?;
        let ui_components = ui_comp_str.and_then(|s| serde_json::from_str(&s).ok());

        Ok(OutboxMessage {
            id: row.get(0)?,
            platform: row.get(1)?,
            channel_id: row.get(2)?,
            token: row.get(3)?,
            app_id: row.get(4)?,
            payload: row.get(5)?,
            ui_components,
            created_at: row.get(7)?,
            sync_status: row.get(8)?,
        })
    })?;

    let mut messages = Vec::new();
    for m in iter {
        messages.push(m?);
    }
    Ok(messages)
}

pub fn mark_message_synced(id: &str) -> Result<()> {
    let guard = LOCAL_DB.lock().unwrap();
    guard.execute(
        "UPDATE outbox SET sync_status = 'synced' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// A background worker that pulls from the SQLite outbox and attempts to send.
/// It uses Tokio's threadpool to manage concurrent network requests securely with backoff.
pub async fn start_outbox_worker() {
    let client = reqwest::Client::new();
    info!("Starting Local SQLite Outbox Worker");
    
    loop {
        if let Ok(pending) = get_pending_messages() {
            for msg in pending {
                let client_clone = client.clone();
                let msg_id = msg.id.clone();
                
                // Spawn a new Tokio task on the threadpool for each outbound message
                tokio::spawn(async move {
                    if let Err(e) = send_with_retry(&client_clone, &msg).await {
                        error!("Failed to send message {} after retries: {}", msg.id, e);
                    } else {
                        // Mark as synced locally
                        if let Err(e) = mark_message_synced(&msg_id) {
                            error!("Failed to mark message {} as synced: {}", msg_id, e);
                        } else {
                            debug!("Successfully sent and marked message {} as synced", msg_id);
                            
                            // Here is where we fire and forget the Postgres sync to Mothership Backend
                            tokio::spawn(sync_to_postgres(msg));
                        }
                    }
                });
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

async fn send_with_retry(client: &reqwest::Client, msg: &OutboxMessage) -> Result<()> {
    let mut attempt = 0;
    let max_retries = 5;
    let base_delay = 500; // ms

    loop {
        attempt += 1;
        
        let req = match msg.platform.as_str() {
            "discord" => {
                let url = format!("https://discord.com/api/v10/webhooks/{}/{}/messages/@original", msg.app_id.as_deref().unwrap_or_default(), msg.token);
                
                let mut full_payload: serde_json::Value = serde_json::from_str(&msg.payload).unwrap_or(serde_json::json!({"content": msg.payload}));
                if let Some(components) = &msg.ui_components {
                    full_payload["components"] = components.clone();
                }

                client.patch(&url)
                    .header("Content-Type", "application/json")
                    .json(&full_payload)
            },
            "telegram" => {
                let url = format!("https://api.telegram.org/bot{}/sendMessage", msg.token);
                let mut full_payload = serde_json::json!({
                    "chat_id": msg.channel_id,
                    "text": msg.payload,
                });
                if let Some(reply_markup) = &msg.ui_components {
                    full_payload["reply_markup"] = reply_markup.clone();
                }

                client.post(&url)
                    .header("Content-Type", "application/json")
                    .json(&full_payload)
            },
            "internal" => return Ok(()), // Internal events don't go to external platforms
            _ => return Err(anyhow::anyhow!("Unknown platform: {}", msg.platform)),
        };

        match req.send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) if resp.status().is_client_error() && resp.status().as_u16() != 429 => {
                // 4xx errors (except Rate Limit) mean our payload is bad, don't retry.
                return Err(anyhow::anyhow!("Client error {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
            },
            Ok(resp) => {
                warn!("Server error {}. Retrying attempt {}/{}", resp.status(), attempt, max_retries);
            },
            Err(e) => {
                warn!("Network error: {}. Retrying attempt {}/{}", e, attempt, max_retries);
            }
        }

        if attempt >= max_retries {
            return Err(anyhow::anyhow!("Max retries exceeded"));
        }

        let delay = std::time::Duration::from_millis(base_delay * (2_u64.pow(attempt - 1)));
        tokio::time::sleep(delay).await;
    }
}

/// Sync a successfully sent message to the centralized Postgres database
async fn sync_to_postgres(msg: OutboxMessage) {
    let backend_url = std::env::var("MOTHERSHIP_URL").unwrap_or_else(|_| "https://api.mothershipdeploy.com".to_string());
    let token = std::env::var("MOTHERSHIP_API_KEY").unwrap_or_default();
    
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/sync/conversations", backend_url);
    
    let payload = serde_json::json!({
        "agent_id": std::env::var("AGENT_ID").unwrap_or_else(|_| "local-agent".to_string()),
        "platform": msg.platform,
        "channel_id": msg.channel_id,
        "payload": msg.payload,
        "timestamp": msg.created_at,
    });

    match client.post(&url)
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await 
    {
        Ok(resp) if resp.status().is_success() => info!("Synced conversation event to Postgres"),
        Ok(resp) => error!("Failed to sync to Postgres, status: {}", resp.status()),
        Err(e) => error!("Network error syncing to Postgres: {}", e),
    }
}
