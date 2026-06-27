use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug)]
pub enum FleetError {
    Provisioning(String),
    Scheduling(String),
    Configuration(String),
    UnknownStatus(String),
}

// Hand-written `Display`/`Error` impls (instead of `thiserror`) so the fleet
// module pulls in NO new dependencies — `thiserror` is not a direct dependency
// of this crate.
impl fmt::Display for FleetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FleetError::Provisioning(m) => {
                write!(f, "Failed to provision fleet resource: {m}")
            }
            FleetError::Scheduling(m) => write!(f, "Failed to schedule job: {m}"),
            FleetError::Configuration(m) => {
                write!(f, "Fleet provider configuration error: {m}")
            }
            FleetError::UnknownStatus(m) => {
                write!(f, "Agent status unknown or unreachable: {m}")
            }
        }
    }
}

impl std::error::Error for FleetError {}

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
///
/// # Distributed round-trip contract
///
/// A Fleet-backed sub-agent executor (see
/// [`crate::core::delegation::FleetExecutor`]) drives a delegated subtask
/// through three phases against this trait:
///
/// 1. **spawn** — [`spawn_agents`](FleetProvider::spawn_agents) dispatches a
///    [`FleetJobRequest`] (the subtask goal/context encoded into the command
///    and/or `env_vars`, `count = 1`).
/// 2. **poll** — [`get_job_status`](FleetProvider::get_job_status) is polled
///    until the job reaches a TERMINAL state. Terminal is matched
///    case-insensitively on `"completed"` / `"succeeded"` (success) and
///    `"failed"` (failure); anything else (`"running"`, `"pending"`, ...) is
///    non-terminal and the poller keeps waiting (bounded).
/// 3. **fetch result** — on success
///    [`get_job_result`](FleetProvider::get_job_result) retrieves the finished
///    agent's text summary.
///
/// The result-fetch step is OPTIONAL for an implementation: providers that
/// cannot yet return a finished agent's output (such as the reference
/// [`MothershipFleetProvider`]) inherit the default `Ok(None)`, and callers
/// must surface that as a clear "provider does not return results yet" error
/// rather than a silent empty success.
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

    /// Retrieve a finished job's result/summary, if the provider supports it.
    ///
    /// Default: `Ok(None)` (not yet supported) — e.g. the reference
    /// [`MothershipFleetProvider`], which would need additional Mothership
    /// Engine infra to ship a finished agent's output back to the caller. This
    /// is the future infra capability the [`crate::core::delegation::FleetExecutor`]
    /// seam is built to consume; until a provider overrides it, a real Fleet
    /// round-trip can spawn and observe completion but cannot return a summary.
    async fn get_job_result(&self, _job_id: &str) -> Result<Option<String>, FleetError> {
        Ok(None)
    }
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

    // NOTE: `get_job_result` is intentionally NOT overridden here — Mothership
    // inherits the trait default (`Ok(None)`). Returning a finished agent's
    // summary back to the caller is a FUTURE Mothership Engine infra capability
    // (the engine would need to capture each agent's terminal output and expose
    // a retrieval RPC). The `FleetExecutor` seam already consumes this method,
    // so wiring real result-return later requires no changes outside this impl.
}
