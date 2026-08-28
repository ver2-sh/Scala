use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum RuntimeUpdateTrack {
    Stable,
    Latest,
    #[default]
    Pinned,
}

impl From<RuntimeUpdateTrack> for norted_core::RuntimeUpdatePreference {
    fn from(value: RuntimeUpdateTrack) -> Self {
        match value {
            RuntimeUpdateTrack::Stable => Self::Stable,
            RuntimeUpdateTrack::Latest => Self::Latest,
            RuntimeUpdateTrack::Pinned => Self::Pinned,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "norted-server",
    version,
    about = "Local language-model server and runtime manager",
    long_about = "Discover local model artifacts, manage independently versioned engine runtimes, and serve local text generation through the Norted gateway."
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
    /// Load a discovered model through the running Norted Server instance
    Load {
        /// Stable model ID from `norted-server models list`
        model_id: String,
        /// Exact installed runtime ID; overrides model and format selections
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Unload the active model from the running Norted Server instance
    Unload,
    /// Inspect locally discovered model artifacts
    Models(ModelsArgs),
    /// Inspect available inference engines
    Engines(EnginesArgs),
    /// Search, install, select, update, and remove concrete runtime packs
    Runtimes(RuntimesArgs),
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
pub struct RuntimesArgs {
    #[command(subcommand)]
    pub command: RuntimesCommand,
}

#[derive(Debug, Subcommand)]
pub enum RuntimesCommand {
    /// List installed managed and configured external runtimes
    List,
    /// Search authoritative upstream runtime releases
    Search {
        /// Engine, backend, format, version, or other search text
        query: Option<String>,
        /// Ignore a fresh catalog cache and contact providers now
        #[arg(long)]
        refresh: bool,
    },
    /// Show an exact installed or available runtime
    Info { runtime_ref: String },
    /// Install one exact available runtime pack
    Install { runtime_ref: String },
    /// Remove one inactive, unselected managed runtime version
    Remove { runtime_ref: String },
    /// Check installed runtimes for newer compatible packs
    CheckUpdates,
    /// Install the newest compatible pack side-by-side
    Update { runtime_ref: String },
    /// Select an exact installed runtime for one format or model
    Select {
        #[arg(long, conflicts_with = "model", required_unless_present = "model")]
        format: Option<norted_core::ArtifactFormat>,
        #[arg(long, conflicts_with = "format", required_unless_present = "format")]
        model: Option<String>,
        /// Update channel to follow while keeping this exact runtime selected
        #[arg(long, value_enum, default_value_t)]
        track: RuntimeUpdateTrack,
        runtime_ref: String,
    },
    /// Clear a persisted format or model runtime selection
    ClearSelection {
        #[arg(long, conflicts_with = "model", required_unless_present = "model")]
        format: Option<norted_core::ArtifactFormat>,
        #[arg(long, conflicts_with = "format", required_unless_present = "format")]
        model: Option<String>,
    },
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
