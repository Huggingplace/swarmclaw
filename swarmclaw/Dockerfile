# Builder Stage
FROM rust:1.81-bookworm AS builder

WORKDIR /usr/src/app

# Install system dependencies
RUN apt-get update && apt-get install -y 
    cmake 
    pkg-config 
    libssl-dev 
    && rm -rf /var/lib/apt/lists/*

# Install flatc (needed for build.rs)
RUN apt-get update && apt-get install -y flatbuffers-compiler && rm -rf /var/lib/apt/lists/*

# Copy the workspace configuration and the crate
COPY Cargo.toml Cargo.lock ./
COPY huggingplace-rustclaw ./huggingplace-rustclaw
COPY mothership-common ./mothership-common
COPY mothership-conn ./mothership-conn
COPY mothership-media ./mothership-media
COPY mothership-proto ./mothership-proto
COPY mothership-cli-rs ./mothership-cli-rs
COPY mothership-cloud-sdk ./mothership-cloud-sdk
COPY never-stop ./never-stop
COPY mothership/carrier ./mothership/carrier
COPY mothership/engine ./mothership/engine
COPY mothership/client/rust ./mothership/client/rust
COPY mothership/server ./mothership/server
COPY mothership/migration ./mothership/migration

# Build the binary
RUN cargo build --release -p huggingplace-rustclaw

# Runtime Stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y 
    ca-certificates 
    libssl3 
    && rm -rf /var/lib/apt/lists/*

# Copy the binary
COPY --from=builder /usr/src/app/target/release/rustclaw /usr/local/bin/rustclaw

# Create workspace directory
RUN mkdir -p /app/workspace

ENV OPENCLAW_WORKSPACE=/app/workspace

ENTRYPOINT ["rustclaw"]
CMD ["run"]
