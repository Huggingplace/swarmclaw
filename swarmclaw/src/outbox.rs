use crate::security::Redactor;
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxMessage {
    pub id: String,
    pub platform: String, // "discord", "telegram", "slack", "whatsapp", or "internal"
    pub channel_id: String,
    pub token: String,
    pub app_id: Option<String>,
    pub payload: String, // JSON payload representing the message to send
    pub ui_components: Option<serde_json::Value>, // Interactive elements (buttons, menus)
    pub created_at: i64,
    pub sync_status: String, // "pending", "in_flight", "synced", "failed"
    pub attempt_count: u32,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<i64>,
    pub next_attempt_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutboxMessageSummary {
    pub id: String,
    pub platform: String,
    pub channel_id: String,
    pub sync_status: String,
    pub created_at: i64,
    pub attempt_count: u32,
    pub last_attempt_at: Option<i64>,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub payload_preview: String,
}

// Global local database instance for the edge agent
pub static LOCAL_DB: Lazy<Arc<Mutex<Connection>>> = Lazy::new(|| {
    // In production, this would be in ~/.swarmclaw/data.db or similar
    let db_path = std::env::current_dir().unwrap().join("swarmclaw_local.db");
    let conn = Connection::open(&db_path).expect("Failed to open local SQLite DB");
    init_outbox_schema(&conn).expect("Failed to initialize outbox table");

    Arc::new(Mutex::new(conn))
});

#[derive(Debug)]
enum DeliveryError {
    Retryable(anyhow::Error),
    Permanent(anyhow::Error),
}

#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    max_attempts: u32,
    base_delay_ms: u64,
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(error) | Self::Permanent(error) => write!(f, "{error}"),
        }
    }
}

fn outbox_db() -> Result<MutexGuard<'static, Connection>> {
    LOCAL_DB
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to lock local outbox database"))
}

fn init_outbox_schema(conn: &Connection) -> Result<()> {
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
            sync_status TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            last_attempt_at INTEGER,
            next_attempt_at INTEGER
        )",
        [],
    )?;

    for statement in [
        "ALTER TABLE outbox ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE outbox ADD COLUMN last_error TEXT",
        "ALTER TABLE outbox ADD COLUMN last_attempt_at INTEGER",
        "ALTER TABLE outbox ADD COLUMN next_attempt_at INTEGER",
    ] {
        let _ = conn.execute(statement, []);
    }

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outbox_status_next_attempt
         ON outbox(sync_status, next_attempt_at, created_at)",
        [],
    )?;

    Ok(())
}

fn default_status(status: &str) -> &str {
    if status.trim().is_empty() {
        "pending"
    } else {
        status
    }
}

pub fn enqueue_message(mut msg: OutboxMessage) -> Result<()> {
    // Redact sensitive info from the payload before enqueuing
    msg.payload = Redactor::redact(&msg.payload);

    let ui_comp_json = msg.ui_components.as_ref().map(|v| v.to_string());
    let sync_status = default_status(&msg.sync_status).to_string();

    let guard = outbox_db()?;
    guard
        .execute(
            "INSERT INTO outbox (
            id, platform, channel_id, token, app_id, payload, ui_components, created_at,
            sync_status, attempt_count, last_error, last_attempt_at, next_attempt_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                msg.id,
                msg.platform,
                msg.channel_id,
                msg.token,
                msg.app_id,
                msg.payload,
                ui_comp_json,
                msg.created_at,
                sync_status,
                msg.attempt_count as i64,
                msg.last_error,
                msg.last_attempt_at,
                msg.next_attempt_at
            ],
        )
        .context("Failed to insert into outbox")?;

    info!("Enqueued outbound message {} to {}", msg.id, msg.platform);
    Ok(())
}

pub fn enqueue_gateway_text_message(
    platform: &str,
    channel_id: &str,
    token: &str,
    app_id: Option<String>,
    delivery_context: Option<serde_json::Value>,
    content: &str,
) -> Result<()> {
    let mut payload = match platform {
        "telegram" => serde_json::json!({
            "chat_id": channel_id,
            "text": content,
        }),
        "slack" => serde_json::json!({
            "channel": channel_id,
            "text": content,
        }),
        "whatsapp" => serde_json::json!({
            "To": channel_id,
            "Body": content,
        }),
        _ => serde_json::json!({
            "content": content,
        }),
    };

    if let Some(extra) = delivery_context {
        merge_json_object(&mut payload, extra);
    }

    enqueue_message(OutboxMessage {
        id: Uuid::new_v4().to_string(),
        platform: platform.to_string(),
        channel_id: channel_id.to_string(),
        token: token.to_string(),
        app_id,
        payload: payload.to_string(),
        ui_components: None,
        created_at: chrono::Utc::now().timestamp_millis(),
        sync_status: "pending".to_string(),
        attempt_count: 0,
        last_error: None,
        last_attempt_at: None,
        next_attempt_at: None,
    })
}

fn merge_json_object(target: &mut serde_json::Value, extra: serde_json::Value) {
    let Some(target_obj) = target.as_object_mut() else {
        return;
    };
    let Some(extra_obj) = extra.as_object() else {
        return;
    };

    for (key, value) in extra_obj {
        target_obj.insert(key.clone(), value.clone());
    }
}

fn recover_in_flight_messages() -> Result<usize> {
    let guard = outbox_db()?;
    let recovered = guard.execute(
        "UPDATE outbox
         SET sync_status = 'pending',
             next_attempt_at = COALESCE(next_attempt_at, ?1)
         WHERE sync_status = 'in_flight'",
        params![now_millis()],
    )?;
    Ok(recovered)
}

pub fn claim_pending_messages(limit: usize) -> Result<Vec<OutboxMessage>> {
    let mut guard = outbox_db()?;
    let tx = guard.transaction()?;
    let now = now_millis();
    let mut stmt = tx.prepare(
        "SELECT id, platform, channel_id, token, app_id, payload, ui_components, created_at,
                sync_status, attempt_count, last_error, last_attempt_at, next_attempt_at
         FROM outbox
         WHERE sync_status = 'pending'
           AND COALESCE(next_attempt_at, 0) <= ?1
         ORDER BY created_at ASC
         LIMIT ?2",
    )?;

    let iter = stmt.query_map(params![now, limit.max(1) as i64], |row| {
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
            attempt_count: row.get::<_, i64>(9)? as u32,
            last_error: row.get(10)?,
            last_attempt_at: row.get(11)?,
            next_attempt_at: row.get(12)?,
        })
    })?;

    let mut messages = Vec::new();
    for m in iter {
        messages.push(m?);
    }

    drop(stmt);

    for message in &messages {
        tx.execute(
            "UPDATE outbox
             SET sync_status = 'in_flight',
                 attempt_count = attempt_count + 1,
                 last_attempt_at = ?2,
                 last_error = NULL
             WHERE id = ?1
               AND sync_status = 'pending'",
            params![message.id, now],
        )?;
    }

    tx.commit()?;

    for message in &mut messages {
        message.sync_status = "in_flight".to_string();
        message.attempt_count += 1;
        message.last_attempt_at = Some(now);
    }

    Ok(messages)
}

fn mark_message_status(
    id: &str,
    status: &str,
    last_error: Option<&str>,
    next_attempt_at: Option<i64>,
) -> Result<()> {
    let guard = outbox_db()?;
    guard.execute(
        "UPDATE outbox
         SET sync_status = ?2,
             last_error = ?3,
             next_attempt_at = ?4
         WHERE id = ?1",
        params![id, status, last_error, next_attempt_at],
    )?;
    Ok(())
}

pub fn schedule_message_retry(id: &str, error: &str, next_attempt_at: i64) -> Result<()> {
    mark_message_status(id, "pending", Some(error), Some(next_attempt_at))
}

pub fn mark_message_synced(id: &str) -> Result<()> {
    mark_message_status(id, "synced", None, None)
}

pub fn mark_message_failed(id: &str, error: &str) -> Result<()> {
    mark_message_status(id, "failed", Some(error), None)
}

pub fn list_outbox_messages(
    status: Option<&str>,
    limit: usize,
) -> Result<Vec<OutboxMessageSummary>> {
    let guard = outbox_db()?;
    let normalized_status = status
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    let sql = if normalized_status.is_some() {
        "SELECT id, platform, channel_id, sync_status, created_at, attempt_count, last_attempt_at,
                next_attempt_at, last_error, payload
         FROM outbox
         WHERE sync_status = ?1
         ORDER BY created_at DESC
         LIMIT ?2"
    } else {
        "SELECT id, platform, channel_id, sync_status, created_at, attempt_count, last_attempt_at,
                next_attempt_at, last_error, payload
         FROM outbox
         ORDER BY created_at DESC
         LIMIT ?1"
    };

    let mut stmt = guard.prepare(sql)?;
    let mut entries = Vec::new();
    if let Some(status) = normalized_status.as_deref() {
        let rows = stmt.query_map(params![status, limit.max(1) as i64], |row| {
            summary_from_row(row)
        })?;
        for row in rows {
            entries.push(row?);
        }
    } else {
        let rows = stmt.query_map(params![limit.max(1) as i64], |row| summary_from_row(row))?;
        for row in rows {
            entries.push(row?);
        }
    }

    Ok(entries)
}

#[cfg(test)]
static TEST_DB_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[cfg(test)]
pub fn test_db_lock() -> MutexGuard<'static, ()> {
    TEST_DB_LOCK
        .lock()
        .expect("failed to lock outbox test mutex")
}

#[cfg(test)]
pub fn reset_local_db_for_tests() -> Result<()> {
    let guard = outbox_db()?;
    let _ = guard.execute("DELETE FROM outbox", []);
    let _ = guard.execute("DELETE FROM inbound_webhook_events", []);
    Ok(())
}

/// A background worker that pulls from the SQLite outbox and attempts to send.
/// It uses Tokio's threadpool to manage concurrent network requests securely with backoff.
pub async fn start_outbox_worker() {
    let client = reqwest::Client::new();
    info!("Starting Local SQLite Outbox Worker");

    match recover_in_flight_messages() {
        Ok(recovered) if recovered > 0 => {
            warn!(
                "Recovered {} in-flight outbox messages after restart",
                recovered
            );
        }
        Ok(_) => {}
        Err(error) => error!("Failed to recover in-flight outbox messages: {}", error),
    }

    loop {
        match claim_pending_messages(50) {
            Ok(pending) => {
                for msg in pending {
                    let client_clone = client.clone();
                    let msg_id = msg.id.clone();

                    // Spawn a new Tokio task on the threadpool for each outbound message
                    tokio::spawn(async move {
                        let policy = retry_policy_for(&msg.platform);
                        match send_message_once(&client_clone, &msg).await {
                            Ok(()) => {
                                if let Err(error) = mark_message_synced(&msg_id) {
                                    error!(
                                        "Failed to mark message {} as synced: {}",
                                        msg_id, error
                                    );
                                } else {
                                    debug!(
                                        "Successfully sent and marked message {} as synced",
                                        msg_id
                                    );
                                    tokio::spawn(sync_to_postgres(msg));
                                }
                            }
                            Err(DeliveryError::Retryable(error)) => {
                                let error_text = error.to_string();
                                if msg.attempt_count >= policy.max_attempts {
                                    error!(
                                        "Dead-lettering message {} after {} attempts: {}",
                                        msg_id, msg.attempt_count, error_text
                                    );
                                    if let Err(mark_error) =
                                        mark_message_failed(&msg_id, &error_text)
                                    {
                                        error!(
                                            "Failed to mark message {} as failed: {}",
                                            msg_id, mark_error
                                        );
                                    }
                                    return;
                                }

                                let next_attempt_at = now_millis()
                                    + retry_delay_millis(policy, msg.attempt_count) as i64;
                                warn!(
                                    "Retryable delivery failure for {} on attempt {}. Next retry at {}: {}",
                                    msg_id, msg.attempt_count, next_attempt_at, error_text
                                );
                                if let Err(mark_error) =
                                    schedule_message_retry(&msg_id, &error_text, next_attempt_at)
                                {
                                    error!(
                                        "Failed to requeue message {} after delivery error: {}",
                                        msg_id, mark_error
                                    );
                                }
                            }
                            Err(DeliveryError::Permanent(error)) => {
                                error!("Permanent delivery failure for {}: {}", msg_id, error);
                                if let Err(mark_error) =
                                    mark_message_failed(&msg_id, &error.to_string())
                                {
                                    error!(
                                        "Failed to mark message {} as failed: {}",
                                        msg_id, mark_error
                                    );
                                }
                            }
                        }
                    });
                }
            }
            Err(error) => {
                error!("Failed to claim pending outbox messages: {}", error);
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

async fn send_message_once(
    client: &reqwest::Client,
    msg: &OutboxMessage,
) -> std::result::Result<(), DeliveryError> {
    if msg.platform == "internal" {
        return Ok(());
    }

    let req = build_delivery_request(client, msg)?;

    match req.send().await {
        Ok(resp) if msg.platform == "slack" && resp.status().is_success() => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if parsed.get("ok").and_then(|value| value.as_bool()) == Some(true) {
                Ok(())
            } else {
                Err(DeliveryError::Permanent(anyhow::anyhow!(
                    "Slack API error {}: {}",
                    status,
                    parsed
                        .get("error")
                        .and_then(|value| value.as_str())
                        .unwrap_or(body.as_str())
                )))
            }
        }
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) if resp.status().is_client_error() && resp.status().as_u16() != 429 => {
            Err(DeliveryError::Permanent(anyhow::anyhow!(
                "Client error {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )))
        }
        Ok(resp) => Err(DeliveryError::Retryable(anyhow::anyhow!(
            "Server error {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ))),
        Err(error) => Err(DeliveryError::Retryable(anyhow::anyhow!(
            "Network error: {}",
            error
        ))),
    }
}

fn build_delivery_request(
    client: &reqwest::Client,
    msg: &OutboxMessage,
) -> std::result::Result<reqwest::RequestBuilder, DeliveryError> {
    match msg.platform.as_str() {
        "discord" => {
            let url = format!(
                "https://discord.com/api/v10/webhooks/{}/{}/messages/@original",
                msg.app_id.as_deref().unwrap_or_default(),
                msg.token
            );

            let mut full_payload: serde_json::Value = serde_json::from_str(&msg.payload)
                .unwrap_or(serde_json::json!({"content": msg.payload}));
            if let Some(components) = &msg.ui_components {
                full_payload["components"] = components.clone();
            }

            Ok(client
                .patch(&url)
                .header("Content-Type", "application/json")
                .json(&full_payload))
        }
        "telegram" => {
            let url = format!("https://api.telegram.org/bot{}/sendMessage", msg.token);
            let mut full_payload: serde_json::Value =
                serde_json::from_str(&msg.payload).unwrap_or(serde_json::json!({
                    "chat_id": msg.channel_id,
                    "text": msg.payload,
                }));
            if full_payload.get("chat_id").is_none() {
                full_payload["chat_id"] = serde_json::json!(msg.channel_id);
            }
            if full_payload.get("text").is_none() {
                full_payload["text"] = serde_json::json!(msg.payload);
            }
            if let Some(reply_markup) = &msg.ui_components {
                full_payload["reply_markup"] = reply_markup.clone();
            }

            Ok(client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&full_payload))
        }
        "slack" => {
            let url = "https://slack.com/api/chat.postMessage";
            let mut full_payload: serde_json::Value =
                serde_json::from_str(&msg.payload).unwrap_or(serde_json::json!({
                    "channel": msg.channel_id,
                    "text": msg.payload,
                }));
            if full_payload.get("channel").is_none() {
                full_payload["channel"] = serde_json::json!(msg.channel_id);
            }
            if full_payload.get("text").is_none() {
                full_payload["text"] = serde_json::json!(msg.payload);
            }
            if let Some(blocks) = &msg.ui_components {
                full_payload["blocks"] = blocks.clone();
            }

            Ok(client
                .post(url)
                .bearer_auth(&msg.token)
                .header("Content-Type", "application/json")
                .json(&full_payload))
        }
        "whatsapp" => {
            let account_sid = msg.app_id.as_deref().ok_or_else(|| {
                DeliveryError::Permanent(anyhow::anyhow!(
                    "WhatsApp delivery requires Twilio account SID in app_id"
                ))
            })?;
            let mut full_payload: serde_json::Value =
                serde_json::from_str(&msg.payload).unwrap_or(serde_json::json!({
                    "To": msg.channel_id,
                    "Body": msg.payload,
                }));
            if full_payload.get("To").is_none() {
                full_payload["To"] = serde_json::json!(msg.channel_id);
            }
            if full_payload.get("Body").is_none() {
                full_payload["Body"] = serde_json::json!(msg.payload);
            }
            if full_payload.get("From").is_none() {
                return Err(DeliveryError::Permanent(anyhow::anyhow!(
                    "WhatsApp delivery requires a From sender in the payload"
                )));
            }

            let form = json_to_form_fields(&full_payload).map_err(DeliveryError::Permanent)?;
            Ok(client
                .post(format!(
                    "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
                    account_sid
                ))
                .basic_auth(account_sid, Some(&msg.token))
                .form(&form))
        }
        _ => Err(DeliveryError::Permanent(anyhow::anyhow!(
            "Unknown platform: {}",
            msg.platform
        ))),
    }
}

fn json_to_form_fields(payload: &serde_json::Value) -> Result<Vec<(String, String)>> {
    let object = payload
        .as_object()
        .context("expected JSON object payload for form delivery")?;

    let mut fields = Vec::new();
    for (key, value) in object {
        let field_value = match value {
            serde_json::Value::Null => continue,
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            _ => value.to_string(),
        };
        fields.push((key.clone(), field_value));
    }
    Ok(fields)
}

fn retry_policy_for(platform: &str) -> RetryPolicy {
    match platform {
        "whatsapp" => RetryPolicy {
            max_attempts: 6,
            base_delay_ms: 2_000,
        },
        "slack" => RetryPolicy {
            max_attempts: 6,
            base_delay_ms: 1_500,
        },
        "telegram" => RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1_000,
        },
        "discord" => RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 750,
        },
        _ => RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 500,
        },
    }
}

fn retry_delay_millis(policy: RetryPolicy, attempt_count: u32) -> u64 {
    let exponent = attempt_count.saturating_sub(1).min(6);
    policy.base_delay_ms * 2_u64.pow(exponent)
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> Result<OutboxMessageSummary, rusqlite::Error> {
    let payload: String = row.get(9)?;
    Ok(OutboxMessageSummary {
        id: row.get(0)?,
        platform: row.get(1)?,
        channel_id: row.get(2)?,
        sync_status: row.get(3)?,
        created_at: row.get(4)?,
        attempt_count: row.get::<_, i64>(5)? as u32,
        last_attempt_at: row.get(6)?,
        next_attempt_at: row.get(7)?,
        last_error: row.get(8)?,
        payload_preview: preview_excerpt(&payload, 140),
    })
}

fn preview_excerpt(content: &str, max_chars: usize) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = collapsed.chars().take(max_chars).collect::<String>();
    if collapsed.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn normalize_clawnet_api_base(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/v1")
    }
}

fn derive_clawnet_api_base_from_ws(ws_url: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(ws_url).ok()?;
    match url.scheme() {
        "ws" => {
            let _ = url.set_scheme("http");
        }
        "wss" => {
            let _ = url.set_scheme("https");
        }
        "http" | "https" => {}
        _ => return None,
    }

    let segments = url.path_segments()?.collect::<Vec<_>>();
    let path = if segments.ends_with(&["ws", "stream"]) && segments.len() >= 2 {
        format!("/{}", segments[..segments.len() - 2].join("/"))
    } else {
        "/api/v1".to_string()
    };

    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);

    Some(url.to_string().trim_end_matches('/').to_string())
}

fn resolve_clawnet_api_base() -> String {
    for var_name in ["CLAWNET_API_URL", "CLAWNET_BACKEND_URL"] {
        if let Ok(value) = std::env::var(var_name) {
            if !value.trim().is_empty() {
                return normalize_clawnet_api_base(&value);
            }
        }
    }

    for var_name in ["WEBRTC_SIGNALING_URL", "CLAWNET_WS_URL"] {
        if let Ok(value) = std::env::var(var_name) {
            if let Some(derived) = derive_clawnet_api_base_from_ws(&value) {
                return derived;
            }
        }
    }

    "http://localhost:8002/api/v1".to_string()
}

/// Sync a successfully sent message to the centralized ClawNet backend
async fn sync_to_postgres(msg: OutboxMessage) {
    let api_base = resolve_clawnet_api_base();
    let token = std::env::var("CLAWNET_API_KEY")
        .or_else(|_| std::env::var("MOTHERSHIP_API_KEY"))
        .unwrap_or_default();

    let client = reqwest::Client::new();
    let url = format!("{}/sync/conversations", api_base);

    let payload = serde_json::json!({
        "agent_id": std::env::var("AGENT_ID").unwrap_or_else(|_| "local-agent".to_string()),
        "platform": msg.platform,
        "channel_id": msg.channel_id,
        "payload": msg.payload,
        "timestamp": msg.created_at,
        "ui_components": msg.ui_components,
    });

    let mut request = client.post(&url).json(&payload);
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() => info!("Synced conversation event to Postgres"),
        Ok(resp) => error!("Failed to sync to Postgres, status: {}", resp.status()),
        Err(e) => error!("Network error syncing to Postgres: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_explicit_api_base() {
        assert_eq!(
            normalize_clawnet_api_base("http://localhost:8002"),
            "http://localhost:8002/api/v1"
        );
        assert_eq!(
            normalize_clawnet_api_base("http://localhost:8002/api/v1/"),
            "http://localhost:8002/api/v1"
        );
    }

    #[test]
    fn derives_api_base_from_signaling_url() {
        let derived =
            derive_clawnet_api_base_from_ws("ws://localhost:8002/api/v1/ws/stream?thread_id=abc");

        assert_eq!(derived.as_deref(), Some("http://localhost:8002/api/v1"));
    }

    #[test]
    fn retry_delay_scales_by_attempt() {
        let policy = retry_policy_for("slack");
        assert_eq!(retry_delay_millis(policy, 1), 1_500);
        assert_eq!(retry_delay_millis(policy, 2), 3_000);
        assert_eq!(retry_delay_millis(policy, 3), 6_000);
    }

    #[test]
    fn enqueues_whatsapp_payload_with_sender_context() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        enqueue_gateway_text_message(
            "whatsapp",
            "whatsapp:+15550002222",
            "auth-token",
            Some("AC123".to_string()),
            Some(json!({ "From": "whatsapp:+15550001111" })),
            "hello world",
        )?;

        let guard = outbox_db()?;
        let payload: String = guard.query_row(
            "SELECT payload FROM outbox ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        let parsed: serde_json::Value = serde_json::from_str(&payload)?;
        assert_eq!(parsed["To"], "whatsapp:+15550002222");
        assert_eq!(parsed["From"], "whatsapp:+15550001111");
        assert_eq!(parsed["Body"], "hello world");
        Ok(())
    }
}
