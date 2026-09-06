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
    /// Reclaim unused managed storage; --all resets generated/heavy artifacts
    Prune(PruneArgs),
    /// Launch the interactive interface, attaching to or owning the serving stack
    Tui,
    /// Run the local HTTP API gateway headlessly until interrupted
    Serve,
    /// Inspect public authentication or manage Norted API keys
    Auth(AuthArgs),
    /// Show current server, model, and engine state
    Status,
    /// Load a user-created Model Profile through the running Norted Server instance
    Load {
        /// Model Profile ID from `norted-server model-profiles list`
        model_profile_id: String,
        /// Exact installed runtime ID; overrides model and format selections
        #[arg(long)]
        runtime: Option<String>,
        /// Ephemeral serving override (repeatable SETTING_ID=VALUE)
        #[arg(long = "set", value_name = "SETTING_ID=VALUE")]
        settings: Vec<String>,
    },
    /// Unload one Model Profile from the running Norted Server instance
    Unload { model_profile_id: String },
    /// Inspect locally discovered model artifacts
    Models(ModelsArgs),
    /// Inspect available inference engines
    Engines(EnginesArgs),
    /// Search, install, select, update, and remove concrete runtime packs
    Runtimes(RuntimesArgs),
    /// Create, edit, load, and inspect user-owned Model Profiles
    ModelProfiles(ModelProfilesArgs),
    /// Manage Server Settings and independent engine overrides
    Settings(SettingsArgs),
    /// Inspect resolved application configuration
    Config(ConfigArgs),
    /// Run offline, read-only whole-system diagnostics
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Remove all managed models, runtimes, caches, logs and transient state
    #[arg(long)]
    pub all: bool,
    /// Show the exact deletion plan and estimated bytes without writing
    #[arg(long)]
    pub dry_run: bool,
    /// Confirm destructive --all without an interactive prompt
    #[arg(long, requires = "all")]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Show successful checks in addition to warnings and failures
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Debug, Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// List recognized .gguf, .q27, and .ninfer artifacts
    List,
    /// Show private serving capabilities for one discovered model
    Info { model_id: String },
    /// Search Hugging Face for concrete downloadable model artifacts
    Search {
        /// Repository, model, publisher, or artifact search text
        query: Option<String>,
        /// Restrict results to one artifact format
        #[arg(long)]
        format: Option<norted_core::ArtifactFormat>,
    },
    /// Download and validate one exact artifact reference returned by search
    Download { model_ref: String },
    /// Safely copy an existing local artifact into the managed library
    Import { path: std::path::PathBuf },
    /// Remove the managed acquisition containing this model
    Remove { model_id: String },
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
    /// Search authoritative upstream runtime candidates
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
pub struct ModelProfilesArgs {
    #[command(subcommand)]
    pub command: ModelProfilesCommand,
}

#[derive(Debug, Subcommand)]
pub enum ModelProfilesCommand {
    /// List user-owned Model Profiles
    List,
    /// Show one Model Profile and its effective settings
    Show { profile: String },
    /// Create a Model Profile bound to one concrete model and engine
    Create {
        profile: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        engine: String,
        /// Default inference role for this profile
        #[arg(long, value_enum, default_value_t)]
        role: ProfileRole,
    },
    /// Duplicate a Model Profile under a new ID
    Duplicate { source: String, profile: String },
    /// Delete an inactive Model Profile
    Delete { profile: String },
    /// Change the concrete model binding
    SetModel { profile: String, model: String },
    /// Change the engine binding
    SetEngine { profile: String, engine: String },
    /// Change the default inference role
    SetRole { profile: String, role: ProfileRole },
    /// Set one or more Model Profile overrides
    Set {
        profile: String,
        #[arg(required = true, value_name = "SETTING_ID=VALUE")]
        settings: Vec<String>,
    },
    /// Clear profile overrides to inherit from Settings
    Unset {
        profile: String,
        #[arg(required = true, value_name = "SETTING_ID")]
        settings: Vec<String>,
    },
    /// Load the Model Profile through the running control server
    Load {
        profile: String,
        #[arg(long)]
        runtime: Option<String>,
        #[arg(long = "set", value_name = "SETTING_ID=VALUE")]
        settings: Vec<String>,
    },
    /// Evaluate the bound model + engine + settings against a runtime
    Compatibility {
        profile: String,
        #[arg(long)]
        runtime: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ProfileRole {
    #[default]
    Primary,
    Auxiliary,
}

impl From<ProfileRole> for norted_core::ModelRole {
    fn from(value: ProfileRole) -> Self {
        match value {
            ProfileRole::Primary => Self::Primary,
            ProfileRole::Auxiliary => Self::Auxiliary,
        }
    }
}

#[derive(Debug, Args)]
pub struct SettingsArgs {
    #[command(subcommand)]
    pub command: SettingsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Show Server Settings or independent engine overrides and runtime baselines
    Show {
        #[arg(long, conflicts_with = "runtime", required_unless_present = "runtime")]
        server: bool,
        #[arg(long, conflicts_with = "server", required_unless_present = "server")]
        runtime: Option<String>,
    },
    /// Set one or more explicit local overrides
    Set(SettingsMutationArgs),
    /// Remove persisted values so the runtime baseline applies again
    Unset(SettingsUnsetArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(true)
        .multiple(false)
        .args(["server", "runtime"])
))]
pub struct SettingsMutationArgs {
    #[arg(long)]
    pub server: bool,
    #[arg(long)]
    pub runtime: Option<String>,
    #[arg(long)]
    #[arg(required = true, value_name = "SETTING_ID=VALUE")]
    pub settings: Vec<String>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("scope")
        .required(true)
        .multiple(false)
        .args(["server", "runtime"])
))]
pub struct SettingsUnsetArgs {
    #[arg(long)]
    pub server: bool,
    #[arg(long)]
    pub runtime: Option<String>,
    #[arg(long)]
    #[arg(required = true, value_name = "SETTING_ID")]
    pub settings: Vec<String>,
}
