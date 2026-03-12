use async_trait::async_trait;
use thiserror::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Error)]
pub enum FleetError {
    #[error("Failed to provision fleet resource: {0}")]
    Provisioning(String),
    #[error("Failed to schedule job: {0}")]
    Scheduling(String),
    #[error("Fleet provider configuration error: {0}")]
    Configuration(String),
    #[error("Agent status unknown or unreachable: {0}")]
    UnknownStatus(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetJobRequest {
    pub job_id: String,
    pub image: String,
    pub command: String,
    pub env_vars: HashMap<String, String>,
    pub min_vcpu: f32,
    pub min_memory_gb: f32,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetJobStatus {
    pub job_id: String,
    pub status: String, // e.g., "running", "pending", "failed"
    pub active_nodes: u32,
}

/// The core trait for Swarm orchestration.
/// By implementing this trait, SwarmClaw can be deployed on ANY infrastructure,
/// not just Mothership. It honors the Open Source nature of the project.
#[async_trait]
pub trait FleetProvider: Send + Sync {
    /// Name of the provider (e.g., "Mothership", "Kubernetes", "Nomad", "LocalDocker")
    fn name(&self) -> &str;

    /// Spin up a new agent or group of agents
    async fn spawn_agents(&self, request: FleetJobRequest) -> Result<(), FleetError>;

    /// Terminate an existing job
    async fn terminate_job(&self, job_id: &str) -> Result<(), FleetError>;

    /// Check the status of a deployed Swarm job
    async fn get_job_status(&self, job_id: &str) -> Result<FleetJobStatus, FleetError>;
}

/// Reference Implementation: Mothership
pub struct MothershipFleetProvider {
    engine_url: String,
}

impl MothershipFleetProvider {
    pub fn new(engine_url: String) -> Self {
        Self { engine_url }
    }
}

#[async_trait]
impl FleetProvider for MothershipFleetProvider {
    fn name(&self) -> &str {
        "Mothership Engine"
    }

    async fn spawn_agents(&self, request: FleetJobRequest) -> Result<(), FleetError> {
        tracing::info!("(Mothership) Dispatching FleetJob {} for image {}", request.job_id, request.image);
        // In reality, this makes a gRPC call to Mothership Engine
        Ok(())
    }

    async fn terminate_job(&self, job_id: &str) -> Result<(), FleetError> {
        tracing::info!("(Mothership) Terminating FleetJob {}", job_id);
        Ok(())
    }

    async fn get_job_status(&self, job_id: &str) -> Result<FleetJobStatus, FleetError> {
        Ok(FleetJobStatus {
            job_id: job_id.to_string(),
            status: "running".to_string(),
            active_nodes: 1,
        })
    }
}
