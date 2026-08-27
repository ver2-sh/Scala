//! Engine-neutral application services and domain types.

mod config;
mod error;
mod event;
mod model;
mod provenance;
mod state;

pub use config::{
    AppConfig, AppPaths, ConfigSource, EngineConfig, LoadedConfig, ModelConfig, ServerConfig,
    TuiConfig,
};
pub use error::{CoreError, Result};
pub use event::{AppEvent, LogLevel};
pub use model::{ArtifactFormat, ModelArtifact, ModelId, ModelRegistry};
pub use provenance::{BuildProvenance, EngineRevision, RuntimeProvenance};
pub use state::{AppSnapshot, ApplicationCore, RuntimeStatus, ServerState};
