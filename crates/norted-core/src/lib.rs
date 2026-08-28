//! Engine-neutral application services and domain types.

mod config;
mod error;
mod event;
mod model;
mod provenance;
mod runtime;
mod state;

pub use config::{
    AppConfig, AppPaths, ConfigSource, EngineConfig, LoadedConfig, ModelConfig,
    SUPPORTED_CONFIG_VERSION, ServerConfig, TuiConfig,
};
pub use error::{CoreError, Result};
pub use event::{AppEvent, LogLevel};
pub use model::{ArtifactFormat, ModelArtifact, ModelArtifactProvenance, ModelId, ModelRegistry};
pub use provenance::{
    AcquisitionMethod, BuildProvenance, EngineInstallation, EngineRevision,
    EnvironmentVariableProvenance, ModelRuntimeIdentity, NativeArgumentProvenance, ProcessIdentity,
    RuntimeProvenance, ToolchainProvenance,
};
pub use runtime::{
    RuntimeDescriptor, RuntimeObservationError, RuntimePublisher, observe_runtime,
    observe_runtime_descriptor,
};
pub use state::{AppSnapshot, ApplicationCore, RegistryState, ServerState};
