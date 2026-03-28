use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to the agent workspace
    #[arg(short, long)]
    pub workspace: Option<String>,

    /// Specific Agent ID to run
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Enable verbose logging
    #[arg(short, long, default_value_t = false)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the agent (default)
    Run,

    /// Print a basic health summary and exit
    Status,

    /// Repackage a WASM skill into a native object file
    Repackage {
        /// Path to the .wasm file
        input: String,
        /// Output path for the object file
        #[arg(short, long)]
        output: Option<String>,
    },

    /// List installed skills
    Skills,

    /// List persisted SwarmClaw sessions
    Sessions {
        /// Maximum number of sessions to print
        #[arg(short, long, default_value_t = 25)]
        limit: usize,
    },

    /// Show recent history for a persisted session
    History {
        /// Session ID to inspect. Defaults to --agent or `default`.
        session: Option<String>,

        /// Maximum number of messages to print
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
    },

    /// Inspect outbound outbox messages and dead letters
    Outbox {
        /// Optional status filter: pending, in_flight, synced, failed
        #[arg(short, long)]
        status: Option<String>,

        /// Maximum number of messages to print
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
    },
}
