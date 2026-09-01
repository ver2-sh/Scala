//! Engine-neutral application services and domain types.

mod auth;
mod config;
mod error;
mod event;
mod model;
mod model_profile;
mod norted_package;
mod provenance;
mod runtime;
mod runtime_pack;
mod settings;
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
pub use model::{
    ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifact, AuxiliaryArtifactRole,
    GgufArtifactIdentity, GgufMetadataError, MODEL_LIBRARY_RECEIPT_FILENAME, ModelArtifact,
    ModelArtifactProvenance, ModelId, ModelLibraryReceipt, ModelLibraryReceiptMember,
    ModelRegistry, NinferArtifactIdentity, NinferContainerError, NinferContainerMetadata,
    inspect_gguf_metadata, inspect_ninfer_container, model_library_receipt_path,
    q27_tokenizer_candidate, select_q27_tokenizer_filename, validate_q27_tokenizer_header,
};
pub use model_profile::{
    EngineId, MODEL_PROFILES_STATE_VERSION, ModelProfile, ModelProfileId, ModelProfilesState,
    ModelProfilesStore, validate_engine_id,
};
pub use norted_package::{
    NortedPackageAcquisitionFile, NortedPackageAcquisitionPlan, NortedPackageAcquisitionRole,
    NortedPackageBinding, NortedPackageFile, NortedPackageKind, norted_package_manifest_name,
    plan_norted_package_acquisition, recover_norted_package_primary_paths,
};
pub use provenance::{
    AcquisitionMethod, AuxiliaryRuntimeIdentity, BuildProvenance, EngineInstallation,
    EngineRevision, EnvironmentVariableProvenance, ModelProfileRuntimeIdentity,
    ModelRuntimeIdentity, NativeArgumentProvenance, NortedPackageRuntimeIdentity, ProcessIdentity,
    RuntimeProvenance, SettingsProvenance, ToolchainProvenance,
};
pub use runtime::{
    RuntimeDescriptor, RuntimeObservationError, RuntimePublisher, observe_runtime,
    observe_runtime_descriptor, observe_runtime_descriptor_read_only,
};
pub use runtime_pack::{
    AcceleratorDevice, AvailableRuntime, ComputeCapability, HostCapabilities, InstalledRuntime,
    MINIMUM_RUNTIME_MANIFEST_SCHEMA_VERSION, RUNTIME_MANIFEST_SCHEMA_VERSION,
    RUNTIME_SELECTIONS_SCHEMA_VERSION, RuntimeAcquisitionMethod, RuntimeAcquisitionPlan,
    RuntimeArchiveFormat, RuntimeCompatibility, RuntimeDigest, RuntimeDigestError, RuntimeDownload,
    RuntimeId, RuntimeIdentity, RuntimeIdentityError, RuntimeManifest, RuntimeManifestError,
    RuntimeOperationPhase, RuntimeOperationProgress, RuntimePackageAssetIdentity,
    RuntimePackageIdentity, RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
    RuntimeSelection, RuntimeSelectionSource, RuntimeSelections,
    RuntimeSourceBuildConfigurationError, RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites,
    RuntimeSourceBuildProvenance, RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem,
    RuntimeSourceBuildToolchain, RuntimeSourceSnapshot, RuntimeUpdatePreference,
    RuntimeUpdateState, effective_cmake_configuration_arguments, is_full_git_sha,
    is_safe_relative_path,
};
pub use settings::{
    GpuOffload, ResolvedSetting, ResolvedSettings, SETTINGS_STATE_VERSION, SettingCategory,
    SettingDefinition, SettingId, SettingKind, SettingScope, SettingSource, SettingValue,
    SettingsError, SettingsPatch, SettingsSchema, SettingsState, SettingsStore, StateStoreError,
    UnsignedIntegerOrChoiceValue, bounded_setting_file_sha256,
};
pub use state::{AppSnapshot, ApplicationCore, RegistryState, ServerState};
