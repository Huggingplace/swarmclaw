# Secure Stub Retrieval (Cloud Tiering)

When a SwarmClaw Minion tiers a heavy file (like a 5MB photo or a 3GB application) to the cloud to save local disk space, it leaves behind a tiny 1KB "Stub" or "Pointer". 

This document outlines the security architecture that guarantees only the authorized Minion can rehydrate (retrieve) the original file, ensuring that the cloud acts as a **Zero-Trust blind hard drive**.

## 1. Cryptographic Stubs (Not URLs)

The 1KB stub left on the user's device does **not** contain a public HTTPS URL (e.g., `https://s3.amazonaws.com/bucket/file`). Storing public URLs creates massive vulnerability surface areas (e.g., IDOR or brute-force guessing).

Instead, the Minion stub contains a strict, cryptographically bound payload:
```json
{
  "file_id": "uuid-v4",
  "content_hash_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "mothership_node_id": "target-1770347851627",
  "decryption_key_wrap": "local_keychain_reference" // Optional KMS/Local wrap
}
```

### Encryption at Rest (Client-Side)
Before the Minion streams the file to the SwarmClaw `Organizer`, it can encrypt the binary payload locally. The decryption key remains *only* in the Minion's local keychain (or inside the stub itself, protected by the OS enclave). The cloud agent receives and stores an encrypted blob it cannot read.

## 2. WebRTC Reverse-Tunnel Retrieval

When the user (or the OS FileProvider) requests to open the stubbed file, the Minion does not make a standard HTTP GET request. It uses the established **WebRTC Data Channel**.

1.  **Intercept:** The Minion intercepts the file read request.
2.  **Tunnel:** The Minion sends a JSON-RPC request over the encrypted P2P WebRTC tunnel to the `Coordinator_SuperClaw`.
    `{"method": "files/rehydrate", "params": {"file_id": "uuid-v4"}}`
3.  **Stream:** The SwarmClaw agent locates the blob in its attached cloud storage and streams the binary data back down the WebRTC channel using FlatBuffers and Zstd compression.

## 3. Edge-Side Authentication (Zero-Trust)

Because the retrieval relies entirely on the WebRTC Data Channel, it natively inherits the Minion's strict authentication layer.
*   **No Cloud API Keys:** The Minion doesn't need to store or transmit an S3 or GCP access token to fetch the file.
*   **Session Validation:** The cloud agent only responds to rehydration requests that come through a currently authenticated, user-approved WebRTC session.
*   **Network Isolation:** If an attacker discovers the `file_id`, they cannot download the file from the public internet, because the SwarmClaw agent does not expose an HTTP server for the file. The data only flows over the P2P WebRTC tunnel.

## Summary

By combining **Client-Side Encryption**, **Cryptographic Hashing**, and **WebRTC Data Channels**, SwarmClaw allows edge devices (like iPhones) to securely offload unlimited amounts of data to the cloud, while guaranteeing that the cloud infrastructure remains completely blind to the content and incapable of leaking it.