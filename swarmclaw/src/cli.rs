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
}
