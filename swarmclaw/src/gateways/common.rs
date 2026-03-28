use crate::core::Agent;
use crate::outbox::LOCAL_DB;
use anyhow::{Context, Result};
use reqwest::Client;
use rusqlite::params;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct StoredAttachment {
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub source_url: String,
}

pub fn mark_webhook_event_once(platform: &str, event_id: &str) -> Result<bool> {
    let guard = LOCAL_DB
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to lock local webhook database"))?;

    guard.execute(
        "CREATE TABLE IF NOT EXISTS inbound_webhook_events (
            platform TEXT NOT NULL,
            event_id TEXT NOT NULL,
            processed_at INTEGER NOT NULL,
            PRIMARY KEY (platform, event_id)
        )",
        [],
    )?;

    let inserted = guard.execute(
        "INSERT OR IGNORE INTO inbound_webhook_events (platform, event_id, processed_at)
         VALUES (?1, ?2, ?3)",
        params![platform, event_id, now_millis()],
    )?;

    Ok(inserted > 0)
}

pub async fn download_attachment_to_workspace(
    agent: &Agent,
    platform: &str,
    session_id: &str,
    suggested_name: &str,
    source_url: &str,
    headers: &[(String, String)],
) -> Result<Option<StoredAttachment>> {
    let Some(workspace_root) = agent.workspace_root() else {
        return Ok(None);
    };

    let relative_dir = PathBuf::from(".swarmclaw")
        .join("inbox")
        .join(sanitize_path_component(platform))
        .join(sanitize_path_component(session_id));
    let absolute_dir = workspace_root.join(&relative_dir);
    fs::create_dir_all(&absolute_dir).with_context(|| {
        format!(
            "failed to create attachment inbox directory at {}",
            absolute_dir.display()
        )
    })?;

    let file_name = unique_file_name(suggested_name);
    let absolute_path = absolute_dir.join(&file_name);
    let relative_path = normalize_relative_path(&relative_dir.join(&file_name));

    let client = Client::new();
    let mut request = client.get(source_url);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("failed to download attachment from {source_url}"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "attachment download failed with status {} for {}",
            response.status(),
            source_url
        );
    }

    let bytes = response.bytes().await?;
    fs::write(&absolute_path, &bytes).with_context(|| {
        format!(
            "failed to write attachment to workspace path {}",
            absolute_path.display()
        )
    })?;

    Ok(Some(StoredAttachment {
        relative_path,
        absolute_path,
        source_url: source_url.to_string(),
    }))
}

pub fn sanitize_path_component(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "item".to_string()
    } else {
        sanitized
    }
}

fn unique_file_name(suggested_name: &str) -> String {
    let path = Path::new(suggested_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_path_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "attachment".to_string());
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(sanitize_path_component)
        .filter(|value| !value.is_empty());

    let timestamp = now_millis();
    match extension {
        Some(extension) => format!("{stem}-{timestamp}.{extension}"),
        None => format!("{stem}-{timestamp}"),
    }
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
