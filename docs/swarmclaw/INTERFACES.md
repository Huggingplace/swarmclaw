# SwarmClaw Core Interfaces

SwarmClaw is built around a set of clean, abstract Rust traits. This modularity ensures that the framework can be adapted to any environment, keeping it fully open-source and vendor-agnostic.

## 1. The `FleetProvider` (Infrastructure)
SwarmClaw can spawn other agents to help it complete tasks (Swarm Mode). It does not care *where* those agents are spawned.

**Trait Location:** `swarmclaw/src/fleet/mod.rs`
```rust
#[async_trait]
pub trait FleetProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn spawn_agents(&self, request: FleetJobRequest) -> Result<(), FleetError>;
    async fn terminate_job(&self, job_id: &str) -> Result<(), FleetError>;
    async fn get_job_status(&self, job_id: &str) -> Result<FleetJobStatus, FleetError>;
}
```
*   **Implementations:** `MothershipFleetProvider`, `KubernetesFleetProvider` (Community), `LocalDockerProvider`.

## 2. The `SecretsStore` (Zero-Trust Security)
SwarmClaw plugins never see raw `.env` files. Secrets are injected at the host boundary just-in-time. 

**Trait Location:** `swarmclaw/src/secrets/mod.rs`
```rust
#[async_trait]
pub trait SecretsStore: Send + Sync {
    async fn get_secret(&self, key: &str, agent_id: &str) -> Result<String, SecretError>;
}
```
*   **Implementations:** 
    *   `MothershipFleetStore` (High-speed local Unix socket fetching for massive swarms).
    *   `SeaOrmSecretsStore` (IronClaw-parity database querying).
    *   `EnvSecretsStore` (Local dev).
    *   `TieredSecretsStore` (Composite fallback).

## 3. The `Skill` and `Tool` (Capabilities)
A `Skill` is a collection of `Tools`. SwarmClaw can load tools dynamically via WASM or MCP.

**Trait Location:** `swarmclaw/src/skills/mod.rs`
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    
    /// Executes the tool. All crashes/panics must be caught by the WorkerPool.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String>;
}
```
*   **Implementations:** 
    *   `WasmTool` (Executes untrusted community code via `wasmtime` and the Zero-Copy FlatBuffers ABI).
    *   `McpTool` (Proxies execution to external standard Model Context Protocol servers).

## 4. The `Agent` (The Loop)
The core `Agent` struct takes these traits as dependencies upon initialization.

```rust
pub struct Agent {
    fleet: Box<dyn FleetProvider>,
    secrets: Box<dyn SecretsStore>,
    skills: Vec<Box<dyn Skill>>,
    // ...
}
```

This strict adherence to dependency injection guarantees that SwarmClaw can evolve endlessly without being locked into a single cloud provider or execution environment.