use crate::core::{agent::ChannelInfo, Agent};
use crate::gateways::common::{download_attachment_to_workspace, mark_webhook_event_once};
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
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, info_span, Instrument};

const PING: u8 = 1;
const APPLICATION_COMMAND: u8 = 2;

#[derive(Deserialize, Debug)]
pub struct Interaction {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub token: Option<String>,
    pub application_id: Option<String>,
    pub channel_id: Option<String>,
    pub data: Option<InteractionData>,
}

#[derive(Deserialize, Debug)]
pub struct InteractionData {
    pub name: Option<String>,
    pub options: Option<Vec<InteractionOption>>,
    pub resolved: Option<ResolvedData>,
}

#[derive(Deserialize, Debug)]
pub struct InteractionOption {
    pub name: String,
    pub value: Option<Value>,
    pub options: Option<Vec<InteractionOption>>,
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
    agent_template: Arc<Agent>,
}

pub struct DiscordWebhookGateway {
    port: u16,
    agent_template: Arc<Agent>,
}

impl DiscordWebhookGateway {
    pub fn new(agent_template: Arc<Agent>) -> Result<Self> {
        let port = std::env::var("DISCORD_WEBHOOK_PORT")
            .unwrap_or_else(|_| "8081".to_string())
            .parse()?;
        Ok(Self {
            port,
            agent_template,
        })
    }

    pub fn router(public_key: VerifyingKey, agent_template: Arc<Agent>) -> Router {
        let state = Arc::new(DiscordState {
            public_key,
            agent_template,
        });

        Router::new()
            .route("/discord/interactions", post(handle_interaction))
            .with_state(state)
    }

    async fn run_server(&self) -> Result<()> {
        let pk_hex = std::env::var("DISCORD_PUBLIC_KEY").context("DISCORD_PUBLIC_KEY not set")?;
        let pk_bytes = hex::decode(pk_hex).context("Invalid hex in DISCORD_PUBLIC_KEY")?;
        let public_key =
            VerifyingKey::try_from(pk_bytes.as_slice()).context("Invalid ed25519 public key")?;

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Starting Discord webhook server on {}", addr);

        let listener = TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            Self::router(public_key, self.agent_template.clone()),
        )
        .await?;

        Ok(())
    }
}

#[async_trait]
impl ChatGateway for DiscordWebhookGateway {
    async fn start(&self) -> Result<()> {
        self.run_server().await
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}

async fn handle_interaction(
    State(state): State<Arc<DiscordState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    let signature_hex = headers
        .get("x-signature-ed25519")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let timestamp = headers
        .get("x-signature-timestamp")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let sig_bytes = hex::decode(signature_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let mut msg = timestamp.as_bytes().to_vec();
    msg.extend_from_slice(&body);

    if state.public_key.verify(&msg, &signature).is_err() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let interaction: Interaction =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    if interaction.kind == PING {
        return Ok(Json(InteractionResponse { kind: 1 }));
    }

    if interaction.kind == APPLICATION_COMMAND {
        let channel_id = interaction.channel_id.clone().unwrap_or_default();
        let request_span = info_span!(
            "gateway_ingress",
            request_id = %format!("discord-{}", interaction.id),
            platform = "discord",
            interaction_id = %interaction.id,
            channel_id = %channel_id
        );
        let is_new_event = mark_webhook_event_once("discord", &interaction.id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if !is_new_event {
            let _guard = request_span.enter();
            info!("Ignoring duplicate Discord interaction");
            return Ok(Json(InteractionResponse { kind: 5 }));
        }

        {
            let _guard = request_span.enter();
            info!("Accepted Discord interaction");
        }

        let response = InteractionResponse { kind: 5 };
        let token = interaction.token.clone().unwrap_or_default();
        let app_id = interaction.application_id.clone().unwrap_or_default();
        let interaction_id = interaction.id.clone();
        let interaction_data = interaction.data;
        let agent_template = state.agent_template.clone();

        tokio::spawn(
            async move {
                process_discord_message(
                    agent_template,
                    app_id,
                    token,
                    channel_id,
                    interaction_id,
                    interaction_data,
                )
                .await;
            }
            .instrument(request_span),
        );

        return Ok(Json(response));
    }

    Err(StatusCode::BAD_REQUEST)
}

async fn process_discord_message(
    agent_template: Arc<Agent>,
    app_id: String,
    token: String,
    channel_id: String,
    interaction_id: String,
    interaction_data: Option<InteractionData>,
) {
    let session_id = format!("discord-{}", channel_id);
    let mut agent = agent_template.spawn_session(session_id);
    let prompt =
        match build_discord_prompt(&agent, &interaction_id, interaction_data.as_ref()).await {
            Ok(prompt) => prompt,
            Err(error) => {
                error!(
                    channel_id = %channel_id,
                    "Failed to prepare Discord interaction context: {}",
                    error
                );
                format!(
                    "Discord interaction {} was received, but attachment preparation failed: {}",
                    interaction_id, error
                )
            }
        };

    info!(
        channel_id = %channel_id,
        "Processing Discord interaction through shared agent loop"
    );

    if let Err(error) = agent
        .handle_gateway_turn(
            &prompt,
            ChannelInfo::new(
                "discord",
                channel_id.clone(),
                token.clone(),
                Some(app_id.clone()),
            ),
        )
        .await
    {
        error!(
            channel_id = %channel_id,
            "Discord gateway turn failed: {}",
            error
        );
        let _ = enqueue_gateway_text_message(
            "discord",
            &channel_id,
            &token,
            Some(app_id),
            None,
            &format!("SwarmClaw error: {error}"),
        );
    }
}

async fn build_discord_prompt(
    agent: &Agent,
    interaction_id: &str,
    data: Option<&InteractionData>,
) -> Result<String> {
    let Some(data) = data else {
        return Ok("A Discord interaction was received without any prompt body.".to_string());
    };

    let mut sections = Vec::new();

    if let Some(name) = data.name.as_deref() {
        sections.push(format!("Discord slash command: /{name}"));
    }

    if let Some(options) = data.options.as_ref() {
        let mut rendered_options = Vec::new();
        flatten_interaction_options(options, "", &mut rendered_options);
        if !rendered_options.is_empty() {
            sections.push(format!("Arguments:\n{}", rendered_options.join("\n")));
        }
    }

    if let Some(attachments) = data
        .resolved
        .as_ref()
        .and_then(|resolved| resolved.attachments.as_ref())
    {
        let mut files = Vec::new();
        for attachment in attachments.values() {
            let stored = download_attachment_to_workspace(
                agent,
                "discord",
                interaction_id,
                &attachment.filename,
                &attachment.url,
                &[],
            )
            .await?;

            if let Some(stored) = stored {
                files.push(format!(
                    "- {} [{}] saved to {} (source: {})",
                    attachment.filename, attachment.id, stored.relative_path, stored.source_url
                ));
            } else {
                files.push(format!(
                    "- {} [{}] {}",
                    attachment.filename, attachment.id, attachment.url
                ));
            }
        }
        if !files.is_empty() {
            sections.push(format!("Attachments:\n{}", files.join("\n")));
        }
    }

    if sections.is_empty() {
        Ok("A Discord interaction was received without any prompt body.".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

fn flatten_interaction_options(
    options: &[InteractionOption],
    prefix: &str,
    rendered: &mut Vec<String>,
) {
    for option in options {
        let key = if prefix.is_empty() {
            option.name.clone()
        } else {
            format!("{prefix}.{}", option.name)
        };

        if let Some(value) = option.value.as_ref() {
            rendered.push(format!("- {}: {}", key, display_value(value)));
        }

        if let Some(children) = option.options.as_ref() {
            flatten_interaction_options(children, &key, rendered);
        }
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateways::test_support::{test_agent_template, wait_for_outbox_message};
    use crate::outbox::{reset_local_db_for_tests, test_db_lock};
    use anyhow::Result;
    use axum::body::Body;
    use axum::http::Request;
    use ed25519_dalek::{Signer, SigningKey};
    use tower::ServiceExt;

    #[tokio::test]
    async fn accepts_signed_interaction_and_enqueues_reply() -> Result<()> {
        let _lock = test_db_lock();
        reset_local_db_for_tests()?;

        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let body = serde_json::json!({
            "id": "discord-int-1",
            "type": 2,
            "token": "discord-token",
            "application_id": "app-123",
            "channel_id": "channel-123",
            "data": {
                "name": "chat",
                "options": [
                    { "name": "prompt", "value": "hello from discord" }
                ]
            }
        })
        .to_string();
        let timestamp = "1712345678";
        let signature = signing_key.sign(format!("{}{}", timestamp, body).as_bytes());

        let response =
            DiscordWebhookGateway::router(signing_key.verifying_key(), test_agent_template())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/discord/interactions")
                        .header("content-type", "application/json")
                        .header("x-signature-ed25519", hex::encode(signature.to_bytes()))
                        .header("x-signature-timestamp", timestamp)
                        .body(Body::from(body))?,
                )
                .await?;

        assert_eq!(response.status(), StatusCode::OK);

        let message = wait_for_outbox_message("discord", "channel-123").await?;
        assert!(message.payload_preview.contains("gateway ok"));
        Ok(())
    }
}
