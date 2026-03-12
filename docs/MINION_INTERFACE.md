# The SwarmClaw-Minion WebRTC Protocol (RMP)

To allow community developers to build custom Minions (e.g., a Minion written in Python for data science, or a Minion written in Swift for macOS native automation), we must clearly define the communication protocol between the cloud-hosted `SwarmClaw` agent and the desktop-hosted `Minion`.

Because the Minion sits behind consumer NATs and firewalls, the connection is established via WebRTC Data Channels.

## Connection Lifecycle & Authentication
Because Minions run on untrusted edge devices (a user's laptop) and agents run in the cloud, a strict, user-mediated authentication flow is required. We use a **URL-based invite system** to broker the WebRTC signaling.

### 1. The ClawNet Invite
* When a user decides an AI agent needs local access, they click "Connect Minion" in the **ClawNet** chat interface.
* ClawNet generates a unique, single-use, time-bound Connection URL:
  `mothership://minion/connect?session_id=abc-123&token=xyz-789&signaling_url=wss://engine.mothership.com`
* The user copies this URL to their clipboard.

### 2. Minion Authentication
* The user starts the Minion binary on their desktop and pastes the URL:
  `minion connect "mothership://minion/connect?session_id=abc-123&token=xyz-789&signaling_url=wss://engine.mothership.com"`
* The Minion connects to the provided `signaling_url` (Mothership Engine) via WebSocket and authenticates using the `token`.

### 3. WebRTC Signaling (SDP Exchange)
Once the Minion is authenticated to the session, Mothership Engine acts as the STUN/Signaling broker:
1. **SwarmClaw Requests Access:** SwarmClaw generates a WebRTC Offer and sends it to Mothership.
2. **Mothership Brokers:** Mothership forwards the Offer to the authenticated Minion.
3. **Minion Accepts:** Minion generates an Answer and sends it back to SwarmClaw via Mothership.
4. **P2P Established:** A direct, encrypted WebRTC Data Channel is opened between the cloud VM and the local desktop. The signaling WebSocket can now be dropped.

## The Data Channel Protocol (JSON-RPC 2.0)
All communication over the established WebRTC Data Channel must follow the JSON-RPC 2.0 specification. 

### 1. The Execution Request (SwarmClaw -> Minion)
When SwarmClaw wants to run a command on the desktop, it sends a request:

```json
{
  "jsonrpc": "2.0",
  "method": "execute_command",
  "params": {
    "command": "gh",
    "args": ["issue", "list", "--repo", "huggingplace/swarmclaw"],
    "cwd": "/Users/dev/workspace",
    "timeout_ms": 30000
  },
  "id": "req-12345"
}
```

### 2. The Policy Evaluation (Minion Internal)
The Minion parses the request and evaluates it against its local security rules. This evaluation happens in two stages:
* **Stage 1: Deterministic Check:** The Minion compares the command against the explicit `minion.yaml` policy file (e.g., "Allow `gh issue` but block `gh auth`").
* **Stage 2: Subjective LLM Check (Optional):** For complex or highly sensitive tools, the Minion can run a small, fast local LLM (like Llama 3 8B) to evaluate the *intent* of the command. For example, if the tool is `delete_files`, the local LLM checks if the requested path is safe to delete before allowing execution.

If the evaluation passes, the Minion spawns the process locally.

### 3. The Stream Updates (Minion -> SwarmClaw)
Because commands can take time (e.g., compiling code) and produce large amounts of text, the Minion must stream `stdout` and `stderr` back to SwarmClaw in real-time. It does this via JSON-RPC Notifications (messages without an `id`):

```json
{
  "jsonrpc": "2.0",
  "method": "stream_chunk",
  "params": {
    "request_id": "req-12345",
    "stream": "stdout",
    "data": "Fetching issues...
"
  }
}
```

### 4. The Execution Result (Minion -> SwarmClaw)
Once the process exits, the Minion sends the final JSON-RPC Response to close the request loop:

```json
{
  "jsonrpc": "2.0",
  "id": "req-12345",
  "result": {
    "exit_code": 0,
    "status": "success",
    "message": "Command completed."
  }
}
```

### 5. Policy Rejection & Rationale Response
If SwarmClaw attempts to run a command that is blocked (either deterministically or subjectively), the Minion immediately returns a JSON-RPC error.
Crucially, the Minion must provide a **rationale** in the error message so the cloud SwarmClaw agent understands *why* it was rejected and can adjust its strategy:

```json
{
  "jsonrpc": "2.0",
  "id": "req-12345",
  "error": {
    "code": 403,
    "message": "Policy Violation: 'gh auth' is explicitly disallowed by local minion.yaml rules. Rationale: You are not permitted to manage user credentials. Please proceed using the currently active session."
  }
}
```

## Data Channel Encoding & Compression

To ensure the WebRTC Data Channel remains highly performant—even when a Minion is streaming thousands of lines of build logs or returning large binary artifacts—the protocol mandates strict serialization and compression standards.

### 1. Binary Serialization (FlatBuffers / MessagePack)
While the protocol examples above use JSON for human readability, the actual wire format over the WebRTC Data Channel should default to a packed binary format.
* **Primary Format:** `MessagePack` (for dynamic, schema-less RPC calls).
* **High-Throughput Format:** `FlatBuffers` (for structured, high-volume streaming, such as continuous terminal stdout or UI frame buffers).

### 2. Stream Compression (Zstd)
WebRTC Data Channels have a maximum message size limit (often 16KB to 64KB depending on the browser/client implementation). To prevent fragmentation and maximize throughput, all payloads exceeding 1KB must be compressed.
* **The Standard:** `Zstandard (zstd)`.
* **Why Zstd?** `zstd` offers a vastly superior compression-ratio-to-speed trade-off compared to gzip or Brotli. It allows the Minion to compress massive terminal outputs almost instantly without spiking the user's desktop CPU, ensuring the cloud agent receives the data in milliseconds.
* **Hardware Acceleration:** Certain modern CPUs (like Apple Silicon and newer Intel/AMD server chips) have dedicated instructions or highly optimized vector math (SIMD/AVX-512) that accelerate `zstd` compression at the hardware level. The Minion implementation should leverage native libraries (like the `zstd` C library wrapped in Rust or Python) rather than slow, pure-language implementations to ensure it can utilize this hardware acceleration.
* **Protocol Handshake:** During the initial SDP Exchange, the Minion and SwarmClaw negotiate compression support. If both support it, all subsequent JSON-RPC payloads are wrapped in a Zstd frame. If a minimalist Minion does not support `zstd`, the protocol falls back to uncompressed MessagePack, though performance will degrade for large payloads.

---

## Minion Tool Discovery & Export
The true power of a Minion is not just running isolated `bash` commands; it is safely exposing complex desktop functionality (like taking a screenshot, reading a local file, or automating an IDE) directly to the cloud AI.

To achieve this without reinventing the wheel, **Minions should implement the Model Context Protocol (MCP) over the WebRTC Data Channel.**

### Zero-Trust Secrets (Edge-Side Auth)
A critical security advantage of the Minion architecture is **Edge-Side Authentication**. 
* **The Vulnerability:** If a cloud agent needs to act on a user's behalf (e.g., managing GitHub issues), passing the user's `GITHUB_TOKEN` from the desktop up to the cloud agent exposes the token to potential interception or cloud-side prompt injection leaks.
* **The Solution:** The cloud SwarmClaw agent **never receives the secret**. Instead, the user stores their API tokens locally in the Minion's own vault (e.g., macOS Keychain, Windows Credential Manager, or encrypted local file).
* **The Execution:** When SwarmClaw sends an MCP command over WebRTC (e.g., `{"method": "tools/call", "params": {"name": "github_create_issue"}}`), the Minion intercepts this, attaches the locally-stored `GITHUB_TOKEN` to the outbound HTTP request, and returns only the result to the cloud agent. This guarantees that even if the cloud agent goes completely rogue, it cannot steal the user's underlying credentials.

### How SwarmClaw Discovers Minion Tools
1. **The Handshake:** Once the WebRTC Data Channel is open, SwarmClaw sends an MCP `tools/list` request to the Minion.
2. **The Response (Context for the LLM):** The Minion replies with a JSON schema of all the tools the user has explicitly allowed in their `minion.yaml`. Crucially, this schema includes rich `description` fields. These descriptions are injected directly into the cloud SwarmClaw's LLM context window, teaching the AI *how* and *when* to use the newly exposed local tools.
   ```json
   {
     "jsonrpc": "2.0",
     "id": "mcp-1",
     "result": {
       "tools": [
         {
           "name": "capture_screen",
           "description": "Takes a screenshot of the user's primary desktop.",
           "inputSchema": { "type": "object", "properties": {} }
         },
         {
           "name": "safe_git_commit",
           "description": "Stages changes and commits with a generated message.",
           "inputSchema": { 
             "type": "object", 
             "properties": { "message": {"type": "string"} } 
           }
         }
       ]
     }
   }
   ```
3. **Execution:** SwarmClaw registers these tools dynamically into its LLM context. When the AI decides to call `capture_screen`, SwarmClaw sends an MCP `tools/call` JSON-RPC request over the WebRTC channel to the Minion. The Minion executes the native screenshot code and returns the base64 compressed image over the data channel.

By adopting MCP as the semantic layer over the WebRTC transport layer, any community-built Minion instantly becomes fully compatible with SwarmClaw, Claude, and any other MCP-compliant agent.

---

## Minion Discovery & Installation via ClawNet (Phase 4)
*Note: This marketplace discovery mechanism is planned for Phase 4. Phase 3 introduces the manual execution of the Minion binary by the user.*

To foster a vibrant ecosystem, users should not have to compile Minions from source. The **ClawNet** interface acts as the discovery hub and marketplace for Minions.

### The Minion Marketplace
* Within the ClawNet desktop or web app, users can browse a directory of verified, community-built Minions.
* Examples: "MacOS UI Automator Minion", "Data Science Python Minion", "Docker-in-Docker Minion".
* **Installation:** Clicking "Install" on a Minion automatically downloads the lightweight binary for the user's operating system (macOS/Windows/Linux) and sets up the default `minion.yaml` policy file in their local application support directory.
* **Connection Handoff:** Once installed, ClawNet seamlessly generates the single-use `mothership://minion/connect...` URL and automatically passes it to the newly installed Minion binary to bootstrap the WebRTC connection without the user needing to copy/paste anything manually.

---

## Why this Protocol Matters
By formalizing this JSON-RPC over WebRTC protocol, we decouple the agent from the executor. 
* A developer could write a custom Minion in **Node.js** to safely expose their desktop Chrome browser to a cloud AI.
* A developer could write a custom Minion in **C#** to safely expose macOS native APIs to a cloud AI.
As long as the external program speaks this WebRTC + JSON-RPC protocol, it can act as a secure, local proxy for any SwarmClaw agent.
