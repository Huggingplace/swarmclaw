# SwarmClaw Fleet Integration

SwarmClaw is designed to be a deeply **Open Source** agent orchestration framework. While it is built to integrate seamlessly with the **Mothership Platform**, it does not *require* Mothership to run. 

To honor this open-source philosophy, the capability to "spawn other agents" (Swarm Mode) is abstracted behind the `FleetProvider` trait.

## The `FleetProvider` Trait

If a SuperClaw (the orchestrator agent) decides it needs to spawn 5 minion agents to execute a map-reduce task, it does not hardcode an API call to Mothership. It calls the `FleetProvider`.

```rust
#[async_trait]
pub trait FleetProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn spawn_agents(&self, request: FleetJobRequest) -> Result<(), FleetError>;
    async fn terminate_job(&self, job_id: &str) -> Result<(), FleetError>;
    async fn get_job_status(&self, job_id: &str) -> Result<FleetJobStatus, FleetError>;
}
```

## Supported Fleet Backends

Because SwarmClaw relies on this trait, you can swap the backend by simply providing a different implementation at boot time.

### 1. `MothershipFleetProvider` (Reference Implementation)
This is the default implementation for users running within the Mothership ecosystem.
*   **How it works:** It makes a high-speed gRPC call to the `Mothership Engine`.
*   **Advantage:** Instantly provisions cloud VMs (Spot or On-Demand) across AWS/GCP and automatically configures WebRTC networking and Zero-Trust Secret tunnels.

### 2. `KubernetesFleetProvider` (Community/Enterprise)
For enterprises that already have a massive K8s cluster, they can implement a provider that talks directly to the Kubernetes API.
*   **How it works:** `spawn_agents` translates the `FleetJobRequest` into a Kubernetes `Job` or `Deployment` YAML and applies it via the K8s API.

### 3. `LocalDockerProvider` (Testing)
For developers testing Swarm logic on their laptop.
*   **How it works:** Uses the local Docker socket (`/var/run/docker.sock`) to spin up containers locally.

## Building Your Own Provider

If you want SwarmClaw to spawn agents on your custom infrastructure (like HashiCorp Nomad, or even a Raspberry Pi cluster), simply implement the trait:

```rust
struct MyPiClusterProvider;

#[async_trait]
impl FleetProvider for MyPiClusterProvider {
    fn name(&self) -> &str { "Raspberry Pi Swarm" }
    
    async fn spawn_agents(&self, req: FleetJobRequest) -> Result<(), FleetError> {
        // Make SSH call to your Pi cluster manager
        Ok(())
    }
    // ...
}
```

By keeping this interface clean, SwarmClaw remains an agnostic, powerful open-source agent framework that can run anywhere.