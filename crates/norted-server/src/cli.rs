use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "norted-server",
    version,
    about = "Local language-model server and runtime manager",
    long_about = "Inspect local model artifacts and the Norted HTTP foundation from a polished terminal interface or deterministic CLI commands. No inference engine is implemented yet."
)]
pub struct Cli {
    /// Emit machine-readable JSON where supported
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Launch the interactive terminal interface
    Tui,
    /// Run the local HTTP API gateway until interrupted
    Serve,
    /// Show current server, model, and engine state
    Status,
    /// Inspect locally discovered model artifacts
    Models(ModelsArgs),
    /// Inspect available inference engines
    Engines(EnginesArgs),
    /// Inspect resolved application configuration
    Config(ConfigArgs),
    /// Check configuration, paths, networking, and terminal environment
    Doctor,
}

#[derive(Debug, Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// List recognized .gguf and .q27 artifacts
    List,
}

#[derive(Debug, Args)]
pub struct EnginesArgs {
    #[command(subcommand)]
    pub command: EnginesCommand,
}

#[derive(Debug, Subcommand)]
pub enum EnginesCommand {
    /// List registered engine adapters and installation state
    List,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved configuration and its platform path
    Show,
}
