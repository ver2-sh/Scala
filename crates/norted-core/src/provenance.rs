use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AcceleratorBinding, ArtifactNativeIdentity, AuxiliaryArtifactRole, EffectiveSetting, EngineId,
    ModelId, ModelProfileId, ModelRole, RuntimeManifest, RuntimeSelectionSource, SettingId,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfileRuntimeIdentity {
    pub model_profile_id: ModelProfileId,
    pub display_name: String,
    pub content_sha256: String,
    pub bound_model_id: ModelId,
    pub bound_engine_id: EngineId,
    pub role: ModelRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineRevision {
    pub engine_id: String,
    pub version: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method", content = "detail")]
pub enum AcquisitionMethod {
    OfficialBinary,
    SourceBuild,
    /// A user-supplied binary that Norted did not install or manage.
    ExternalBinary,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainProvenance {
    pub compiler: Option<String>,
    pub compiler_version: Option<String>,
    pub toolchain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProvenance {
    pub build_options: BTreeMap<String, String>,
    pub toolchain: Option<ToolchainProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInstallation {
    pub engine: EngineRevision,
    pub source_repository: Option<String>,
    pub acquisition_method: AcquisitionMethod,
    pub binary_path: PathBuf,
    pub binary_sha256: Option<String>,
    pub build: Option<BuildProvenance>,
    pub platform: String,
    pub architecture: String,
    pub runtime_variant: Option<String>,
    pub acquired_at_unix: Option<i64>,
    pub observed_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRuntimeIdentity {
    pub model_id: ModelId,
    pub artifact_path: PathBuf,
    pub content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<ArtifactNativeIdentity>,
    #[serde(default)]
    pub auxiliary: Vec<AuxiliaryRuntimeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norted_package: Option<NortedPackageRuntimeIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NortedPackageRuntimeIdentity {
    pub binding: crate::NortedPackageBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxiliaryRuntimeIdentity {
    pub role: AuxiliaryArtifactRole,
    pub artifact_path: PathBuf,
    pub size_bytes: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum NativeArgumentProvenance {
    Value(String),
    Redacted {
        argument: String,
        value_sha256: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentVariableProvenance {
    pub name: String,
    pub inherited: bool,
    /// Optional hash of the value for comparison without retaining the secret.
    pub value_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub process_id: u32,
    pub process_start_identity: Option<String>,
}

/// Immutable snapshot of the structured Norted settings used for one
/// launch. Absent entries were deliberately left to the upstream runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsProvenance {
    #[serde(default)]
    pub effective: BTreeMap<SettingId, EffectiveSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProvenance {
    pub model: ModelRuntimeIdentity,
    pub runtime: RuntimeManifest,
    pub runtime_entrypoint: PathBuf,
    pub selection_source: RuntimeSelectionSource,
    pub accelerator_binding: Option<AcceleratorBinding>,
    pub installation: EngineInstallation,
    pub model_profile: ModelProfileRuntimeIdentity,
    #[serde(default)]
    pub settings: SettingsProvenance,
    /// Current adapter/runtime and generation facts. Trustworthy effective
    /// SettingId values live in the `resolved_settings` JSON object. This is
    /// intentionally distinct from structured configured-setting provenance.
    pub normalized_settings: BTreeMap<String, serde_json::Value>,
    pub native_arguments: Vec<NativeArgumentProvenance>,
    pub native_environment: Vec<EnvironmentVariableProvenance>,
    /// Whether the engine process also inherited the parent process environment.
    pub inherits_parent_environment: bool,
    pub process: ProcessIdentity,
    pub private_backend_endpoint: String,
    pub launched_at_unix: i64,
}
