# SwarmClaw 🦀🤖

Welcome to the **SwarmClaw** core repository.

This repository contains the high-performance, Rust-native AI agent runtime designed for massive scale, P2P federation, and Zero-Copy WebAssembly (WASM) execution.

![SwarmClaw Logo](assets/logo.svg)

## Core Philosophy
SwarmClaw is built to be the "Brain" of the Mothership ecosystem. While Mothership provides the compute "Body," SwarmClaw provides the intelligent execution layer. It is designed to be:
- **Fast:** Zero-copy memory management using FlatBuffers.
- **Secure:** Hard-sandboxed WASM runtime for agent skills.
- **Scalable:** Built-in support for massive agent fleets and P2P federation.

## Repository Structure
- **`swarmclaw/`**: The core orchestrator and runtime binary.
- **`swarmclaw-sdk/`**: The Rust SDK for building Zero-Copy WASM skills.
- **`skills-library/`**: Standard securely sandboxed tools (Filesystem, Shell, etc.).
- **`docs/`**: Core protocol specifications:
    - [Minion WebRTC Protocol](docs/MINION_INTERFACE.md)
    - [Fleet Scaling Strategy](docs/FLEET_STRATEGY.md)
- **`assets/`**: Brand artwork and concept designs.

## Licensing
This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
