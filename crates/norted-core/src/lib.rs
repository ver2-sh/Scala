//! Engine-neutral application services and domain types.

mod config;
mod error;
mod event;
mod model;
mod provenance;
mod runtime;
mod runtime_pack;
mod state;

pub use config::{
    AppConfig, AppPaths, ConfigSource, EngineConfig, LoadedConfig, ModelConfig,
    SUPPORTED_CONFIG_VERSION, ServerConfig, TuiConfig,
};
pub use error::{CoreError, Result};
pub use event::{AppEvent, LogLevel};
pub use model::{
    ArtifactFormat, AuxiliaryArtifact, AuxiliaryArtifactRole, ModelArtifact,
    ModelArtifactProvenance, ModelId, ModelRegistry,
};
pub use provenance::{
    AcquisitionMethod, AuxiliaryRuntimeIdentity, BuildProvenance, EngineInstallation,
    EngineRevision, EnvironmentVariableProvenance, ModelRuntimeIdentity, NativeArgumentProvenance,
    ProcessIdentity, RuntimeProvenance, ToolchainProvenance,
};
pub use runtime::{
    RuntimeDescriptor, RuntimeObservationError, RuntimePublisher, observe_runtime,
    observe_runtime_descriptor,
};
pub use runtime_pack::{
    AcceleratorDevice, AvailableRuntime, ComputeCapability, HostCapabilities, InstalledRuntime,
    RUNTIME_MANIFEST_SCHEMA_VERSION, RUNTIME_SELECTIONS_SCHEMA_VERSION, RuntimeAcquisitionMethod,
    RuntimeArchiveFormat, RuntimeCompatibility, RuntimeDigest, RuntimeDigestError, RuntimeDownload,
    RuntimeId, RuntimeIdentity, RuntimeIdentityError, RuntimeManifest, RuntimeManifestError,
    RuntimeOperationPhase, RuntimeOperationProgress, RuntimePackageAssetIdentity,
    RuntimePackageIdentity, RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
    RuntimeSelection, RuntimeSelectionSource, RuntimeSelections, RuntimeUpdatePreference,
    RuntimeUpdateState, is_safe_relative_path,
};
pub use state::{AppSnapshot, ApplicationCore, RegistryState, ServerState};
