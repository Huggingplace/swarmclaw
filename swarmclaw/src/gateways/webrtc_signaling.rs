use crate::core::state::Role;
use crate::core::{agent::ChannelInfo, Agent};
use crate::gateways::ChatGateway;
use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, error, info, info_span, warn, Instrument};
use uuid::Uuid;
use webrtc::{
    api::APIBuilder, data_channel::data_channel_message::DataChannelMessage,
    data_channel::RTCDataChannel, ice_transport::ice_candidate::RTCIceCandidateInit,
    ice_transport::ice_server::RTCIceServer, peer_connection::configuration::RTCConfiguration,
    peer_connection::sdp::session_description::RTCSessionDescription,
    peer_connection::RTCPeerConnection,
};

pub struct WebRTCSignalingGateway {
    ws_url: String,
    agent_id: String,
    agent_template: Arc<Agent>,
}

impl WebRTCSignalingGateway {
    pub fn new(ws_url: String, agent_id: String, agent_template: Arc<Agent>) -> Self {
        Self {
            ws_url,
            agent_id,
            agent_template,
        }
    }
}

#[async_trait]
impl ChatGateway for WebRTCSignalingGateway {
    async fn start(&self) -> Result<()> {
        let url = if self.ws_url.contains('?') {
            format!("{}&token=agent_{}", self.ws_url, self.agent_id)
        } else {
            format!("{}?token=agent_{}", self.ws_url, self.agent_id)
        };
        info!("Connecting to WebRTC WebSocket (Signaling) at {}", url);

        let (ws_stream, _) = connect_async(&url).await?;
        info!("Successfully connected to WebRTC Signaling Server");

        let (mut write, mut read) = ws_stream.split();
        let (tx_ws, mut rx_ws) = mpsc::unbounded_channel::<Message>();

        // Spawn a task to handle outbound WebSocket writes
        tokio::spawn(async move {
            while let Some(msg) = rx_ws.recv().await {
                if let Err(e) = write.send(msg).await {
                    error!("WebSocket send error: {}", e);
                    break;
                }
            }
        });

        // Initialize WebRTC API
        let api = APIBuilder::new().build();

        // Prepare WebRTC Configuration (Using public Google STUN for NAT traversal)
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };

        // We will store the PeerConnection here once a client offers to connect
        let pc_mutex: Arc<TokioMutex<Option<Arc<RTCPeerConnection>>>> =
            Arc::new(TokioMutex::new(None));
        let agent_id_clone = self.agent_id.clone();
        let agent_template = self.agent_template.clone();

        // Announce presence
        let connect_msg = json!({
            "type": "status",
            "sender_id": self.agent_id,
            "status": "online"
        });
        tx_ws.send(Message::Text(connect_msg.to_string().into()))?;

        // Start listening to Signaling WebSocket
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let parsed: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let sender_id = parsed
                        .get("sender_id")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if sender_id == self.agent_id {
                        continue; // Ignore our own broadcasted signals
                    }

                    let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    match msg_type {
                        "webrtc_offer" => {
                            info!("Received WebRTC Offer from {}", sender_id);
                            if let Some(sdp_str) = parsed.get("sdp").and_then(|s| s.as_str()) {
                                let peer_connection =
                                    api.new_peer_connection(config.clone()).await?;
                                let pc_clone = Arc::new(peer_connection);

                                *pc_mutex.lock().await = Some(pc_clone.clone());

                                // Setup ICE Candidate handler
                                let tx_ws_ice = tx_ws.clone();
                                let aid_ice = agent_id_clone.clone();
                                pc_clone.on_ice_candidate(Box::new(
                                    move |c: Option<
                                        webrtc::ice_transport::ice_candidate::RTCIceCandidate,
                                    >| {
                                        let tx = tx_ws_ice.clone();
                                        let aid = aid_ice.clone();
                                        Box::pin(async move {
                                            if let Some(c) = c {
                                                if let Ok(json) = c.to_json() {
                                                    let msg = json!({
                                                        "type": "webrtc_candidate",
                                                        "sender_id": aid,
                                                        "candidate": json
                                                    });
                                                    let _ = tx.send(Message::Text(
                                                        msg.to_string().into(),
                                                    ));
                                                }
                                            }
                                        })
                                    },
                                ));

                                // Setup Data Channel handler
                                let agent_dc = agent_template.clone();
                                let remote_sender_id = sender_id.to_string();
                                let tx_ws_data_channel = tx_ws.clone();
                                let outbound_agent_id = agent_id_clone.clone();
                                pc_clone.on_data_channel(Box::new(
                                    move |d: Arc<RTCDataChannel>| {
                                        info!("New DataChannel {} {}", d.label(), d.id());

                                        let outbound_channel = d.clone();
                                        let response_channel = outbound_channel.clone();
                                        let agent_template = agent_dc.clone();
                                        let remote_sender_id = remote_sender_id.clone();
                                        let tx_ws_channel = tx_ws_data_channel.clone();
                                        let outbound_agent_id = outbound_agent_id.clone();
                                        Box::pin(async move {
                                            outbound_channel.on_message(Box::new(
                                                move |msg: DataChannelMessage| {
                                                    let msg_str =
                                                        String::from_utf8(msg.data.to_vec())
                                                            .unwrap_or_default();
                                                    info!(
                                                        "Message from DataChannel: '{}'",
                                                        msg_str
                                                    );

                                                    let outbound_channel = response_channel.clone();
                                                    let agent_template = agent_template.clone();
                                                    let remote_sender_id = remote_sender_id.clone();
                                                    let tx_ws_channel = tx_ws_channel.clone();
                                                    let outbound_agent_id =
                                                        outbound_agent_id.clone();

                                                    Box::pin(async move {
                                                        if msg_str.trim().is_empty() {
                                                            return;
                                                        }

                                                        let request_span = info_span!(
                                                            "gateway_ingress",
                                                            request_id = %format!("webrtc-{}", Uuid::new_v4()),
                                                            platform = "webrtc",
                                                            transport = "data_channel",
                                                            sender_id = %remote_sender_id,
                                                            payload_bytes = msg_str.len()
                                                        );
                                                        {
                                                            let _guard = request_span.enter();
                                                            info!("Accepted WebRTC data channel message");
                                                        }

                                                        let parsed_message =
                                                            serde_json::from_str::<Value>(&msg_str)
                                                                .ok();
                                                        let thread_id = parsed_message
                                                            .as_ref()
                                                            .and_then(|value| {
                                                                value.get("thread_id")
                                                            })
                                                            .and_then(Value::as_str)
                                                            .map(ToOwned::to_owned);
                                                        let reply_to_message_id = parsed_message
                                                            .as_ref()
                                                            .and_then(|value| {
                                                                value.get("message_id")
                                                            })
                                                            .and_then(Value::as_str)
                                                            .map(ToOwned::to_owned);
                                                        let content = parsed_message
                                                            .as_ref()
                                                            .and_then(|value| {
                                                                value.get("content")
                                                            })
                                                            .and_then(Value::as_str)
                                                            .map(str::to_owned)
                                                            .unwrap_or_else(|| msg_str.clone());
                                                        let session_scope = thread_id
                                                            .clone()
                                                            .unwrap_or_else(|| {
                                                                remote_sender_id.clone()
                                                            });
                                                        if let (Some(thread_id), Some(message_id)) =
                                                            (
                                                                thread_id.clone(),
                                                                reply_to_message_id.clone(),
                                                            )
                                                        {
                                                            let delivery_event = json!({
                                                                "type": "agent_message_delivered",
                                                                "sender_id": outbound_agent_id.clone(),
                                                                "thread_id": thread_id,
                                                                "message_id": message_id,
                                                            });
                                                            let _ = tx_ws_channel.send(
                                                                Message::Text(
                                                                    delivery_event
                                                                        .to_string()
                                                                        .into(),
                                                                ),
                                                            );
                                                        }
                                                        let response = match run_webrtc_turn(
                                                            agent_template,
                                                            &session_scope,
                                                            &remote_sender_id,
                                                            &content,
                                                        )
                                                        .instrument(request_span)
                                                        .await
                                                        {
                                                            Ok(response) => response,
                                                            Err(error) => {
                                                                format!(
                                                                    "SwarmClaw error: {}",
                                                                    error
                                                                )
                                                            }
                                                        };

                                                        let response_message_id =
                                                            Uuid::new_v4().to_string();
                                                        if let Some(thread_id) = thread_id.clone() {
                                                            let response_content = response.clone();
                                                            let outbound_agent_id_for_ws =
                                                                outbound_agent_id.clone();
                                                            let outbound_agent_id_for_dc =
                                                                outbound_agent_id.clone();
                                                            let response_event = json!({
                                                                "type": "agent_message_reply",
                                                                "sender_id": outbound_agent_id_for_ws,
                                                                "thread_id": thread_id,
                                                                "message_id": response_message_id.clone(),
                                                                "in_reply_to_message_id": reply_to_message_id,
                                                                "content": response_content.clone(),
                                                            });
                                                            let _ = tx_ws_channel.send(
                                                                Message::Text(
                                                                    response_event
                                                                        .to_string()
                                                                        .into(),
                                                                ),
                                                            );
                                                            if let Err(error) = outbound_channel
                                                                .send_text(
                                                                    json!({
                                                                        "type": "agent_message_reply",
                                                                        "id": response_message_id,
                                                                        "thread_id": thread_id,
                                                                        "reply_to_message_id": reply_to_message_id,
                                                                        "sender_id": outbound_agent_id_for_dc,
                                                                        "content": response_content,
                                                                    })
                                                                    .to_string(),
                                                                )
                                                                .await
                                                            {
                                                                error!(
                                                                    "Failed to send DataChannel response: {}",
                                                                    error
                                                                );
                                                            }
                                                            return;
                                                        }

                                                        if let Err(error) = outbound_channel
                                                            .send_text(response)
                                                            .await
                                                        {
                                                            error!(
                                                        "Failed to send DataChannel response: {}",
                                                        error
                                                    );
                                                        }
                                                    })
                                                },
                                            ));
                                        })
                                    },
                                ));

                                // Set Remote Description
                                let sdp =
                                    RTCSessionDescription::offer(sdp_str.to_string()).unwrap();
                                pc_clone.set_remote_description(sdp).await?;

                                // Create Answer
                                let answer = pc_clone.create_answer(None).await?;
                                pc_clone.set_local_description(answer.clone()).await?;

                                // Send Answer back via Signaling
                                let answer_msg = json!({
                                    "type": "webrtc_answer",
                                    "sender_id": agent_id_clone,
                                    "sdp": answer.sdp
                                });
                                tx_ws.send(Message::Text(answer_msg.to_string().into()))?;
                            }
                        }
                        "webrtc_candidate" => {
                            if let Some(candidate_val) = parsed.get("candidate") {
                                if let Ok(candidate_init) =
                                    serde_json::from_value::<RTCIceCandidateInit>(
                                        candidate_val.clone(),
                                    )
                                {
                                    if let Some(pc) = pc_mutex.lock().await.as_ref() {
                                        debug!("Adding ICE Candidate from {}", sender_id);
                                        if let Err(e) = pc.add_ice_candidate(candidate_init).await {
                                            warn!("Failed to add ICE candidate: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        "direct_message_delivery" => {
                            if let Some(content) = parsed.get("content").and_then(|c| c.as_str()) {
                                info!("Received direct ClawNet delivery on WebRTC: {}", content);

                                let content_owned = content.to_string();
                                let sender_id_owned = sender_id.to_string();
                                let thread_id = parsed
                                    .get("thread_id")
                                    .and_then(|value| value.as_str())
                                    .map(ToOwned::to_owned);
                                let reply_to_message_id = parsed
                                    .get("message_id")
                                    .and_then(|value| value.as_str())
                                    .map(ToOwned::to_owned);
                                let agent_template = agent_template.clone();
                                let tx_ws_clone = tx_ws.clone();
                                let aid = self.agent_id.clone();
                                let request_span = info_span!(
                                    "gateway_ingress",
                                    request_id = %format!("webrtc-{}", Uuid::new_v4()),
                                    platform = "webrtc",
                                    transport = "direct_delivery",
                                    sender_id = %sender_id_owned,
                                    payload_bytes = content_owned.len()
                                );

                                tokio::spawn(
                                    async move {
                                        if content_owned.trim().is_empty() {
                                            return;
                                        }

                                        let session_scope = thread_id
                                            .clone()
                                            .unwrap_or_else(|| sender_id_owned.clone());
                                        let response_text = match run_webrtc_turn(
                                            agent_template,
                                            &session_scope,
                                            &sender_id_owned,
                                            &content_owned,
                                        )
                                        .await
                                        {
                                            Ok(response) => response,
                                            Err(error) => format!("SwarmClaw error: {}", error),
                                        };

                                        let response = json!({
                                            "type": "agent_message_reply",
                                            "sender_id": aid,
                                            "thread_id": thread_id,
                                            "in_reply_to_message_id": reply_to_message_id,
                                            "content": response_text
                                        });
                                        let _ = tx_ws_clone
                                            .send(Message::Text(response.to_string().into()));
                                    }
                                    .instrument(request_span),
                                );
                            }
                        }
                        "message" => {
                            if let Some(content) = parsed.get("content").and_then(|c| c.as_str()) {
                                info!("Received legacy WS message on WebRTC: {}", content);

                                let content_owned = content.to_string();
                                let sender_id_owned = sender_id.to_string();
                                let thread_id = parsed
                                    .get("thread_id")
                                    .and_then(|value| value.as_str())
                                    .map(ToOwned::to_owned);
                                let agent_template = agent_template.clone();
                                let tx_ws_clone = tx_ws.clone();
                                let aid = self.agent_id.clone();
                                let request_span = info_span!(
                                    "gateway_ingress",
                                    request_id = %format!("webrtc-{}", Uuid::new_v4()),
                                    platform = "webrtc",
                                    transport = "websocket",
                                    sender_id = %sender_id_owned,
                                    payload_bytes = content_owned.len()
                                );

                                {
                                    let _guard = request_span.enter();
                                    info!("Accepted WebRTC websocket message");
                                }

                                tokio::spawn(
                                    async move {
                                        if content_owned.trim().is_empty() {
                                            return;
                                        }

                                        let session_scope = thread_id
                                            .clone()
                                            .unwrap_or_else(|| sender_id_owned.clone());
                                        let response_text = match run_webrtc_turn(
                                            agent_template,
                                            &session_scope,
                                            &sender_id_owned,
                                            &content_owned,
                                        )
                                        .await
                                        {
                                            Ok(response) => response,
                                            Err(error) => format!("SwarmClaw error: {}", error),
                                        };

                                        let response = json!({
                                            "type": "message",
                                            "sender_id": aid,
                                            "content": response_text
                                        });
                                        let _ = tx_ws_clone
                                            .send(Message::Text(response.to_string().into()));
                                    }
                                    .instrument(request_span),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) => {
                    warn!("WebRTC Signaling WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("Error receiving from WebRTC Signaling WebSocket: {}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn send(&self, _target_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }
}

async fn run_webrtc_turn(
    agent_template: Arc<Agent>,
    session_scope: &str,
    sender_id: &str,
    input: &str,
) -> Result<String> {
    let session_id = format!("webrtc-{}", normalize_webrtc_sender_id(session_scope));
    let mut agent = agent_template.spawn_session(session_id);
    let history_len = agent.state.history.len();

    agent
        .handle_gateway_turn(
            input,
            ChannelInfo::new("internal", sender_id.to_string(), String::new(), None),
        )
        .await?;

    Ok(
        latest_assistant_reply_since(&agent, history_len).unwrap_or_else(|| {
            "SwarmClaw completed the request, but the model returned no text.".to_string()
        }),
    )
}

fn latest_assistant_reply_since(agent: &Agent, start_len: usize) -> Option<String> {
    if start_len >= agent.state.history.len() {
        return None;
    }

    agent.state.history[start_len..]
        .iter()
        .rev()
        .find_map(|message| {
            if message.role == Role::Assistant && !message.content.trim().is_empty() {
                Some(message.content.clone())
            } else {
                None
            }
        })
}

fn normalize_webrtc_sender_id(sender_id: &str) -> String {
    let sanitized = sender_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "anonymous".to_string()
    } else {
        sanitized
    }
}
