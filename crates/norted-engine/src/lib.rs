//! Engine-neutral contracts for managed upstream inference runtimes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use norted_core::{ArtifactFormat, BuildProvenance, EngineRevision, ModelArtifact};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineIdentity {
    pub id: String,
    pub display_name: String,
    pub upstream_repository: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineCapabilities {
    pub artifact_formats: Vec<ArtifactFormat>,
    pub api: Vec<ApiCapability>,
    pub features: Vec<EngineFeature>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiCapability {
    Responses,
    ChatCompletions,
    Embeddings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineFeature {
    TextGeneration,
    ToolCalling,
    StructuredOutput,
    Vision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstallationState {
    NotInstalled,
    Installed {
        revision: EngineRevision,
        provenance: BuildProvenance,
    },
    Invalid {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum UpdateState {
    Unknown,
    Current,
    Available { revision: EngineRevision },
    CheckFailed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineProbe {
    pub installation: InstallationState,
    pub update: UpdateState,
    pub healthy: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeOption {
    pub name: String,
    pub description: String,
    pub value_kind: OptionValueKind,
    pub repeatable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionValueKind {
    Flag,
    String,
    Integer,
    Float,
    Path,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub model: ModelArtifact,
    pub normalized_options: BTreeMap<String, String>,
    pub native_arguments: Vec<String>,
    pub native_environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDescriptor {
    pub process_id: u32,
    pub engine: EngineRevision,
    pub model_id: norted_core::ModelId,
    pub endpoint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine operation is not implemented: {0}")]
    Unsupported(String),
    #[error("engine is not installed")]
    NotInstalled,
    #[error("engine operation failed: {0}")]
    Operation(String),
}

#[async_trait]
pub trait EngineAdapter: Send + Sync {
    fn identity(&self) -> EngineIdentity;
    fn capabilities(&self) -> EngineCapabilities;
    fn native_options(&self) -> Vec<NativeOption>;
    async fn probe(&self) -> Result<EngineProbe, EngineError>;
    async fn install(&self) -> Result<EngineRevision, EngineError>;
    async fn update(&self) -> Result<EngineRevision, EngineError>;
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError>;
    async fn start(&self, spec: LaunchSpec) -> Result<ProcessDescriptor, EngineError>;
    async fn stop(&self, process: &ProcessDescriptor) -> Result<(), EngineError>;
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError>;
}

#[derive(Default)]
pub struct EngineRegistry {
    adapters: Vec<Box<dyn EngineAdapter>>,
}

impl EngineRegistry {
    pub fn adapters(&self) -> &[Box<dyn EngineAdapter>] {
        &self.adapters
    }
}
