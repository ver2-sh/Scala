use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

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
    /// Inspect public authentication or manage Norted API keys
    Auth(AuthArgs),
    /// Show current server, model, and engine state
    Status,
    /// Load a discovered model through the running Norted Server instance
    Load {
        /// Stable model ID from `norted-server models list`
        model_id: String,
        /// Exact installed runtime ID; overrides model and format selections
        #[arg(long)]
        runtime: Option<String>,
        /// Named load profile for this invocation; replaces the model assignment
        #[arg(long)]
        profile: Option<String>,
        /// Ephemeral structured load override (repeatable SETTING_ID=VALUE)
        #[arg(long = "set", value_name = "SETTING_ID=VALUE")]
        settings: Vec<String>,
    },
    /// Unload the active model from the running Norted Server instance
    Unload,
    /// Inspect locally discovered model artifacts
    Models(ModelsArgs),
    /// Inspect available inference engines
    Engines(EnginesArgs),
    /// Search, install, select, update, and remove concrete runtime packs
    Runtimes(RuntimesArgs),
    /// Create, edit, assign, and inspect reusable load profiles
    Profiles(ProfilesArgs),
    /// Manage defaults and inspect exact-runtime load settings
    Settings(SettingsArgs),
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
    /// Show private serving capabilities for one discovered model
    Info { model_id: String },
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Show configured/effective public authentication and bind safety
    Status,
    /// List, create, or revoke Norted API keys
    Keys(AuthKeysArgs),
}

#[derive(Debug, Args)]
pub struct AuthKeysArgs {
    #[command(subcommand)]
    pub command: AuthKeysCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthKeysCommand {
    /// List key metadata without secrets or digests
    List,
    /// Create a strong random API key; its secret is shown once
    Create {
        /// Human-readable key label
        #[arg(long)]
        name: Option<String>,
    },
    /// Revoke an API key by its stable key ID
    Revoke { key_id: String },
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

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    #[command(subcommand)]
    pub command: ProfilesCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProfilesCommand {
    /// List named profiles and model assignments
    List,
    /// Show one named profile
    Show { name: String },
    /// Create an empty named profile
    Create { name: String },
    /// Delete an unassigned named profile
    Delete { name: String },
    /// Set one or more values in a named profile
    Set {
        name: String,
        #[arg(required = true, value_name = "SETTING_ID=VALUE")]
        settings: Vec<String>,
    },
    /// Remove values from a named profile so they inherit again
    Unset {
        name: String,
        #[arg(required = true, value_name = "SETTING_ID")]
        settings: Vec<String>,
    },
    /// Persist one named profile assignment for a model
    Assign {
        #[arg(long)]
        model: String,
        name: String,
    },
    /// Clear a model's named profile assignment
    ClearAssignment {
        #[arg(long)]
        model: String,
    },
}

#[derive(Debug, Args)]
pub struct SettingsArgs {
    #[command(subcommand)]
    pub command: SettingsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Show effective values and their precedence sources for an exact runtime
    Show {
        #[arg(long)]
        model: String,
        #[arg(long)]
        runtime: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Show the exact selected runtime's structured setting schema
    Schema {
        #[arg(long)]
        model: String,
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Set one or more persisted defaults
    Set(SettingsMutationArgs),
    /// Remove persisted defaults so lower layers inherit again
    Unset(SettingsUnsetArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(true)
        .multiple(false)
        .args(["global", "engine", "model"])
))]
pub struct SettingsMutationArgs {
    #[arg(long)]
    pub global: bool,
    #[arg(long)]
    pub engine: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(required = true, value_name = "SETTING_ID=VALUE")]
    pub settings: Vec<String>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(true)
        .multiple(false)
        .args(["global", "engine", "model"])
))]
pub struct SettingsUnsetArgs {
    #[arg(long)]
    pub global: bool,
    #[arg(long)]
    pub engine: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(required = true, value_name = "SETTING_ID")]
    pub settings: Vec<String>,
}
