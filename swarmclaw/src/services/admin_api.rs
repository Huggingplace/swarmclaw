use crate::core::session_store;
use crate::outbox;
use crate::services::control_plane_store::ControlPlaneStore;
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Clone)]
struct AdminState {
    workspace_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct SessionsResponse {
    store_path: String,
    sessions: Vec<SessionDto>,
}

#[derive(Debug, Serialize)]
struct SessionDto {
    session_id: String,
    created_at: i64,
    updated_at: i64,
    message_count: usize,
    last_role: Option<String>,
    last_timestamp: Option<u64>,
    last_content_preview: Option<String>,
}

#[derive(Debug, Serialize)]
struct HistoryResponse {
    store_path: String,
    session_id: String,
    messages: Vec<HistoryMessageDto>,
}

#[derive(Debug, Serialize)]
struct HistoryMessageDto {
    message_index: usize,
    role: String,
    content: String,
    timestamp: u64,
    tool_call_id: Option<String>,
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct OutboxResponse {
    messages: Vec<outbox::OutboxMessageSummary>,
}

#[derive(Debug, Serialize)]
struct ControlPlaneResponse {
    channels: Vec<crate::services::control_plane_store::ChannelRegistration>,
}

pub struct AdminApiServer {
    port: u16,
    workspace_path: PathBuf,
}

impl AdminApiServer {
    pub fn new(workspace_path: PathBuf) -> Result<Self> {
        let port = std::env::var("SWARMCLAW_ADMIN_PORT")
            .unwrap_or_else(|_| "8787".to_string())
            .parse()?;
        Ok(Self {
            port,
            workspace_path,
        })
    }

    pub fn router(workspace_path: PathBuf) -> Router {
        let state = Arc::new(AdminState { workspace_path });
        Router::new()
            .route("/admin/health", get(health))
            .route("/admin/sessions", get(list_sessions))
            .route("/admin/sessions/{session_id}/history", get(session_history))
            .route("/admin/outbox", get(list_outbox))
            .route(
                "/admin/control-plane/channels",
                get(list_control_plane_channels),
            )
            .with_state(state)
    }

    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        info!("Starting SwarmClaw admin API on {}", addr);
        let listener = TcpListener::bind(addr).await?;
        axum::serve(listener, Self::router(self.workspace_path.clone())).await?;
        Ok(())
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn list_sessions(
    State(state): State<Arc<AdminState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionsResponse>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(25).max(1);
    let store_path = session_store::migrate_legacy_sessions_in_workspace(&state.workspace_path)
        .map_err(internal_error)?;
    let sessions = session_store::list_sessions(&store_path, limit).map_err(internal_error)?;

    Ok(Json(SessionsResponse {
        store_path: store_path.display().to_string(),
        sessions: sessions
            .into_iter()
            .map(|session| SessionDto {
                session_id: session.session_id,
                created_at: session.created_at,
                updated_at: session.updated_at,
                message_count: session.message_count,
                last_role: session.last_role.map(role_to_string),
                last_timestamp: session.last_timestamp,
                last_content_preview: session.last_content_preview,
            })
            .collect(),
    }))
}

async fn session_history(
    State(state): State<Arc<AdminState>>,
    Path(session_id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<HistoryResponse>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50).max(1);
    let store_path = session_store::migrate_legacy_sessions_in_workspace(&state.workspace_path)
        .map_err(internal_error)?;
    let Some(history) = session_store::load_recent_history(&store_path, &session_id, limit)
        .map_err(internal_error)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown session '{}'", session_id),
        ));
    };

    Ok(Json(HistoryResponse {
        store_path: store_path.display().to_string(),
        session_id,
        messages: history
            .into_iter()
            .map(|entry| HistoryMessageDto {
                message_index: entry.message_index,
                role: role_to_string(entry.message.role),
                content: entry.message.content,
                timestamp: entry.message.timestamp,
                tool_call_id: entry.message.tool_call_id,
                tool_calls: entry.message.tool_calls,
            })
            .collect(),
    }))
}

async fn list_outbox(
    Query(query): Query<ListQuery>,
) -> Result<Json<OutboxResponse>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50).max(1);
    let messages =
        outbox::list_outbox_messages(query.status.as_deref(), limit).map_err(internal_error)?;
    Ok(Json(OutboxResponse { messages }))
}

async fn list_control_plane_channels(
    State(state): State<Arc<AdminState>>,
) -> Result<Json<ControlPlaneResponse>, (StatusCode, String)> {
    let store = ControlPlaneStore::open(&state.workspace_path.join(".swarmclaw"))
        .await
        .map_err(internal_error)?;
    let channels = store
        .list_channel_registrations()
        .await
        .map_err(internal_error)?;
    Ok(Json(ControlPlaneResponse { channels }))
}

fn role_to_string(role: crate::core::state::Role) -> String {
    match role {
        crate::core::state::Role::System => "system".to_string(),
        crate::core::state::Role::User => "user".to_string(),
        crate::core::state::Role::Assistant => "assistant".to_string(),
        crate::core::state::Role::Tool => "tool".to_string(),
    }
}

fn internal_error(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::{Message, Role, State};
    use crate::outbox::{enqueue_gateway_text_message, reset_local_db_for_tests, test_db_lock};
    use anyhow::Result;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn serves_session_and_outbox_snapshots() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        let workspace =
            std::env::temp_dir().join(format!("swarmclaw-admin-api-{}", Uuid::new_v4()));
        let sessions_dir = workspace.join(".swarmclaw").join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let legacy_state_path = sessions_dir.join("default.json");
        std::fs::write(
            &legacy_state_path,
            serde_json::to_vec(&State {
                history: vec![Message {
                    role: Role::User,
                    content: "hello admin".to_string(),
                    timestamp: 1,
                    tool_calls: None,
                    tool_call_id: None,
                }],
            })?,
        )?;

        enqueue_gateway_text_message("slack", "C999", "token", None, None, "pending hello")?;

        let router = AdminApiServer::router(workspace.clone());

        let sessions_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/sessions?limit=10")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(sessions_response.status(), StatusCode::OK);
        let sessions_body = to_bytes(sessions_response.into_body(), usize::MAX).await?;
        let sessions_json: serde_json::Value = serde_json::from_slice(&sessions_body)?;
        assert_eq!(sessions_json["sessions"][0]["session_id"], "default");

        let outbox_response = router
            .oneshot(
                Request::builder()
                    .uri("/admin/outbox?status=pending&limit=10")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(outbox_response.status(), StatusCode::OK);
        let outbox_body = to_bytes(outbox_response.into_body(), usize::MAX).await?;
        let outbox_json: serde_json::Value = serde_json::from_slice(&outbox_body)?;
        assert_eq!(outbox_json["messages"][0]["platform"], "slack");

        std::fs::remove_dir_all(workspace)?;
        Ok(())
    }
}
