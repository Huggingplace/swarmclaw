FROM rust:1.81-bookworm AS builder

WORKDIR /usr/src/app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        cmake \
        flatbuffers-compiler \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Mirror the current repo layout needed by the swarmclaw_core workspace.
COPY swarmclaw_core/Cargo.toml swarmclaw_core/Cargo.lock ./swarmclaw_core/
COPY swarmclaw_core/swarmclaw ./swarmclaw_core/swarmclaw
COPY swarmclaw_core/swarmclaw-sdk ./swarmclaw_core/swarmclaw-sdk
COPY swarmclaw_core/skills-library ./swarmclaw_core/skills-library
COPY mothership-media ./mothership-media

WORKDIR /usr/src/app/swarmclaw_core
RUN cargo build --release -p swarmclaw

FROM debian:bookworm-slim

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/swarmclaw_core/target/release/swarmclaw /usr/local/bin/swarmclaw

RUN mkdir -p /app/workspace

ENV OPENCLAW_WORKSPACE=/app/workspace

ENTRYPOINT ["swarmclaw"]
CMD ["run"]
