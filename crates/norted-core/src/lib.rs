//! Engine-neutral application services and domain types.

mod auth;
mod config;
mod error;
mod event;
mod load_settings;
mod model;
mod provenance;
mod runtime;
mod runtime_pack;
mod state;

pub use auth::{
    API_KEY_PREFIX, API_KEYS_SCHEMA_VERSION, ApiKeyRecord, ApiKeyState, ApiKeyStore,
    ApiKeyStoreError, ApiKeySummary, CreatedApiKey, EffectivePublicAuthMode, MAX_API_KEYS,
    PublicAuthMode, PublicAuthStatus,
};
pub use config::{
    AppConfig, AppPaths, ConfigSource, EngineConfig, LoadedConfig, ModelConfig,
    SUPPORTED_CONFIG_VERSION, ServerConfig, TuiConfig,
};
pub use error::{CoreError, Result};
pub use event::{AppEvent, LogLevel};
pub use load_settings::{
    GpuOffload, LOAD_PROFILES_SCHEMA_VERSION, LoadProfile, LoadProfileName, LoadProfilesError,
    LoadProfilesState, LoadProfilesStore, LoadSettingDefinition, LoadSettingId, LoadSettingKind,
    LoadSettingScope, LoadSettingSource, LoadSettingValue, LoadSettingsError, LoadSettingsPatch,
    LoadSettingsSchema, ResolvedLoadSetting, ResolvedLoadSettings, UnsignedIntegerOrChoiceValue,
};
pub use model::{
    ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifact, AuxiliaryArtifactRole,
    ModelArtifact, ModelArtifactProvenance, ModelId, ModelRegistry, NinferArtifactIdentity,
    NinferContainerError, NinferContainerMetadata, inspect_ninfer_container,
};
pub use provenance::{
    AcquisitionMethod, AuxiliaryRuntimeIdentity, BuildProvenance, EngineInstallation,
    EngineRevision, EnvironmentVariableProvenance, LoadSettingsProvenance, ModelRuntimeIdentity,
    NativeArgumentProvenance, ProcessIdentity, RuntimeProvenance, ToolchainProvenance,
};
pub use runtime::{
    RuntimeDescriptor, RuntimeObservationError, RuntimePublisher, observe_runtime,
    observe_runtime_descriptor,
};
pub use runtime_pack::{
    AcceleratorDevice, AvailableRuntime, ComputeCapability, HostCapabilities, InstalledRuntime,
    MINIMUM_RUNTIME_MANIFEST_SCHEMA_VERSION, RUNTIME_MANIFEST_SCHEMA_VERSION,
    RUNTIME_SELECTIONS_SCHEMA_VERSION, RuntimeAcquisitionMethod, RuntimeAcquisitionPlan,
    RuntimeArchiveFormat, RuntimeCompatibility, RuntimeDigest, RuntimeDigestError, RuntimeDownload,
    RuntimeId, RuntimeIdentity, RuntimeIdentityError, RuntimeManifest, RuntimeManifestError,
    RuntimeOperationPhase, RuntimeOperationProgress, RuntimePackageAssetIdentity,
    RuntimePackageIdentity, RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
    RuntimeSelection, RuntimeSelectionSource, RuntimeSelections, RuntimeSourceBuildPlan,
    RuntimeSourceBuildPrerequisites, RuntimeSourceBuildProvenance, RuntimeSourceBuildRecipe,
    RuntimeSourceBuildToolchain, RuntimeSourceSnapshot, RuntimeUpdatePreference,
    RuntimeUpdateState, is_full_git_sha, is_safe_relative_path,
};
pub use state::{AppSnapshot, ApplicationCore, RegistryState, ServerState};
