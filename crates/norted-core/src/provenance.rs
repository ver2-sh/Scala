use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ModelId;

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
    pub installed_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRuntimeIdentity {
    pub model_id: ModelId,
    pub artifact_path: PathBuf,
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum NativeArgumentProvenance {
    Value(String),
    Redacted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentVariableProvenance {
    pub name: String,
    /// Optional hash of the value for comparison without retaining the secret.
    pub value_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub process_id: u32,
    pub process_start_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProvenance {
    pub model: ModelRuntimeIdentity,
    pub engine: EngineRevision,
    pub engine_binary_sha256: Option<String>,
    pub profile: Option<String>,
    pub normalized_settings: BTreeMap<String, serde_json::Value>,
    pub native_arguments: Vec<NativeArgumentProvenance>,
    pub native_environment: Vec<EnvironmentVariableProvenance>,
    pub process: ProcessIdentity,
    pub launched_at_unix: i64,
}
