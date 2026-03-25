use crate::gateways::ChatGateway;
use crate::core::Agent;
use async_trait::async_trait;
use anyhow::{Result, Context};
use tracing::{info, error, warn, debug};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use webrtc::{
    api::APIBuilder,
    data_channel::data_channel_message::DataChannelMessage,
    data_channel::RTCDataChannel,
    ice_transport::ice_candidate::RTCIceCandidateInit,
    ice_transport::ice_server::RTCIceServer,
    peer_connection::configuration::RTCConfiguration,
    peer_connection::peer_connection_state::RTCPeerConnectionState,
    peer_connection::sdp::session_description::RTCSessionDescription,
    peer_connection::RTCPeerConnection,
};

pub struct WebRTCSignalingGateway {
    ws_url: String,
    agent_id: String,
    agent: Arc<TokioMutex<Agent>>,
}

impl WebRTCSignalingGateway {
    pub fn new(ws_url: String, agent_id: String, agent: Arc<TokioMutex<Agent>>) -> Self {
        Self { ws_url, agent_id, agent }
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
        let pc_mutex: Arc<TokioMutex<Option<Arc<RTCPeerConnection>>>> = Arc::new(TokioMutex::new(None));
        let agent_id_clone = self.agent_id.clone();
        let agent_ref = self.agent.clone();
        
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

                    let sender_id = parsed.get("sender_id").and_then(|s| s.as_str()).unwrap_or("");
                    if sender_id == self.agent_id {
                        continue; // Ignore our own broadcasted signals
                    }

                    let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    match msg_type {
                        "webrtc_offer" => {
                            info!("Received WebRTC Offer from {}", sender_id);
                            if let Some(sdp_str) = parsed.get("sdp").and_then(|s| s.as_str()) {
                                let peer_connection = api.new_peer_connection(config.clone()).await?;
                                let pc_clone = Arc::new(peer_connection);
                                
                                *pc_mutex.lock().await = Some(pc_clone.clone());

                                // Setup ICE Candidate handler
                                let tx_ws_ice = tx_ws.clone();
                                let aid_ice = agent_id_clone.clone();
                                pc_clone.on_ice_candidate(Box::new(move |c: Option<webrtc::ice_transport::ice_candidate::RTCIceCandidate>| {
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
                                                let _ = tx.send(Message::Text(msg.to_string().into()));
                                            }
                                        }
                                    })
                                }));

                                // Setup Data Channel handler
                                let aid_dc = agent_id_clone.clone();
                                let agent_dc = agent_ref.clone();
                                pc_clone.on_data_channel(Box::new(move |d: Arc<RTCDataChannel>| {
                                    info!("New DataChannel {} {}", d.label(), d.id());
                                    
                                    let d2 = d.clone();
                                    let aid = aid_dc.clone();
                                    let agent_inner = agent_dc.clone();
                                    Box::pin(async move {
                                        d2.on_message(Box::new(move |msg: DataChannelMessage| {
                                            let msg_str = String::from_utf8(msg.data.to_vec()).unwrap_or_default();
                                            info!("Message from DataChannel: '{}'", msg_str);
                                            
                                            let d3 = d.clone();
                                            let agent_inner_inner = agent_inner.clone();
                                            let aid_inner = aid.clone();
                                            
                                            Box::pin(async move {
                                                let mut agent_lock = agent_inner_inner.lock().await;
                                                
                                                // Add user message to history
                                                use crate::core::state::{Message as AgentMessage, Role};
                                                agent_lock.state.history.push(AgentMessage {
                                                    role: Role::User,
                                                    content: msg_str.clone(),
                                                    timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                                    tool_calls: None,
                                                    tool_call_id: None,
                                                });

                                                // Use a custom stream handler that pipes chunks to DataChannel
                                                let d4 = d3.clone();
                                                let mut full_response = String::new();
                                                
                                                match agent_lock.llm.stream(&agent_lock.state.history, &Default::default(), &[]).await {
                                                    Ok(mut stream) => {
                                                        while let Some(chunk) = stream.next().await {
                                                            match chunk {
                                                                Ok(crate::llm::ChatChunk::Content(delta)) => {
                                                                    full_response.push_str(&delta);
                                                                    let _ = d4.send_text(delta).await;
                                                                }
                                                                Ok(crate::llm::ChatChunk::Done) => break,
                                                                Err(e) => {
                                                                    let err_msg = format!("⚠️ Engine Error: {}", e);
                                                                    let _ = d4.send_text(err_msg).await;
                                                                    break;
                                                                }
                                                                _ => {}
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        let err_msg = format!("⚠️ Engine Error: {}", e);
                                                        let _ = d4.send_text(err_msg).await;
                                                    }
                                                }
                                                
                                                // Record assistant message
                                                agent_lock.state.history.push(AgentMessage {
                                                    role: Role::Assistant,
                                                    content: full_response,
                                                    timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                                    tool_calls: None,
                                                    tool_call_id: None,
                                                });
                                            })
                                        }));
                                    })
                                }));

                                // Set Remote Description
                                let sdp = RTCSessionDescription::offer(sdp_str.to_string()).unwrap();
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
                        },
                        "webrtc_candidate" => {
                            if let Some(candidate_val) = parsed.get("candidate") {
                                if let Ok(candidate_init) = serde_json::from_value::<RTCIceCandidateInit>(candidate_val.clone()) {
                                    if let Some(pc) = pc_mutex.lock().await.as_ref() {
                                        debug!("Adding ICE Candidate from {}", sender_id);
                                        if let Err(e) = pc.add_ice_candidate(candidate_init).await {
                                            warn!("Failed to add ICE candidate: {}", e);
                                        }
                                    }
                                }
                            }
                        },
                        "message" => {
                            if let Some(content) = parsed.get("content").and_then(|c| c.as_str()) {
                                info!("Received legacy WS message on WebRTC: {}", content);
                                
                                let content_owned = content.to_string();
                                let agent_inner = agent_ref.clone();
                                let tx_ws_clone = tx_ws.clone();
                                let aid = self.agent_id.clone();
                                
                                tokio::spawn(async move {
                                    let mut agent_lock = agent_inner.lock().await;
                                    
                                    use crate::core::state::{Message as AgentMessage, Role};
                                    agent_lock.state.history.push(AgentMessage {
                                        role: Role::User,
                                        content: content_owned,
                                        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                        tool_calls: None,
                                        tool_call_id: None,
                                    });

                                    let mut full_response = String::new();
                                    
                                    match agent_lock.llm.stream(&agent_lock.state.history, &Default::default(), &[]).await {
                                        Ok(mut stream) => {
                                            while let Some(chunk) = stream.next().await {
                                                match chunk {
                                                    Ok(crate::llm::ChatChunk::Content(delta)) => {
                                                        full_response.push_str(&delta);
                                                    }
                                                    Ok(crate::llm::ChatChunk::Done) => break,
                                                    Err(e) => {
                                                        let err_msg = format!("⚠️ Engine Error: {}", e);
                                                        let response = json!({
                                                            "type": "message",
                                                            "sender_id": aid,
                                                            "content": err_msg
                                                        });
                                                        let _ = tx_ws_clone.send(Message::Text(response.to_string().into()));
                                                        break;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let err_msg = format!("⚠️ Engine Error: {}", e);
                                            let response = json!({
                                                "type": "message",
                                                "sender_id": aid,
                                                "content": err_msg
                                            });
                                            let _ = tx_ws_clone.send(Message::Text(response.to_string().into()));
                                        }
                                    }

                                    if !full_response.is_empty() {
                                        let response = json!({
                                            "type": "message",
                                            "sender_id": aid,
                                            "content": full_response.clone()
                                        });
                                        let _ = tx_ws_clone.send(Message::Text(response.to_string().into()));
                                        
                                        agent_lock.state.history.push(AgentMessage {
                                            role: Role::Assistant,
                                            content: full_response,
                                            timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                            tool_calls: None,
                                            tool_call_id: None,
                                        });
                                    }
                                });
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
