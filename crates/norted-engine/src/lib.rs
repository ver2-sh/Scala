//! Engine-neutral contracts for managed upstream inference runtimes.

use std::collections::{BTreeMap, btree_map::Entry};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use norted_core::{ArtifactFormat, EngineInstallation, EngineRevision, ModelArtifact};
use serde::{Deserialize, Deserializer, Serialize};

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

impl EngineCapabilities {
    pub fn accepts(&self, format: ArtifactFormat) -> bool {
        self.artifact_formats.contains(&format)
    }
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

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum CompatibilityDecision {
    Supported,
    Unsupported { reason: String },
}

impl CompatibilityDecision {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ExactAcquisitionValue(String);

impl ExactAcquisitionValue {
    pub fn new(value: impl Into<String>) -> Result<Self, ExactAcquisitionValueError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(ExactAcquisitionValueError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ExactAcquisitionValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("exact acquisition version or revision cannot be empty")]
pub struct ExactAcquisitionValueError;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "selector")]
pub enum ExactAcquisitionTarget {
    Version {
        version: ExactAcquisitionValue,
    },
    Revision {
        revision: ExactAcquisitionValue,
    },
    VersionAndRevision {
        version: ExactAcquisitionValue,
        revision: ExactAcquisitionValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "target")]
pub enum AcquisitionTarget {
    Stable,
    Latest,
    Exact { selector: ExactAcquisitionTarget },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationContext {
    pub install_root: PathBuf,
    pub cache_root: PathBuf,
    pub platform: String,
    pub architecture: String,
    pub build_options: BTreeMap<String, String>,
    pub runtime_variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquisitionRequest {
    pub target: AcquisitionTarget,
    pub context: InstallationContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstallationState {
    NotInstalled,
    Installed {
        installation: Box<EngineInstallation>,
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

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("engine ID cannot be empty")]
    EmptyEngineId,
    #[error("engine ID `{0}` is already registered")]
    DuplicateEngineId(String),
}

#[async_trait]
pub trait EngineAdapter: Send + Sync {
    fn identity(&self) -> EngineIdentity;
    fn capabilities(&self) -> EngineCapabilities;
    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if self.capabilities().accepts(model.format) {
            CompatibilityDecision::Supported
        } else {
            CompatibilityDecision::Unsupported {
                reason: format!(
                    "artifact format `{}` is not supported by this engine",
                    model.format.as_str()
                ),
            }
        }
    }
    fn native_options(&self) -> Vec<NativeOption>;
    async fn probe(&self) -> Result<EngineProbe, EngineError>;
    async fn install(&self, request: AcquisitionRequest)
    -> Result<EngineInstallation, EngineError>;
    async fn update(&self, request: AcquisitionRequest) -> Result<EngineInstallation, EngineError>;
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError>;
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError>;
}

/// Common process mechanics live behind this boundary, not in engine adapters.
#[async_trait]
pub trait ProcessSupervisor: Send + Sync {
    async fn spawn(
        &self,
        spec: LaunchSpec,
        engine: EngineRevision,
        model_id: norted_core::ModelId,
    ) -> Result<ProcessDescriptor, EngineError>;
    async fn terminate(&self, process: &ProcessDescriptor) -> Result<(), EngineError>;
}

#[derive(Default)]
pub struct EngineRegistry {
    adapters: BTreeMap<String, Arc<dyn EngineAdapter>>,
}

impl EngineRegistry {
    pub fn register(&mut self, adapter: Arc<dyn EngineAdapter>) -> Result<(), RegistryError> {
        let id = adapter.identity().id;
        if id.trim().is_empty() {
            return Err(RegistryError::EmptyEngineId);
        }
        match self.adapters.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(adapter);
                Ok(())
            }
            Entry::Occupied(_) => Err(RegistryError::DuplicateEngineId(id)),
        }
    }

    pub fn get(&self, engine_id: &str) -> Option<Arc<dyn EngineAdapter>> {
        self.adapters.get(engine_id).cloned()
    }

    pub fn adapters(&self) -> impl Iterator<Item = &Arc<dyn EngineAdapter>> {
        self.adapters.values()
    }

    pub fn compatible_with(&self, artifact: &ModelArtifact) -> Vec<Arc<dyn EngineAdapter>> {
        self.adapters
            .values()
            .filter(|adapter| adapter.compatibility(artifact).is_supported())
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;
    use norted_core::{ArtifactFormat, ModelArtifact, ModelId};

    use super::{
        AcquisitionRequest, CompatibilityDecision, EngineAdapter, EngineCapabilities, EngineError,
        EngineIdentity, EngineInstallation, EngineProbe, EngineRegistry, ExactAcquisitionTarget,
        ExactAcquisitionValue, LaunchRequest, LaunchSpec, NativeOption, ProcessDescriptor,
    };

    #[test]
    fn exact_acquisition_values_are_non_empty_and_normalized() {
        assert!(ExactAcquisitionValue::new("").is_err());
        assert!(ExactAcquisitionValue::new(" \t\r\n ").is_err());
        assert_eq!(
            ExactAcquisitionValue::new("  v1.2.3  ")
                .expect("non-empty exact value")
                .as_str(),
            "v1.2.3"
        );
    }

    #[test]
    fn exact_acquisition_serde_rejects_empty_values_and_supports_all_selectors() {
        for invalid in [
            r#"{"selector":"version","version":""}"#,
            r#"{"selector":"revision","revision":"   "}"#,
            r#"{"selector":"version_and_revision","version":"1.0","revision":"\t"}"#,
        ] {
            assert!(serde_json::from_str::<ExactAcquisitionTarget>(invalid).is_err());
        }

        let version = serde_json::from_str::<ExactAcquisitionTarget>(
            r#"{"selector":"version","version":" 1.0 "}"#,
        )
        .expect("version selector");
        let revision = serde_json::from_str::<ExactAcquisitionTarget>(
            r#"{"selector":"revision","revision":" abc123 "}"#,
        )
        .expect("revision selector");
        let both = serde_json::from_str::<ExactAcquisitionTarget>(
            r#"{"selector":"version_and_revision","version":" 1.0 ","revision":" abc123 "}"#,
        )
        .expect("version and revision selector");

        assert!(matches!(version, ExactAcquisitionTarget::Version { .. }));
        assert!(matches!(revision, ExactAcquisitionTarget::Revision { .. }));
        assert!(matches!(
            both,
            ExactAcquisitionTarget::VersionAndRevision { .. }
        ));
        assert_eq!(
            serde_json::to_value(version).expect("serialize normalized selector")["version"],
            "1.0"
        );
    }

    struct ArchitectureAdapter {
        id: &'static str,
        architecture: &'static str,
    }

    #[async_trait]
    impl EngineAdapter for ArchitectureAdapter {
        fn identity(&self) -> EngineIdentity {
            EngineIdentity {
                id: self.id.to_owned(),
                display_name: self.id.to_owned(),
                upstream_repository: String::new(),
            }
        }

        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities {
                artifact_formats: vec![ArtifactFormat::Gguf],
                ..EngineCapabilities::default()
            }
        }

        fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
            let coarse = if self.capabilities().accepts(model.format) {
                CompatibilityDecision::Supported
            } else {
                CompatibilityDecision::Unsupported {
                    reason: "format is unsupported".to_owned(),
                }
            };
            if !coarse.is_supported() {
                return coarse;
            }
            match model.architecture.as_deref() {
                Some(architecture) if architecture == self.architecture => {
                    CompatibilityDecision::Supported
                }
                architecture => CompatibilityDecision::Unsupported {
                    reason: format!(
                        "requires architecture `{}`, found `{}`",
                        self.architecture,
                        architecture.unwrap_or("unknown")
                    ),
                },
            }
        }

        fn native_options(&self) -> Vec<NativeOption> {
            Vec::new()
        }

        async fn probe(&self) -> Result<EngineProbe, EngineError> {
            unreachable!()
        }

        async fn install(
            &self,
            _request: AcquisitionRequest,
        ) -> Result<EngineInstallation, EngineError> {
            unreachable!()
        }

        async fn update(
            &self,
            _request: AcquisitionRequest,
        ) -> Result<EngineInstallation, EngineError> {
            unreachable!()
        }

        async fn build_launch_spec(
            &self,
            _request: LaunchRequest,
        ) -> Result<LaunchSpec, EngineError> {
            unreachable!()
        }

        async fn health(&self, _process: &ProcessDescriptor) -> Result<bool, EngineError> {
            unreachable!()
        }
    }

    #[test]
    fn registry_uses_adapter_model_compatibility_after_the_format_gate() {
        let mut registry = EngineRegistry::default();
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-a",
                architecture: "architecture-a",
            }))
            .expect("register architecture-a adapter");
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-b",
                architecture: "architecture-b",
            }))
            .expect("register architecture-b adapter");

        let model = ModelArtifact {
            id: ModelId("fixture".to_owned()),
            display_name: "Fixture".to_owned(),
            path: PathBuf::from("fixture.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("architecture-b".to_owned()),
            context_length: None,
            provenance: None,
        };
        let compatible = registry.compatible_with(&model);

        assert_eq!(compatible.len(), 1);
        assert_eq!(compatible[0].identity().id, "architecture-b");
        assert_eq!(
            registry
                .get("architecture-a")
                .expect("architecture-a adapter")
                .compatibility(&model),
            CompatibilityDecision::Unsupported {
                reason: "requires architecture `architecture-a`, found `architecture-b`".to_owned()
            }
        );
    }
}
