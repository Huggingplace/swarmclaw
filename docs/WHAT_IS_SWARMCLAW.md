# What is SwarmClaw?

SwarmClaw is an **agentic automation framework and ecosystem** designed for the modern, multi-channel developer. It is the "intelligent connective tissue" of the Mothership platform.

While NeverStop provides the human-computer interface (the terminal and dashboard) and Mothership provides the raw compute infrastructure (the Fleet), **SwarmClaw** provides the autonomous workers.

## Core Philosophy: Zero-Bloat & Webhook-First
Most AI agents today are heavy, polling-based daemon processes that constantly consume compute resources just waiting for something to happen. 

SwarmClaw rejects this model.

It is built on a **pure webhook-first architecture**. A SwarmClaw agent sits completely dormant—consuming zero compute cycles—until a triggering event occurs (e.g., a message in Discord, an update to a Jira ticket, or a GitHub push). When an event occurs, the Mothership infrastructure instantly routes the payload to the agent, which processes the context, executes its deterministic policy, and spins back down.

## What Does a SwarmClaw Actually Do?
A SwarmClaw agent bridges the gap between conversational chat platforms and hard engineering tasks. 

Imagine an agent sitting in your team's Discord server:
1.  **Media Triage ("MediaLink"):** A designer drops a 50MB video file into Discord. The SwarmClaw agent instantly intercepts it, utilizes Mothership Fleet compute to transcode and upload it to enterprise storage, and returns a tiny 1kb shareable link back to the chat, saving everyone's local disk space and bandwidth.
2.  **Code Contextualization:** You see a fascinating new open-source library on GitHub. Instead of spending two hours reading the source code, you click a "Launch SwarmClaw" badge. A temporary agent is instantly provisioned, ingests the entire repository context, and allows you to ask it architectural questions directly from your phone.
3.  **Secure Execution (via Minions):** A SwarmClaw agent is asked to fetch live logs from a production database. Instead of holding raw SSH keys, it communicates with a local **Minion daemon** running securely in the environment, executing only whitelisted, heavily audited commands.

## Key Capabilities

*   **Multi-Channel By Default:** SwarmClaws natively speak the APIs of Discord, Telegram, Slack, and GitHub. You don't have to write platform-specific bot logic; you just define the agent's goal.
*   **Security & Secret Redaction:** Built entirely in memory-safe Rust, the SwarmClaw framework features an internal Regex middleware layer. Before an agent can ever post a message back to Discord or log its actions to the database, its output is violently scrubbed for API keys, UUIDs, and passwords, ensuring zero leakage of enterprise secrets.
*   **Real-Time Streaming:** Unlike standard chatbots that "think" for 10 seconds and drop a massive block of text, SwarmClaw utilizes async chunking to deliver responses word-by-word to platforms like Discord and Telegram, making interactions feel instantaneous.
*   **Fleet Backed:** When a SwarmClaw needs to perform a heavy task (like compiling a Rust binary or transcoding video), it doesn't do it in the lightweight web server. It uses the Mothership Fleet API to request a dedicated `FleetJob` container, executes the task, and destroys the container.

## The Viral Ecosystem
SwarmClaw isn't just a tool; it's designed to be a social, viral network. 
*   **The Groupchat Presence:** Because SwarmClaws natively integrate with consumer chat platforms (iMessage via Beeper, Telegram, WhatsApp), developers can simply invite a specialized Claw into their friend group or engineering team chat. It becomes an active participant that can resolve disputes, summarize long threads, or execute tools natively in the conversation.
*   **Claw-to-Claw Collaboration ("Introduce"):** When you spin up a temporary Claw from a one-click GitHub link, you don't have to work with it in isolation. You can **"Introduce"** your "Main Claw" to this new, specialized Claw. They can chat with each other directly to brainstorm solutions, cross-reference their knowledge bases, and complete complex multi-step tasks.