# SwarmClaw Secrets Architecture: Zero-Trust & Swarm Scale

To ensure `huggingplace-swarmclaw` matches and exceeds the security posture of projects like IronClaw, we are implementing a **Zero-Trust Host-Boundary Injection** model for secret management. 

Crucially, because SwarmClaw is designed to run natively on **Mothership Fleet** infrastructure, our secret architecture is built to support massive horizontal scaling (Swarm Mode) without bottlenecking a central database.

## 1. The Core Philosophy: Host-Boundary Injection

Like IronClaw, SwarmClaw agents and their WASM-based skills **never see raw secrets**. 

*   **The Problem:** Passing API keys into the LLM context or as environment variables allows a malicious prompt injection to trick the model into printing the keys in chat or sending them to a rogue server.
*   **The Solution:** Skills only receive placeholders (e.g., `{{secrets.GITHUB_TOKEN}}`). 
*   **The Execution:** When a WASM tool attempts an outbound HTTP request, the SwarmClaw Orchestrator intercepts the call at the host boundary, validates the destination URL against the tool's capability manifest, and swaps the placeholder with the actual secret decrypting it *just-in-time*.

## 2. The `SecretsStore` Trait

To support different deployment environments, secret management is abstracted behind an asynchronous Rust trait. This allows us to hot-swap the backend depending on whether the agent is running locally on a laptop, as a standalone cloud server, or as part of a 100-node Mothership Swarm.

```rust
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("Secret not found")]
    NotFound,
    #[error("Storage error: {0}")]
    Storage(String),
}

#[async_trait]
pub trait SecretsStore: Send + Sync {
    /// Retrieve a decrypted secret for injection at the host boundary.
    async fn get_secret(&self, key: &str, agent_id: &str) -> Result<String, SecretError>;
}
```

## 3. Supported Backends (The "Multiple Options")

SwarmClaw provides multiple implementations of `SecretsStore` to match IronClaw's flexibility while enabling Swarm scaling.

### Option A: `EnvSecretsStore` (Local Development)
For local testing (`mothership local run`), developers do not want to spin up a full Postgres database just to test a new script. This store reads directly from a `.env` file. 
*   *Note:* A strict environment check (`if env == "production"`) prevents this from being used in deployed clouds.

### Option B: `SeaOrmSecretsStore` (The Monolith / IronClaw Parity)
For users running a standalone, self-hosted SwarmClaw agent, this provides exact feature parity with IronClaw's `PostgresSecretsStore` and `LibSqlSecretsStore`.
*   Uses `seaorm` to connect to a central PostgreSQL or libSQL/Turso database.
*   Secrets are stored AES-256-GCM encrypted at rest.
*   Suitable for single-node or low-concurrency deployments.

### Option C: `MothershipFleetStore` (The Swarm Enabler)
**This is SwarmClaw's unique competitive advantage.**
If a user runs `mothership fleet run --image swarmclaw --count 1000`, 1000 agents querying a central Postgres database for secrets simultaneously would cause a massive bottleneck. 

Instead, SwarmClaw integrates natively with the Mothership infrastructure:
1.  **Pre-Flight Sync:** When the Mothership Engine provisions the Target VMs for the Swarm, it retrieves the required secrets from the central vault (ButtrBase/Postgres).
2.  **Secure Delivery:** The Engine pushes these secrets to the **Mothership Carrier** daemon running on each edge VM over a secure tunnel.
3.  **Local Fetch:** The `MothershipFleetStore` implementation in SwarmClaw does *not* talk to the public internet. Instead, it requests the secret from the local Carrier daemon via a Unix socket or a local memory-mapped file.

**Result:** 1,000 agents can boot instantly, securely access their credentials locally with zero network latency, and execute tasks without hammering the central database.

### Option D: `MinionEdgeStore` (The Ultimate Zero-Trust)
While `MothershipFleetStore` keeps secrets safe from prompt injection by injecting them at the host boundary, **it still requires the Mothership Cloud to possess the user's secrets**. 
For ultra-paranoid use cases (e.g., local development on a laptop connecting to a cloud AI), users can utilize the **Minion** WebRTC proxy.
*   The API keys (e.g., `GITHUB_TOKEN`) are stored strictly on the user's local machine (via OS Keychain or local file).
*   The cloud SwarmClaw agent sends an MCP tool call (`github_create_issue`) over WebRTC.
*   The local Minion intercepts the call, attaches the locally-stored API token to the HTTP request, executes it from the user's desktop IP address, and returns the result to the cloud agent. 
*   **Result:** The cloud AI agent *never* touches, stores, or transmits the user's personal credentials.

## 4. The Roadmap: Confidential Fleet (TEE)

To match the ultimate security vision of IronClaw, SwarmClaw is designed to run within **Trusted Execution Environments (TEEs)** orchestrated by the Mothership Engine.

* **Zero-Trust for Sensitive Swarms:** By launching the `MothershipFleetStore` on TEE-enabled hardware (Intel TDX, AMD SEV, AWS Nitro Enclaves), the secrets delivered from the Carrier to the SwarmClaw Orchestrator are protected by hardware-level memory encryption.
* **Security on Spot:** This enables a unique "Confidential Spot" strategy: users can run highly sensitive agents on the cheapest available spot hardware, knowing that not even the cloud provider or a hypervisor vulnerability can leak the agent's secrets or its reasoning state.
* **Orchestration Simplicity:** The TEE attestation and hardware-encrypted tunnel are managed by the Mothership Control Plane. The SwarmClaw agent simply uses the `MothershipFleetStore` as usual, but gains hardware-grade isolation for its credentials.
