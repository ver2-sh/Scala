//! Engine-neutral contracts for managed upstream inference runtimes.

use std::collections::{BTreeMap, btree_map::Entry};
use std::ffi::OsString;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::Stream;
use norted_core::{
    ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifactRole, AvailableRuntime,
    EngineInstallation, EngineRevision, HostCapabilities, InstalledRuntime, ModelArtifact, ModelId,
    ModelRuntimeIdentity, ResolvedSettings, RuntimeCompatibility, RuntimeId, RuntimeIdentity,
    RuntimeProbeObservation, SettingDefaultPreview, SettingDefaultSource, SettingDefinition,
    SettingId, SettingScope, SettingsError, SettingsPatch, SettingsSchema,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

pub mod benchmark;
mod catalog;
mod control;
mod installer;
mod manager;
mod packs;
mod prune;
mod store;
mod supervisor;
pub use prune::plan_runtime_prune;

pub use catalog::{
    CatalogError, GitHubCommit, GitHubCompare, GitHubComparisonStatus, GitHubRelease,
    GitHubReleaseAsset, GitHubReleaseClient, GitHubRepository, RuntimeCatalog, RuntimeCatalogEntry,
    RuntimeCatalogProvider, RuntimeCatalogSnapshot, RuntimeProviderAuthority, RuntimeProviderError,
    compatibility_for, compatibility_for_nvidia_device, detect_host_capabilities,
    is_exact_nvidia_gpu_uuid, isolated_cuda_environment, isolated_cuda_environment_for_binding,
    visible_nvidia_device_set, visible_nvidia_devices,
};

pub use control::{
    CONTROL_LOAD_PATH, CONTROL_STATUS_PATH, CONTROL_UNLOAD_PATH, ControlClient, ControlClientError,
    ControlErrorResponse, ControlLoadRequest, ControlUnloadRequest,
};
pub use manager::{
    BackendLifecycle, BackendLoadPhase, BackendLoadProgress, BackendParallelism, BackendResidency,
    BackendStatus, ControlStatus, EngineStatus, InferenceActivity, InferenceActivityPhase,
    InferenceRoutingContext, RuntimeError, RuntimeManager, RuntimeManagerOptions, RuntimeNotice,
    RuntimeNoticeLevel,
};
pub use packs::{
    InstalledRuntimeStatus, ModelProfileEngineSwitchCandidate, RuntimeListSnapshot,
    RuntimeLocalInspection, RuntimeModelCandidate, RuntimePackError, RuntimePackManager,
    RuntimeSearchResult, RuntimeSearchSnapshot, RuntimeUpdateCheck,
};
pub use store::{
    RuntimeLease, RuntimeStore, RuntimeStoreError, RuntimeStoreIssue, RuntimeStoreIssueKind,
    RuntimeStoreSnapshot,
};
pub use supervisor::{CapturedCommand, TokioProcessSupervisor, capture_command};

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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiCapability {
    Responses,
    ChatCompletions,
    Completions,
    Embeddings,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
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

/// Returns shared setting shapes instantiated with identities owned exclusively
/// by one adapter. The suffixes are reusable; every returned ID is qualified by
/// `engine_id` and cannot participate in another runtime's state.
pub fn common_setting_definitions_for(
    engine_id: &str,
    setting_ids: &[&str],
) -> Vec<SettingDefinition> {
    let library = common_setting_definition_library(engine_id);
    setting_ids
        .iter()
        .map(|id| {
            let suffix = id
                .strip_prefix(engine_id)
                .and_then(|value| value.strip_prefix('.'))
                .unwrap_or(id);
            let qualified = format!("{engine_id}.{suffix}");
            library
                .iter()
                .find(|definition| definition.id.as_str() == qualified)
                .unwrap_or_else(|| panic!("unknown common setting definition `{id}`"))
                .clone()
        })
        .collect()
}

fn common_setting_definition_library(engine_id: &str) -> Vec<SettingDefinition> {
    vec![
        SettingDefinition {
            id: qualified_setting_id(engine_id, "context_length"),
            label: "Context length".to_owned(),
            description: "Explicit context window requested from the selected runtime".to_owned(),
            kind: norted_core::SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::General,
            supported: true,
            unsupported_reason: None,
            unit: Some("tokens".to_owned()),
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "parallel_requests"),
            label: "Parallel requests".to_owned(),
            description: "Number of concurrent server slots".to_owned(),
            kind: norted_core::SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::General,
            supported: true,
            unsupported_reason: None,
            unit: Some("slots".to_owned()),
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "temperature"),
            label: "Temperature".to_owned(),
            description: "Configured generation temperature; request values may override it"
                .to_owned(),
            kind: norted_core::SettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::Generation,
            supported: true,
            unsupported_reason: None,
            unit: None,
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "top_p"),
            label: "Top P".to_owned(),
            description: "Configured nucleus-sampling probability".to_owned(),
            kind: norted_core::SettingKind::Float {
                minimum: Some(0.0),
                maximum: Some(1.0),
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::Generation,
            supported: true,
            unsupported_reason: None,
            unit: None,
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "top_k"),
            label: "Top K".to_owned(),
            description: "Configured top-k sampler cutoff".to_owned(),
            kind: norted_core::SettingKind::UnsignedInteger {
                minimum: Some(0),
                maximum: None,
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::Generation,
            supported: true,
            unsupported_reason: None,
            unit: None,
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "min_p"),
            label: "Min P".to_owned(),
            description: "Configured minimum-token probability threshold".to_owned(),
            kind: norted_core::SettingKind::Float {
                minimum: Some(0.0),
                maximum: Some(1.0),
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::Generation,
            supported: true,
            unsupported_reason: None,
            unit: None,
            default_preview: None,
        },
        SettingDefinition {
            id: qualified_setting_id(engine_id, "reasoning_effort"),
            label: "Reasoning effort".to_owned(),
            description: "Default reasoning effort when the selected engine supports it".to_owned(),
            kind: norted_core::SettingKind::Choice {
                choices: ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            scope: norted_core::SettingScope::Runtime {
                engine_id: engine_id.to_owned(),
            },
            category: norted_core::SettingCategory::Reasoning,
            supported: true,
            unsupported_reason: None,
            unit: None,
            default_preview: None,
        },
        common_definition(
            "seed",
            "Random seed",
            "Configured RNG seed, or `random` to request runtime randomness",
            norted_core::SettingKind::UnsignedIntegerOrChoice {
                minimum: Some(0),
                maximum: Some(u64::from(u32::MAX)),
                choices: vec!["random".to_owned()],
            },
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "repeat_penalty",
            "Repeat penalty",
            "Configured repetition penalty; request values may override it",
            norted_core::SettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "presence_penalty",
            "Presence penalty",
            "Configured token-presence penalty in the OpenAI-compatible -2..=2 range",
            norted_core::SettingKind::Float {
                minimum: Some(-2.0),
                maximum: Some(2.0),
            },
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "frequency_penalty",
            "Frequency penalty",
            "Configured token-frequency penalty in the OpenAI-compatible -2..=2 range",
            norted_core::SettingKind::Float {
                minimum: Some(-2.0),
                maximum: Some(2.0),
            },
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "max_output_tokens",
            "Default response length",
            "Default maximum generated tokens when a request omits its own limit",
            norted_core::SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: Some(u64::from(u32::MAX)),
            },
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "stop_strings",
            "Stop strings",
            "One or more configured generation stop strings as a JSON string array",
            norted_core::SettingKind::StringList,
            norted_core::SettingCategory::Prompt,
            engine_id,
        ),
        common_definition(
            "system_prompt",
            "System prompt",
            "Default system instructions used only when the request supplies no system or developer message",
            norted_core::SettingKind::String,
            norted_core::SettingCategory::Prompt,
            engine_id,
        ),
        common_definition(
            "reasoning",
            "Enable thinking",
            "Default reasoning/thinking mode: on, off, or template-driven auto",
            norted_core::SettingKind::Choice {
                choices: ["on", "off", "auto"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            norted_core::SettingCategory::Reasoning,
            engine_id,
        ),
        common_definition(
            "reasoning_budget",
            "Reasoning budget",
            "Thinking token budget: -1 is unrestricted, 0 ends immediately, positive values set a budget",
            norted_core::SettingKind::Integer {
                minimum: Some(-1),
                maximum: Some(i64::from(i32::MAX)),
            },
            norted_core::SettingCategory::Reasoning,
            engine_id,
        ),
        common_definition(
            "reasoning_budget_message",
            "Reasoning budget message",
            "Message injected before the end-of-thinking tag when the budget is exhausted",
            norted_core::SettingKind::String,
            norted_core::SettingCategory::Reasoning,
            engine_id,
        ),
        common_definition(
            "structured_output_schema",
            "Structured output schema",
            "Default per-request JSON Schema object used when the request omits its output format",
            norted_core::SettingKind::JsonObject,
            norted_core::SettingCategory::Generation,
            engine_id,
        ),
        common_definition(
            "context_overflow",
            "Context overflow",
            "Request context policy; truncate_middle uses exact model token counts and preserves instructions and the newest tail",
            norted_core::SettingKind::Choice {
                choices: ["error", "truncate_middle"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            norted_core::SettingCategory::Advanced,
            engine_id,
        ),
    ]
}

fn common_definition(
    id: &str,
    label: &str,
    description: &str,
    kind: norted_core::SettingKind,
    category: norted_core::SettingCategory,
    engine_id: &str,
) -> SettingDefinition {
    SettingDefinition {
        id: qualified_setting_id(engine_id, id),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: norted_core::SettingScope::Runtime {
            engine_id: engine_id.to_owned(),
        },
        category,
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: match id {
            "stop_strings" | "system_prompt" | "reasoning_budget_message" => Some(
                SettingDefaultPreview::new("None", SettingDefaultSource::Norted),
            ),
            "structured_output_schema" => Some(SettingDefaultPreview::new(
                "plain text",
                SettingDefaultSource::Norted,
            )),
            "context_overflow" => Some(SettingDefaultPreview::new(
                "error",
                SettingDefaultSource::Norted,
            )),
            _ => None,
        },
    }
}

fn qualified_setting_id(engine_id: &str, suffix: &str) -> SettingId {
    SettingId::new(format!("{engine_id}.{suffix}")).expect("static engine-qualified setting ID")
}

pub async fn record_local_file_setting_identity(
    patch: &mut SettingsPatch,
    path_setting: &str,
    sha256_setting: &str,
    path_base: &Path,
    maximum_bytes: u64,
) -> Result<(), SettingsError> {
    let path_id = SettingId::new(path_setting)?;
    let Some(norted_core::SettingValue::Path(path)) = patch.0.get(&path_id) else {
        return Ok(());
    };
    let observed =
        norted_core::bounded_setting_file_sha256(&path_id, path, path_base, maximum_bytes).await?;
    let sha256_id = SettingId::new(sha256_setting)?;
    if let Some(configured) = patch.0.get(&sha256_id) {
        match configured {
            norted_core::SettingValue::String(configured)
                if configured.eq_ignore_ascii_case(&observed) => {}
            norted_core::SettingValue::String(configured) => {
                return Err(SettingsError::InvalidValue {
                    setting_id: sha256_id,
                    value: configured.clone(),
                    reason: format!("does not match the selected file (observed {observed})"),
                });
            }
            configured => {
                return Err(SettingsError::InvalidValue {
                    setting_id: sha256_id,
                    value: configured.to_string(),
                    reason: "expected a SHA-256 string".to_owned(),
                });
            }
        }
    }
    patch.insert(sha256_id, norted_core::SettingValue::String(observed));
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PreparedAuxiliaryArtifact {
    pub role: AuxiliaryArtifactRole,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone)]
pub struct PreparedModelInput {
    pub primary: ModelArtifact,
    pub auxiliary: Vec<PreparedAuxiliaryArtifact>,
    pub primary_file_identity: Option<PreparedFileIdentity>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PreparedFileIdentity {
    pub size_bytes: u64,
    pub modified_unix_nanos: Option<u128>,
    pub filesystem_device: Option<u64>,
    pub filesystem_inode: Option<u64>,
}

/// Non-blocking sink for engine-neutral load progress. Reporters must return
/// promptly; the runtime manager uses a coalescing channel behind this API.
pub type LoadProgressReporter = Arc<dyn Fn(BackendLoadProgress) + Send + Sync>;

/// Exact, engine-neutral progress for one streaming inference request.
/// Adapters report only values directly observed from their runtime contract.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InferenceActivityUpdate {
    PromptProgress { current: u64, total: u64 },
    GeneratedTokens(u64),
}

/// Non-blocking sink for exact per-request inference progress.
pub type InferenceActivityReporter = Arc<dyn Fn(InferenceActivityUpdate) + Send + Sync>;

const HASH_BUFFER_SIZE: usize = 8 * 1024 * 1024;
const HASH_PROGRESS_BYTE_INTERVAL: u64 = 64 * 1024 * 1024;
const HASH_PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);

/// Prepares a manifest-bound model in place. Package files are hash-checked at
/// this explicit load boundary and again immediately before launch.
pub async fn prepare_norted_package_input(
    model: &ModelArtifact,
) -> Result<PreparedModelInput, EngineError> {
    prepare_norted_package_input_inner(model, None).await
}

/// Prepares and completely verifies a manifest-bound model while reporting
/// byte-accurate progress for each artifact being hashed.
pub async fn prepare_norted_package_input_with_progress(
    model: &ModelArtifact,
    progress: &LoadProgressReporter,
) -> Result<PreparedModelInput, EngineError> {
    prepare_norted_package_input_inner(model, Some(progress)).await
}

async fn prepare_norted_package_input_inner(
    model: &ModelArtifact,
    progress: Option<&LoadProgressReporter>,
) -> Result<PreparedModelInput, EngineError> {
    let Some(package) = &model.norted_package else {
        return Ok(PreparedModelInput {
            primary: model.clone(),
            auxiliary: Vec::new(),
            primary_file_identity: None,
        });
    };
    let canonical = canonical_regular_file(&model.path, "Norted package primary artifact").await?;
    if canonical != model.path {
        return Err(EngineError::InvalidConfiguration(
            "Norted package primary path changed after discovery".to_owned(),
        ));
    }
    let metadata = tokio::fs::metadata(&canonical).await.map_err(package_io)?;
    if metadata.len() != package.expected_primary_size {
        return Err(EngineError::InvalidConfiguration(format!(
            "Norted package primary size mismatch: expected {}, observed {}",
            package.expected_primary_size,
            metadata.len()
        )));
    }
    let primary_sha = hash_package_artifact(
        &canonical,
        "primary",
        "initial preparation",
        BackendLoadPhase::PreparingModel,
        "Verifying package SHA-256",
        progress,
    )
    .await
    .map_err(package_io)?;
    if primary_sha != package.expected_primary_sha256 {
        return Err(EngineError::InvalidConfiguration(
            "Norted package primary artifact SHA256 differs from its manifest".to_owned(),
        ));
    }
    let mut primary = model.clone();
    primary.path = canonical;
    primary.hash = Some(primary_sha);
    primary.created = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0);
    let mut auxiliary = Vec::new();
    for declared in &model.auxiliary_artifacts {
        let expected = declared.hash.as_deref().ok_or_else(|| {
            EngineError::InvalidConfiguration(format!(
                "Norted package auxiliary {} has no manifest SHA256",
                declared.path.display()
            ))
        })?;
        let path = canonical_regular_file(&declared.path, "Norted package auxiliary").await?;
        if path != declared.path {
            return Err(EngineError::InvalidConfiguration(
                "Norted package auxiliary path changed after discovery".to_owned(),
            ));
        }
        let metadata = tokio::fs::metadata(&path).await.map_err(package_io)?;
        if metadata.len() != declared.size_bytes {
            return Err(EngineError::InvalidConfiguration(format!(
                "Norted package auxiliary {} changed size after discovery",
                path.display()
            )));
        }
        let artifact_role = format!("auxiliary:{}", auxiliary_role_label(&declared.role));
        let message = format!(
            "Verifying package {} SHA-256",
            auxiliary_role_label(&declared.role)
        );
        let observed = hash_package_artifact(
            &path,
            &artifact_role,
            "initial preparation",
            BackendLoadPhase::PreparingModel,
            &message,
            progress,
        )
        .await
        .map_err(package_io)?;
        if observed != expected {
            return Err(EngineError::InvalidConfiguration(format!(
                "Norted package auxiliary {} SHA256 differs from its manifest",
                path.display()
            )));
        }
        auxiliary.push(PreparedAuxiliaryArtifact {
            role: declared.role.clone(),
            path,
            size_bytes: metadata.len(),
            content_sha256: observed,
        });
    }
    Ok(PreparedModelInput {
        primary,
        auxiliary,
        primary_file_identity: Some(file_identity(&metadata)),
    })
}

pub async fn revalidate_norted_package_before_launch(
    model: &PreparedModelInput,
) -> Result<(), EngineError> {
    revalidate_norted_package_before_launch_inner(model, None).await
}

/// Repeats the complete package verification at the final pre-launch boundary
/// while reporting byte-accurate progress. This is intentionally independent
/// from preparation and must never be replaced by cached metadata.
pub async fn revalidate_norted_package_before_launch_with_progress(
    model: &PreparedModelInput,
    progress: &LoadProgressReporter,
) -> Result<(), EngineError> {
    revalidate_norted_package_before_launch_inner(model, Some(progress)).await
}

async fn revalidate_norted_package_before_launch_inner(
    model: &PreparedModelInput,
    progress: Option<&LoadProgressReporter>,
) -> Result<(), EngineError> {
    let Some(package) = &model.primary.norted_package else {
        return Ok(());
    };
    let canonical =
        canonical_regular_file(&model.primary.path, "prepared Norted package primary").await?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(package_io)?;
    let observed_identity = file_identity(&metadata);
    let observed_sha = hash_package_artifact(
        &canonical,
        "primary",
        "final pre-launch revalidation",
        BackendLoadPhase::PreparingLaunch,
        "Revalidating package before launch",
        progress,
    )
    .await
    .map_err(package_io)?;
    if canonical != model.primary.path
        || observed_identity.size_bytes != package.expected_primary_size
        || model.primary_file_identity.as_ref() != Some(&observed_identity)
        || model.primary.hash.as_deref() != Some(package.expected_primary_sha256.as_str())
        || observed_sha != package.expected_primary_sha256
        || model.primary.hash.as_deref() != Some(observed_sha.as_str())
    {
        return Err(EngineError::InvalidConfiguration(
            "Norted package primary artifact changed between preparation and launch".to_owned(),
        ));
    }
    for auxiliary in &model.auxiliary {
        let path =
            canonical_regular_file(&auxiliary.path, "prepared Norted package auxiliary").await?;
        let metadata = tokio::fs::metadata(&path).await.map_err(package_io)?;
        let artifact_role = format!("auxiliary:{}", auxiliary_role_label(&auxiliary.role));
        let message = format!(
            "Revalidating package {} before launch",
            auxiliary_role_label(&auxiliary.role)
        );
        let observed = hash_package_artifact(
            &path,
            &artifact_role,
            "final pre-launch revalidation",
            BackendLoadPhase::PreparingLaunch,
            &message,
            progress,
        )
        .await
        .map_err(package_io)?;
        if path != auxiliary.path
            || metadata.len() != auxiliary.size_bytes
            || observed != auxiliary.content_sha256
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "Norted package auxiliary {} changed between preparation and launch",
                auxiliary.path.display()
            )));
        }
    }
    Ok(())
}

async fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf, EngineError> {
    let path = tokio::fs::canonicalize(path).await.map_err(package_io)?;
    if !tokio::fs::metadata(&path)
        .await
        .map_err(package_io)?
        .is_file()
    {
        return Err(EngineError::InvalidConfiguration(format!(
            "{label} is not a regular file"
        )));
    }
    Ok(path)
}

async fn hash_package_artifact(
    path: &Path,
    artifact_role: &str,
    verification: &str,
    phase: BackendLoadPhase,
    message: &str,
    progress: Option<&LoadProgressReporter>,
) -> std::io::Result<String> {
    let started = Instant::now();
    let path = path.to_path_buf();
    let progress = progress.cloned();
    let message = message.to_owned();
    let (digest, size_bytes) = hash_file_with_progress(&path, move |current, total, throughput| {
        if let Some(progress) = &progress {
            let message = throughput.map_or_else(
                || message.clone(),
                |bytes_per_second| {
                    format!("{message} · {}", format_hash_throughput(bytes_per_second))
                },
            );
            progress(BackendLoadProgress {
                phase,
                fraction: None,
                current: Some(current),
                total: Some(total),
                message: Some(message),
            });
        }
    })
    .await?;
    let elapsed = started.elapsed();
    let throughput = throughput_bytes_per_second(size_bytes, elapsed);
    tracing::info!(
        artifact_role,
        verification,
        size_bytes,
        elapsed_seconds = elapsed.as_secs_f64(),
        throughput_mib_per_second = throughput as f64 / (1024.0 * 1024.0),
        "completed Norted package SHA-256 verification"
    );
    Ok(digest)
}

async fn hash_file_with_progress<F>(path: &Path, progress: F) -> std::io::Result<(String, u64)>
where
    F: Fn(u64, u64, Option<u64>) + Send + 'static,
{
    // Dropping preparation (for example at a benchmark deadline) must also
    // stop the blocking verifier instead of hashing a multi-gigabyte artifact
    // after its owning load future has gone away. A cancelled hash never yields
    // an integrity identity.
    struct CancelOnDrop(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancelled));
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("could not open {} for hashing: {error}", path.display()),
            )
        })?;
        let total = file
            .metadata()
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not inspect {} for hashing: {error}", path.display()),
                )
            })?
            .len();
        let started = Instant::now();
        let mut last_reported_at = started;
        let mut last_reported_bytes = 0_u64;
        let mut current = 0_u64;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];
        progress(0, total, None);
        loop {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "artifact verification cancelled",
                ));
            }
            let read = file.read(&mut buffer).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not read {} while hashing: {error}", path.display()),
                )
            })?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            current = current.saturating_add(read as u64);
            let now = Instant::now();
            if current.saturating_sub(last_reported_bytes) >= HASH_PROGRESS_BYTE_INTERVAL
                || now.duration_since(last_reported_at) >= HASH_PROGRESS_TIME_INTERVAL
            {
                progress(
                    current.min(total),
                    total,
                    Some(throughput_bytes_per_second(current, started.elapsed())),
                );
                last_reported_bytes = current;
                last_reported_at = now;
            }
        }
        progress(
            current.min(total),
            total,
            Some(throughput_bytes_per_second(current, started.elapsed())),
        );
        Ok((format!("{:x}", digest.finalize()), current))
    })
    .await
    .map_err(|error| std::io::Error::other(format!("hashing task failed: {error}")))?
}

fn throughput_bytes_per_second(bytes: u64, elapsed: Duration) -> u64 {
    let nanos = elapsed.as_nanos().max(1);
    let bytes = u128::from(bytes);
    u64::try_from(bytes.saturating_mul(1_000_000_000) / nanos).unwrap_or(u64::MAX)
}

fn format_hash_throughput(bytes_per_second: u64) -> String {
    format!("{:.0} MiB/s", bytes_per_second as f64 / (1024.0 * 1024.0))
}

fn auxiliary_role_label(role: &AuxiliaryArtifactRole) -> &str {
    match role {
        AuxiliaryArtifactRole::Manifest => "manifest",
        AuxiliaryArtifactRole::Tokenizer => "tokenizer",
        AuxiliaryArtifactRole::Projector => "projector",
        AuxiliaryArtifactRole::Sharp => "Sharp template",
        AuxiliaryArtifactRole::Other(name) => name,
    }
}

fn package_io(error: std::io::Error) -> EngineError {
    EngineError::InvalidConfiguration(format!("could not validate Norted package file: {error}"))
}

fn file_identity(metadata: &std::fs::Metadata) -> PreparedFileIdentity {
    let modified_unix_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        PreparedFileIdentity {
            size_bytes: metadata.len(),
            modified_unix_nanos,
            filesystem_device: Some(metadata.dev()),
            filesystem_inode: Some(metadata.ino()),
        }
    }
    #[cfg(not(unix))]
    PreparedFileIdentity {
        size_bytes: metadata.len(),
        modified_unix_nanos,
        filesystem_device: None,
        filesystem_inode: None,
    }
}

impl PreparedModelInput {
    pub fn runtime_identity(&self) -> ModelRuntimeIdentity {
        ModelRuntimeIdentity {
            model_id: self.primary.id.clone(),
            artifact_path: self.primary.path.clone(),
            content_sha256: self.primary.hash.clone(),
            native_identity: self.primary.native_identity.clone(),
            auxiliary: self
                .auxiliary
                .iter()
                .map(|artifact| norted_core::AuxiliaryRuntimeIdentity {
                    role: artifact.role.clone(),
                    artifact_path: artifact.path.clone(),
                    size_bytes: artifact.size_bytes,
                    content_sha256: artifact.content_sha256.clone(),
                })
                .collect(),
            norted_package: self
                .primary
                .norted_package
                .clone()
                .map(|binding| norted_core::NortedPackageRuntimeIdentity { binding }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub model: PreparedModelInput,
    pub runtime: InstalledRuntime,
    pub accelerator_binding: Option<norted_core::AcceleratorBinding>,
    pub backend_address: SocketAddr,
    pub settings: ResolvedSettings,
    pub settings_schema: SettingsSchema,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, String>,
    pub environment_remove: Vec<OsString>,
    pub inherits_parent_environment: bool,
    pub working_directory: Option<PathBuf>,
    /// Adapter-created private files that the supervisor removes when the
    /// process exits. Adapters may remove them earlier after observation.
    pub temporary_files: Vec<PathBuf>,
    pub endpoint: Option<String>,
    pub normalized_settings: BTreeMap<String, serde_json::Value>,
    pub settings: ResolvedSettings,
    pub native_arguments: Vec<String>,
    pub installation: EngineInstallation,
    pub runtime: InstalledRuntime,
    pub model: PreparedModelInput,
    pub accelerator_binding: Option<norted_core::AcceleratorBinding>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StartupObservation {
    Ready(BTreeMap<String, serde_json::Value>),
    RetryContextCapacity {
        kv_mode: String,
        observed_context: u64,
        minimum_context: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveGenerationSettings {
    pub temperature: f64,
    pub top_p: f64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelServingCapabilities {
    pub model_id: ModelId,
    pub format: ArtifactFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<ArtifactNativeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<NortedPackageSummary>,
    pub compatible_engine_ids: Vec<String>,
    pub compatible_installed_runtime_ids: Vec<RuntimeId>,
    pub selected_runtime_id: Option<RuntimeId>,
    pub active: bool,
    pub text_input: bool,
    pub text_output: bool,
    pub responses: bool,
    pub chat_completions: bool,
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageSummary {
    pub kind: norted_core::NortedPackageKind,
    pub manifest_schema: String,
    pub manifest_version: u32,
    pub sharp_validated: bool,
    pub canonical_lineage_key_short: Option<String>,
    pub native_identity: Option<ArtifactNativeIdentity>,
}

fn norted_package_summary(model: &ModelArtifact) -> Option<NortedPackageSummary> {
    let package = model.norted_package.as_ref()?;
    Some(NortedPackageSummary {
        kind: package.kind,
        manifest_schema: package.manifest_schema.clone(),
        manifest_version: package.manifest_version,
        sharp_validated: package.sharp.is_some(),
        canonical_lineage_key_short: package
            .canonical_source_lineage_key
            .as_ref()
            .map(|key| key.chars().take(12).collect()),
        native_identity: model.native_identity.clone(),
    })
}

impl EffectiveGenerationSettings {
    pub fn merged(self, patch: &GenerationSettingsPatch) -> Self {
        Self {
            temperature: patch.temperature.unwrap_or(self.temperature),
            top_p: patch.top_p.unwrap_or(self.top_p),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDescriptor {
    pub supervisor_id: String,
    pub process_id: u32,
    pub engine: EngineRevision,
    pub runtime_id: norted_core::RuntimeId,
    pub runtime_version: String,
    pub runtime_variant: String,
    pub runtime_executable_sha256: String,
    pub model_id: norted_core::ModelId,
    pub endpoint: Option<String>,
    pub launched_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessExit {
    pub process_id: u32,
    pub success: bool,
    pub code: Option<i32>,
    pub expected: bool,
    pub detail: String,
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InferenceRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InferenceContentPart {
    Text { text: String },
    ImageUrl { url: String },
    VideoUrl { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded function arguments, matching the OpenAI tool-call wire contract.
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceMessage {
    pub role: InferenceRole,
    pub content: Vec<InferenceContentPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<InferenceToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl InferenceMessage {
    pub fn text(role: InferenceRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![InferenceContentPart::Text { text: text.into() }],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn text_only(&self) -> Option<String> {
        let mut text = String::new();
        for part in &self.content {
            match part {
                InferenceContentPart::Text { text: part } => text.push_str(part),
                InferenceContentPart::ImageUrl { .. } | InferenceContentPart::VideoUrl { .. } => {
                    return None;
                }
            }
        }
        Some(text)
    }

    pub fn has_media(&self) -> bool {
        self.content.iter().any(|part| {
            matches!(
                part,
                InferenceContentPart::ImageUrl { .. } | InferenceContentPart::VideoUrl { .. }
            )
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationSettingsPatch {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub min_p: Option<f64>,
    pub seed: Option<u64>,
    pub repeat_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub stop: Option<Vec<String>>,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_budget: Option<i64>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl GenerationSettingsPatch {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.min_p.is_none()
            && self.seed.is_none()
            && self.repeat_penalty.is_none()
            && self.presence_penalty.is_none()
            && self.frequency_penalty.is_none()
            && self.stop.is_none()
            && self.reasoning_enabled.is_none()
            && self.reasoning_budget.is_none()
            && self.reasoning_effort.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InferenceToolChoice {
    Auto,
    None,
    Required,
    Function { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OutputFormat {
    Text,
    JsonObject,
    JsonSchema {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        schema: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub model_profile_id: norted_core::ModelProfileId,
    pub messages: Vec<InferenceMessage>,
    pub generation_settings: GenerationSettingsPatch,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<InferenceTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<InferenceToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,
    pub max_output_tokens: Option<u32>,
    pub stream: bool,
}

/// Raw text, never rendered through a chat template.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model_profile_id: norted_core::ModelProfileId,
    pub prompt: String,
    pub generation_settings: GenerationSettingsPatch,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub model_profile_id: norted_core::ModelProfileId,
    pub input: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EmbeddingOutput {
    /// One numeric vector per input, in original input order.
    pub vectors: Vec<Vec<f32>>,
    pub prompt_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceOutput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<InferenceToolCall>,
    pub usage: Option<InferenceUsage>,
    pub finish_reason: InferenceFinishReason,
}

pub struct RoutedInferenceOutput {
    pub output: InferenceOutput,
    pub effective_generation_settings: EffectiveGenerationSettings,
    pub effective_output_format: Option<OutputFormat>,
}

pub struct RoutedInferenceStream {
    pub stream: InferenceStream,
    pub effective_generation_settings: EffectiveGenerationSettings,
    pub effective_output_format: Option<OutputFormat>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceFinishReason {
    Stop,
    MaxOutputTokens,
    ToolCalls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum InferenceEvent {
    TextDelta {
        delta: String,
    },
    ToolCallDelta {
        index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        arguments_delta: String,
    },
    Completed {
        usage: Option<InferenceUsage>,
        finish_reason: InferenceFinishReason,
    },
}

pub type InferenceStream =
    Pin<Box<dyn Stream<Item = Result<InferenceEvent, EngineError>> + Send + 'static>>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine operation is not implemented: {0}")]
    Unsupported(String),
    #[error("engine is not installed")]
    NotInstalled,
    #[error("engine operation failed: {0}")]
    Operation(String),
    #[error("engine backend is unavailable: {0}")]
    BackendUnavailable(String),
    #[error("engine operation timed out: {0}")]
    TimedOut(String),
    #[error("invalid engine configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid generation settings: {0}")]
    InvalidGenerationSettings(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("engine ID cannot be empty")]
    EmptyEngineId,
    #[error("engine ID `{0}` is already registered")]
    DuplicateEngineId(String),
}

/// Engine-owned logical variant identity used only for update discovery.
/// RuntimeIdentity and RuntimeId remain immutable package identities.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeVariantUpdateIdentity {
    pub functional_variant: String,
    pub source_recipe_generation: Option<u32>,
}

impl RuntimeVariantUpdateIdentity {
    pub fn exact(identity: &RuntimeIdentity) -> Self {
        Self {
            functional_variant: identity.variant.clone(),
            source_recipe_generation: None,
        }
    }
}

#[async_trait]
pub trait EngineAdapter: Send + Sync {
    fn identity(&self) -> EngineIdentity;
    fn capabilities(&self) -> EngineCapabilities;
    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    fn available_runtime_compatibility(
        &self,
        _runtime: &AvailableRuntime,
    ) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    fn runtime_compatibility(&self, _runtime: &InstalledRuntime) -> CompatibilityDecision {
        CompatibilityDecision::Supported
    }
    /// Separates an intentional functional variant from an explicitly ordered
    /// source-recipe generation without changing immutable runtime identity.
    fn runtime_variant_update_identity(
        &self,
        identity: &RuntimeIdentity,
    ) -> RuntimeVariantUpdateIdentity {
        RuntimeVariantUpdateIdentity::exact(identity)
    }
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
    /// Evaluates requirements that depend on this concrete runtime, model, and
    /// host. The shared resolver uses this for every model-specific path.
    fn runtime_model_compatibility(
        &self,
        _runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> RuntimeCompatibility {
        match self.compatibility(model) {
            CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
            CompatibilityDecision::Unsupported { reason } => {
                RuntimeCompatibility::Incompatible(reason)
            }
        }
    }
    /// Returns only features that this exact runtime/model/settings tuple can
    /// carry through Norted's public and private translations end to end.
    fn serving_features(
        &self,
        _runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _settings: Option<&ResolvedSettings>,
    ) -> Vec<EngineFeature> {
        self.capabilities().features
    }
    /// Whether a configured common setting should be copied into omitted
    /// per-request generation fields. Engines may instead own a setting as an
    /// upstream launch default while still accepting explicit request fields.
    fn uses_setting_as_request_default(&self, _id: &str) -> bool {
        true
    }
    /// Applies adapter-owned cross-setting normalization after the generic
    /// four-layer resolver has selected exact, engine-qualified values.
    fn normalize_settings(&self, settings: &mut ResolvedSettings) -> Result<(), EngineError> {
        if settings.engine_id == self.identity().id {
            Ok(())
        } else {
            Err(EngineError::InvalidConfiguration(format!(
                "resolved settings belong to `{}`, not `{}`",
                settings.engine_id,
                self.identity().id
            )))
        }
    }
    /// Validate configuration after selecting a runtime. This never influences runtime identity.
    fn validate_configuration(
        &self,
        _runtime: &InstalledRuntime,
        _model: Option<&ModelArtifact>,
        _host: &HostCapabilities,
        _settings: &ResolvedSettings,
    ) -> Result<(), EngineError> {
        Ok(())
    }
    /// Lower values are preferred after compatibility. Engines own the
    /// semantic ordering of their runtime variants.
    fn runtime_model_preference(
        &self,
        _runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> u16 {
        100
    }
    /// Evaluates a catalog runtime against a concrete model without installing
    /// it. Ordinary engines inherit the same coarse model gate as installed
    /// runtimes.
    fn available_runtime_model_compatibility(
        &self,
        _runtime: &AvailableRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> RuntimeCompatibility {
        match self.compatibility(model) {
            CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
            CompatibilityDecision::Unsupported { reason } => {
                RuntimeCompatibility::Incompatible(reason)
            }
        }
    }
    fn available_runtime_model_preference(
        &self,
        _runtime: &AvailableRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> u16 {
        100
    }
    /// Returns the exact ordered accelerator binding selected by the same
    /// policy used for model/runtime compatibility. `None` means the engine
    /// does not bind accelerators.
    fn runtime_model_accelerator_binding(
        &self,
        _runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> Option<norted_core::AcceleratorBinding> {
        None
    }
    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        Ok(PreparedModelInput {
            primary: model.clone(),
            auxiliary: Vec::new(),
            primary_file_identity: None,
        })
    }
    /// Progress-aware form used by the runtime manager. Adapters that perform
    /// measurable preparation should override this and keep the original
    /// method as a compatibility path for direct callers.
    async fn prepare_model_input_with_progress(
        &self,
        model: &ModelArtifact,
        _progress: LoadProgressReporter,
    ) -> Result<PreparedModelInput, EngineError> {
        self.prepare_model_input(model).await
    }
    fn native_options(&self) -> Vec<NativeOption>;
    /// Returns every stable setting this adapter understands, independent of
    /// whether one exact installed runtime currently supports it.
    fn setting_definitions(&self) -> Vec<SettingDefinition> {
        Vec::new()
    }
    /// Gates stable engine settings using facts proved by one concrete model.
    /// This boundary must not depend on an installed runtime.
    fn model_setting_definitions(
        &self,
        _model: &ModelArtifact,
    ) -> Result<Vec<SettingDefinition>, EngineError> {
        Ok(self.setting_definitions())
    }
    /// Applies one exact installed runtime's model-independent settings
    /// contract. This is used by engine-level settings clients that have no
    /// truthful model context to supply.
    async fn runtime_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
        Ok(SettingsSchema {
            engine_id: self.identity().id,
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions: self.setting_definitions(),
        })
    }
    /// Gates the curated semantic settings against one exact runtime contract.
    async fn settings_schema(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
        Ok(SettingsSchema {
            engine_id: self.identity().id,
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions: self.model_setting_definitions(model)?,
        })
    }
    /// Reports the legacy/flexible-entry runtime configured directly for this
    /// adapter. Managed packs are discovered by the shared runtime store.
    async fn probe(&self) -> Result<EngineProbe, EngineError>;
    /// Validates one exact runtime instance and records what the executable
    /// itself reported. Install activation and launch both use this boundary.
    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError>;
    /// Extracts exact native artifact identities from an already verified
    /// source snapshot when the upstream registry has a stable declarative
    /// representation. The default is truthful uncertainty.
    fn source_native_identities(
        &self,
        _source_root: &Path,
    ) -> Result<Vec<ArtifactNativeIdentity>, EngineError> {
        Ok(Vec::new())
    }
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError>;
    /// Returns the narrowly ordered process attempts for one launch. Most
    /// adapters have exactly one attempt. q27 automatic KV configuration may
    /// enumerate exact-runtime-proven fallback modes.
    async fn build_launch_attempts(
        &self,
        request: LaunchRequest,
    ) -> Result<Vec<LaunchSpec>, EngineError> {
        Ok(vec![self.build_launch_spec(request).await?])
    }
    /// Optionally describes adapter-owned work performed immediately before a
    /// launch attempt. The manager publishes this before calling
    /// `prepare_launch_attempt`, while process spawning has not started.
    fn prepare_launch_progress(&self, _spec: &LaunchSpec) -> Option<BackendLoadProgress> {
        None
    }
    /// Re-establishes the adapter-owned launch boundary immediately before an
    /// actual spawn. This is also where endpoint-scoped request routing state
    /// may be installed after clearing stale state.
    async fn prepare_launch_attempt(&self, _spec: &LaunchSpec) -> Result<(), EngineError> {
        Ok(())
    }
    /// Progress-aware final launch boundary. The default preserves adapters
    /// with no measurable preparation work.
    async fn prepare_launch_attempt_with_progress(
        &self,
        spec: &LaunchSpec,
        _progress: LoadProgressReporter,
    ) -> Result<(), EngineError> {
        self.prepare_launch_attempt(spec).await
    }
    /// Clears adapter-owned state after a spawn failure, terminated attempt,
    /// unload, or crash. Process supervision itself remains manager-owned.
    async fn clear_launch_state(&self, _endpoint: Option<&str>) {}
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError>;
    /// Converts bounded process startup output into facts that must be proven
    /// before a backend is promoted to healthy. Adapters should reject absent
    /// or contradictory observations when a model policy requires them.
    async fn startup_observation(
        &self,
        _process: &ProcessDescriptor,
        _stderr_tail: &[String],
    ) -> Result<StartupObservation, EngineError> {
        Ok(StartupObservation::Ready(BTreeMap::new()))
    }
    /// Interprets bounded startup output into an optional engine-neutral
    /// load-progress observation while the manager waits for readiness. This
    /// is UX evidence only, never an admission gate: adapters return `None`
    /// for unrecognized or missing output, and startup must not fail solely
    /// because progress is unavailable. Determinate values are allowed only
    /// when the exact runtime output contains a trustworthy measurement.
    fn startup_progress(&self, _stderr_tail: &[String]) -> Option<BackendLoadProgress> {
        None
    }
    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError>;
    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        _backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        if settings.is_empty() {
            Ok(())
        } else {
            Err(EngineError::InvalidGenerationSettings(format!(
                "engine `{}` does not support explicit request-time generation settings",
                self.identity().id
            )))
        }
    }
    fn validate_inference_request(
        &self,
        request: &InferenceRequest,
        backend_defaults: &EffectiveGenerationSettings,
        _settings_schema: &SettingsSchema,
    ) -> Result<(), EngineError> {
        self.validate_generation_settings(&request.generation_settings, backend_defaults)?;
        if !request.tools.is_empty()
            || request.tool_choice.is_some()
            || request.parallel_tool_calls.is_some()
            || request.messages.iter().any(|message| {
                message.role == InferenceRole::Tool
                    || !message.tool_calls.is_empty()
                    || message.tool_call_id.is_some()
            })
        {
            return Err(EngineError::InvalidGenerationSettings(format!(
                "engine `{}` does not support tool calling",
                self.identity().id
            )));
        }
        if request.messages.iter().any(InferenceMessage::has_media) {
            return Err(EngineError::InvalidGenerationSettings(format!(
                "engine `{}` does not support media input",
                self.identity().id
            )));
        }
        if matches!(
            request.output_format.as_ref(),
            Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
        ) {
            return Err(EngineError::InvalidGenerationSettings(format!(
                "engine `{}` does not support structured output",
                self.identity().id
            )));
        }
        Ok(())
    }
    /// Returns the exact effective per-request context capacity when the
    /// selected backend can prove it. `None` means the adapter has no exact
    /// tokenizer/context contract and context rewriting must not proceed.
    async fn context_capacity(&self, _endpoint: &str) -> Result<Option<u64>, EngineError> {
        Ok(None)
    }
    /// Counts the fully rendered request with the selected model tokenizer.
    async fn count_input_tokens(
        &self,
        _endpoint: &str,
        _request: &InferenceRequest,
    ) -> Result<Option<u64>, EngineError> {
        Ok(None)
    }
    /// Artifact-specific support, separate from format compatibility and provenance.
    fn supports_model_capability(&self, _model: &ModelArtifact, capability: ApiCapability) -> bool {
        self.capabilities().api.contains(&capability)
    }
    async fn complete(
        &self,
        _endpoint: &str,
        _request: CompletionRequest,
    ) -> Result<InferenceOutput, EngineError> {
        Err(EngineError::Unsupported("raw completions".to_owned()))
    }
    async fn complete_stream(
        &self,
        _endpoint: &str,
        _request: CompletionRequest,
        _activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        Err(EngineError::Unsupported(
            "streaming raw completions".to_owned(),
        ))
    }
    async fn embed(
        &self,
        _endpoint: &str,
        _request: EmbeddingRequest,
    ) -> Result<EmbeddingOutput, EngineError> {
        Err(EngineError::Unsupported("embeddings".to_owned()))
    }
    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError>;
    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError>;
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
    /// Cancel deadline-bound work through the same supervisor ownership. The
    /// native supervisor skips the graceful wait; other supervisors may retain
    /// their normal termination contract.
    async fn terminate_immediately(&self, process: &ProcessDescriptor) -> Result<(), EngineError> {
        self.terminate(process).await
    }
    async fn subscribe(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<watch::Receiver<Option<ProcessExit>>, EngineError>;
    async fn stderr_tail(&self, process: &ProcessDescriptor) -> Result<Vec<String>, EngineError>;
    async fn shutdown(&self);
}

#[derive(Clone, Default)]
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

    pub fn setting_definitions(&self) -> Result<Vec<SettingDefinition>, EngineError> {
        let mut definitions = Vec::<SettingDefinition>::new();
        for adapter in self.adapters.values() {
            let engine_id = adapter.identity().id;
            for definition in adapter.setting_definitions() {
                definition
                    .validate_scope()
                    .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
                if !matches!(
                    &definition.scope,
                    SettingScope::Runtime { engine_id: owner } if owner == &engine_id
                ) {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "setting `{}` is not owned by adapter `{engine_id}`",
                        definition.id
                    )));
                }
                if let Some(existing) = definitions.iter().find(|existing| {
                    existing.id == definition.id && existing.scope == definition.scope
                }) {
                    if existing != &definition {
                        return Err(EngineError::InvalidConfiguration(format!(
                            "setting `{}` has conflicting adapter definitions",
                            definition.id
                        )));
                    }
                } else {
                    definitions.push(definition);
                }
            }
        }
        definitions.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| format!("{:?}", left.scope).cmp(&format!("{:?}", right.scope)))
        });
        Ok(definitions)
    }

    pub fn parse_settings(&self, assignments: &[String]) -> Result<SettingsPatch, EngineError> {
        let definitions = self
            .setting_definitions()?
            .into_iter()
            .map(|definition| (definition.id.clone(), definition))
            .collect::<BTreeMap<_, _>>();
        let mut patch = SettingsPatch::default();
        for assignment in assignments {
            let (raw_id, raw_value) = assignment.split_once('=').ok_or_else(|| {
                EngineError::InvalidConfiguration(format!(
                    "setting `{assignment}` must use SETTING_ID=VALUE"
                ))
            })?;
            let id = SettingId::new(raw_id.to_owned())
                .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
            let definition = definitions.get(&id).ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    SettingsError::UnknownSetting(id.clone()).to_string(),
                )
            })?;
            let value = definition
                .parse(raw_value)
                .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
            patch.insert(id, value);
        }
        Ok(patch)
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
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use norted_core::{
        AppPaths, ArtifactFormat, AuxiliaryArtifactRole, AvailableRuntime, HostCapabilities,
        ModelArtifact, ModelId, ModelRegistry, RuntimeArchiveFormat, RuntimeCompatibility,
        RuntimeDigest, RuntimeDownload, RuntimeIdentity, RuntimePackageIdentity,
        RuntimeReleaseChannel, RuntimeRequirements, SettingId, SettingValue, SettingsPatch,
    };
    use sha2::{Digest, Sha256};

    use super::{
        BackendLifecycle, BackendLoadPhase, BackendLoadProgress, BackendStatus,
        CompatibilityDecision, EffectiveGenerationSettings, EngineAdapter, EngineCapabilities,
        EngineError, EngineIdentity, EngineProbe, EngineRegistry, GenerationSettingsPatch,
        InferenceRole, InstallationState, LaunchRequest, LaunchSpec, NativeOption,
        PreparedAuxiliaryArtifact, PreparedModelInput, ProcessDescriptor, RuntimeCatalogProvider,
        RuntimePackManager, UpdateState, hash_file_with_progress, prepare_norted_package_input,
        prepare_norted_package_input_with_progress, record_local_file_setting_identity,
        revalidate_norted_package_before_launch,
        revalidate_norted_package_before_launch_with_progress,
    };

    struct ArchitectureAdapter {
        id: &'static str,
        architecture: &'static str,
        format: ArtifactFormat,
    }

    #[test]
    fn effective_generation_settings_merge_without_changing_backend_defaults() {
        let backend_defaults = EffectiveGenerationSettings {
            temperature: 0.7,
            top_p: 0.9,
        };
        let override_temperature = GenerationSettingsPatch {
            temperature: Some(0.2),
            ..Default::default()
        };

        assert_eq!(
            backend_defaults.merged(&GenerationSettingsPatch::default()),
            backend_defaults
        );
        assert_eq!(
            backend_defaults.merged(&override_temperature),
            EffectiveGenerationSettings {
                temperature: 0.2,
                top_p: 0.9,
            }
        );
        assert_eq!(backend_defaults.temperature, 0.7);
        assert_eq!(backend_defaults.top_p, 0.9);
    }

    #[tokio::test]
    async fn local_file_setting_identity_is_recorded_and_mismatches_fail() {
        let temporary = tempfile::tempdir().expect("template fixture");
        let template = temporary.path().join("custom.jinja");
        std::fs::write(&template, b"template").expect("template");
        let path_id = SettingId::new("q27.template_path").expect("path setting ID");
        let sha_id = SettingId::new("q27.template_sha256").expect("SHA setting ID");
        let mut patch = SettingsPatch::default();
        patch.insert(path_id.clone(), SettingValue::Path(template));
        record_local_file_setting_identity(
            &mut patch,
            path_id.as_str(),
            sha_id.as_str(),
            temporary.path(),
            1024,
        )
        .await
        .expect("record template identity");
        assert_eq!(
            patch.0[&sha_id],
            SettingValue::String(format!("{:x}", Sha256::digest(b"template")))
        );

        patch.insert(sha_id, SettingValue::String("0".repeat(64)));
        assert!(
            record_local_file_setting_identity(
                &mut patch,
                path_id.as_str(),
                "q27.template_sha256",
                temporary.path(),
                1024,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn blocking_hash_matches_reference_for_empty_and_small_files() {
        let temporary = tempfile::tempdir().expect("hash fixture");
        for (name, bytes) in [("empty", &b""[..]), ("small", &b"norted hash fixture"[..])] {
            let path = temporary.path().join(name);
            std::fs::write(&path, bytes).expect("write hash fixture");
            let (observed, size) = hash_file_with_progress(&path, |_, _, _| {})
                .await
                .expect("hash fixture");
            assert_eq!(observed, format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(size, bytes.len() as u64);
        }
    }

    #[tokio::test]
    async fn blocking_hash_reports_monotonic_bounded_multibuffer_progress() {
        let temporary = tempfile::tempdir().expect("hash fixture");
        let path = temporary.path().join("multi-buffer.bin");
        let bytes = (0..(20 * 1024 * 1024 + 137))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut file = std::fs::File::create(&path).expect("create fixture");
        file.write_all(&bytes).expect("write fixture");
        drop(file);
        let observations = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&observations);
        let (observed, size) = hash_file_with_progress(&path, move |current, total, _| {
            captured
                .lock()
                .expect("progress observations")
                .push((current, total));
        })
        .await
        .expect("hash fixture");

        assert_eq!(observed, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(size, bytes.len() as u64);
        let observations = observations.lock().expect("progress observations");
        assert!(!observations.is_empty());
        assert!(
            observations.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "hash byte progress must be monotonic"
        );
        assert!(
            observations.iter().all(|(current, total)| current <= total),
            "hash byte progress must never exceed its total"
        );
        assert_eq!(observations.last().copied(), Some((size, size)));
    }

    #[tokio::test]
    async fn blocking_hash_propagates_open_failure() {
        let temporary = tempfile::tempdir().expect("hash fixture");
        let missing = temporary.path().join("missing.bin");
        let error = hash_file_with_progress(&missing, |_, _, _| {})
            .await
            .expect_err("missing hash input must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains("missing.bin"));
    }

    #[tokio::test]
    #[ignore = "manual local-file throughput comparison"]
    async fn manual_blocking_hash_benchmark() {
        let path = std::env::var_os("NORTED_HASH_BENCH_FILE")
            .map(PathBuf::from)
            .expect("set NORTED_HASH_BENCH_FILE");
        let started = std::time::Instant::now();
        let (_, size) = hash_file_with_progress(&path, |_, _, _| {})
            .await
            .expect("benchmark hash");
        let elapsed = started.elapsed();
        eprintln!(
            "hashed {size} bytes in {:.3}s ({:.1} MiB/s)",
            elapsed.as_secs_f64(),
            size as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0)
        );
    }

    #[test]
    fn canonical_inference_roles_serialize_with_openai_names() {
        for (role, expected) in [
            (InferenceRole::Developer, "developer"),
            (InferenceRole::System, "system"),
            (InferenceRole::User, "user"),
            (InferenceRole::Assistant, "assistant"),
        ] {
            assert_eq!(serde_json::to_value(role).expect("role"), expected);
        }
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
                artifact_formats: vec![self.format],
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
            Ok(EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "fixture adapter has no installed runtime".to_owned(),
            })
        }

        async fn probe_runtime(
            &self,
            _runtime: &norted_core::InstalledRuntime,
        ) -> Result<norted_core::RuntimeProbeObservation, EngineError> {
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

        async fn effective_generation_settings(
            &self,
            _process: &ProcessDescriptor,
        ) -> Result<super::EffectiveGenerationSettings, EngineError> {
            unreachable!()
        }

        async fn infer(
            &self,
            _endpoint: &str,
            _request: super::InferenceRequest,
        ) -> Result<super::InferenceOutput, EngineError> {
            unreachable!()
        }

        async fn infer_stream(
            &self,
            _endpoint: &str,
            _request: super::InferenceRequest,
            _activity: super::InferenceActivityReporter,
        ) -> Result<super::InferenceStream, EngineError> {
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
                format: ArtifactFormat::Gguf,
            }))
            .expect("register architecture-a adapter");
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "architecture-b",
                architecture: "architecture-b",
                format: ArtifactFormat::Gguf,
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
            native_identity: None,
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
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

    #[test]
    fn adapter_default_rejects_explicit_generation_settings() {
        let adapter = ArchitectureAdapter {
            id: "no-request-samplers",
            architecture: "fixture",
            format: ArtifactFormat::Gguf,
        };
        assert!(
            adapter
                .validate_generation_settings(
                    &GenerationSettingsPatch::default(),
                    &EffectiveGenerationSettings {
                        temperature: 0.0,
                        top_p: 1.0,
                    },
                )
                .is_ok()
        );
        assert!(matches!(
            adapter.validate_generation_settings(
                &GenerationSettingsPatch {
                    temperature: Some(0.5),
                    ..Default::default()
                },
                &EffectiveGenerationSettings {
                    temperature: 0.0,
                    top_p: 1.0,
                },
            ),
            Err(EngineError::InvalidGenerationSettings(_))
        ));
    }

    #[tokio::test]
    async fn model_serving_capabilities_are_registry_derived_and_allow_no_runtime() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        let paths = AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/model-profiles.json"),
            model_profiles_lock_file: root.join("data/.model-profiles.lock"),
        };
        paths.ensure_required().expect("application paths");

        let mut registry = EngineRegistry::default();
        registry
            .register(Arc::new(ArchitectureAdapter {
                id: "fixture-engine",
                architecture: "fixture-architecture",
                format: ArtifactFormat::Gguf,
            }))
            .expect("register fixture adapter");
        let providers = Vec::<Arc<dyn RuntimeCatalogProvider>>::new();
        let manager = RuntimePackManager::new(&paths, registry, providers).expect("pack manager");
        let model = ModelArtifact {
            id: ModelId("fixture-model".to_owned()),
            display_name: "Fixture model".to_owned(),
            path: root.join("model.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("fixture-architecture".to_owned()),
            context_length: None,
            provenance: None,
            native_identity: None,
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        };

        let capabilities = manager
            .model_serving_capabilities(&model, Some(&model.id))
            .await
            .expect("capabilities");
        assert_eq!(capabilities.model_id, model.id);
        assert_eq!(capabilities.compatible_engine_ids, ["fixture-engine"]);
        assert!(capabilities.compatible_installed_runtime_ids.is_empty());
        assert_eq!(capabilities.selected_runtime_id, None);
        assert!(capabilities.active);
        assert!(capabilities.text_input && capabilities.text_output);
        assert!(capabilities.responses && capabilities.chat_completions && capabilities.streaming);
        assert!(!capabilities.tools);
        assert!(!capabilities.vision);
        assert!(!capabilities.structured_output);
        serde_json::to_value(&capabilities).expect("serializable capabilities");
    }

    #[test]
    fn available_model_compatibility_defaults_preserve_other_engines_for_the_format() {
        let adapter = ArchitectureAdapter {
            id: "second-q27-engine",
            architecture: "qwen35",
            format: ArtifactFormat::Q27,
        };
        let model = ModelArtifact {
            id: ModelId("fixture-q27".to_owned()),
            display_name: "Fixture Q27".to_owned(),
            path: PathBuf::from("fixture.q27"),
            format: ArtifactFormat::Q27,
            size_bytes: 0,
            created: 0,
            hash: None,
            architecture: Some("qwen35".to_owned()),
            context_length: None,
            provenance: None,
            native_identity: None,
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        };
        let identity = RuntimeIdentity {
            engine_id: "second-q27-engine".to_owned(),
            package_family: "fixture".to_owned(),
            version: "1".to_owned(),
            upstream_revision: None,
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerator: "cpu".to_owned(),
            variant: "default".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture".to_owned(),
                repository: Some("fixture/repository".to_owned()),
                release_tag: Some("v1".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("runtime.zip".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        let runtime = AvailableRuntime {
            runtime_id: norted_core::RuntimeId::from_identity(&identity),
            identity,
            display_name: "Second Q27 engine".to_owned(),
            supported_formats: vec![ArtifactFormat::Q27],
            source_url: "https://github.com/fixture/repository/releases/tag/v1".to_owned(),
            published_at_unix: None,
            channels: vec![RuntimeReleaseChannel::Stable],
            prerelease: false,
            acquisition: norted_core::RuntimeAcquisitionPlan::ReleaseAsset {
                download: RuntimeDownload {
                    url: "https://github.com/fixture/repository/releases/download/v1/runtime.zip"
                        .to_owned(),
                    size_bytes: 1,
                    digest: Some(RuntimeDigest::sha256("a".repeat(64)).expect("digest")),
                    archive_format: RuntimeArchiveFormat::Zip,
                    entrypoint_names: vec!["server".to_owned()],
                },
                additional_downloads: Vec::new(),
            },
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
        };
        assert!(matches!(
            adapter.available_runtime_model_compatibility(
                &runtime,
                &model,
                &HostCapabilities::current_without_accelerator_probe(),
                None,
            ),
            RuntimeCompatibility::Compatible
        ));
    }

    #[test]
    fn prepared_auxiliary_identity_is_preserved_for_launch_provenance() {
        let prepared = PreparedModelInput {
            primary: ModelArtifact {
                id: ModelId("fixture".to_owned()),
                display_name: "Fixture".to_owned(),
                path: PathBuf::from("model.q27"),
                format: ArtifactFormat::Q27,
                size_bytes: 100,
                created: 0,
                hash: None,
                architecture: None,
                context_length: None,
                provenance: None,
                native_identity: None,
                auxiliary_artifacts: Vec::new(),
                norted_package: None,
            },
            auxiliary: vec![PreparedAuxiliaryArtifact {
                role: AuxiliaryArtifactRole::Tokenizer,
                path: PathBuf::from("model.tok"),
                size_bytes: 8,
                content_sha256: "a".repeat(64),
            }],
            primary_file_identity: None,
        };
        let identity = prepared.runtime_identity();
        assert_eq!(identity.auxiliary.len(), 1);
        assert_eq!(identity.auxiliary[0].role, AuxiliaryArtifactRole::Tokenizer);
        assert_eq!(identity.auxiliary[0].size_bytes, 8);
        assert_eq!(identity.auxiliary[0].content_sha256, "a".repeat(64));
    }

    #[tokio::test]
    async fn package_preparation_hashes_primary_in_place_and_rejects_tampering() {
        let temporary = tempfile::tempdir().expect("package preparation fixture");
        let primary = temporary.path().join("model.gguf");
        let bytes = b"package-primary";
        std::fs::write(&primary, bytes).expect("primary fixture");
        let digest = format!("{:x}", Sha256::digest(bytes));
        let manifest = serde_json::json!({
            "schema": 2,
            "build_key": "a".repeat(64),
            "outputs": {
                "model.gguf": {
                    "filename": "model.gguf",
                    "size": bytes.len(),
                    "sha256": digest,
                    "quant": "UD-Q6_K_XL"
                }
            }
        });
        let manifest_path = temporary.path().join("BUILD-MANIFEST.json");
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest JSON");
        std::fs::write(&manifest_path, &manifest_bytes).expect("manifest fixture");
        let registry = ModelRegistry::discover(&[temporary.path().to_path_buf()]);
        let artifact = registry.artifacts().first().expect("package artifact");
        let summary = super::norted_package_summary(artifact).expect("local package summary");
        assert_eq!(summary.kind, norted_core::NortedPackageKind::Gguf);
        assert_eq!(summary.manifest_version, 2);
        let preparation_progress = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&preparation_progress);
        let reporter: super::LoadProgressReporter = Arc::new(move |progress| {
            captured
                .lock()
                .expect("preparation progress")
                .push(progress);
        });
        let prepared = prepare_norted_package_input_with_progress(artifact, &reporter)
            .await
            .expect("prepared package");
        assert_eq!(prepared.primary.path, primary.canonicalize().unwrap());
        assert_eq!(prepared.primary.hash.as_deref(), Some(digest.as_str()));
        assert!(prepared.primary_file_identity.is_some());
        assert!(
            prepared
                .auxiliary
                .iter()
                .any(|artifact| artifact.role == AuxiliaryArtifactRole::Manifest),
            "package manifest remains a completely verified auxiliary artifact"
        );
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 2);
        let primary_preparation_completed = {
            let preparation_progress = preparation_progress.lock().expect("preparation progress");
            preparation_progress.iter().any(|progress| {
                progress.phase == BackendLoadPhase::PreparingModel
                    && progress.current == Some(bytes.len() as u64)
                    && progress.total == Some(bytes.len() as u64)
                    && progress
                        .message
                        .as_deref()
                        .is_some_and(|message| message.starts_with("Verifying package SHA-256"))
            })
        };
        assert!(primary_preparation_completed);

        let launch_progress = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&launch_progress);
        let reporter: super::LoadProgressReporter = Arc::new(move |progress| {
            captured.lock().expect("launch progress").push(progress);
        });
        revalidate_norted_package_before_launch_with_progress(&prepared, &reporter)
            .await
            .expect("unchanged package revalidation");
        assert!(
            launch_progress
                .lock()
                .expect("launch progress")
                .iter()
                .any(|progress| {
                    progress.phase == BackendLoadPhase::PreparingLaunch
                        && progress.current == Some(bytes.len() as u64)
                        && progress.total == Some(bytes.len() as u64)
                        && progress.message.as_deref().is_some_and(|message| {
                            message.starts_with("Revalidating package before launch")
                        })
                })
        );

        let mut changed_manifest = manifest_bytes.clone();
        changed_manifest.push(b' ');
        std::fs::write(&manifest_path, changed_manifest).expect("mutated manifest fixture");
        assert!(
            revalidate_norted_package_before_launch(&prepared)
                .await
                .is_err(),
            "final verification must fail closed for a changed auxiliary"
        );
        assert!(
            prepare_norted_package_input(artifact).await.is_err(),
            "initial verification must fail closed for a changed auxiliary"
        );
        std::fs::write(&manifest_path, &manifest_bytes).expect("restore manifest fixture");
        revalidate_norted_package_before_launch(&prepared)
            .await
            .expect("restored auxiliary remains valid");

        std::fs::write(&primary, b"PACKAGE-primary").expect("same-size tamper in place");
        assert_eq!(
            std::fs::metadata(&primary).unwrap().len(),
            bytes.len() as u64
        );
        assert!(
            revalidate_norted_package_before_launch(&prepared)
                .await
                .is_err()
        );
        assert!(prepare_norted_package_input(artifact).await.is_err());
    }

    #[test]
    fn backend_status_serializes_an_optional_load_progress_state() {
        let with_progress = BackendStatus {
            generation: 9,
            lifecycle: BackendLifecycle::Loading,
            model_profile_id: norted_core::ModelProfileId::new("qwen-quality").expect("profile ID"),
            model_id: ModelId("qwen3.8-27b".to_owned()),
            role: norted_core::ModelRole::Primary,
            residency: crate::BackendResidency::Jit,
            engine_id: Some("q27".to_owned()),
            runtime_id: None,
            runtime_version: Some("0.10.0".to_owned()),
            runtime_variant: Some("w12".to_owned()),
            runtime_executable_sha256: None,
            accelerator_binding: None,
            process_id: Some(4321),
            private_endpoint: Some("http://127.0.0.1:4321".to_owned()),
            load_progress: Some(BackendLoadProgress {
                phase: BackendLoadPhase::LoadingModel,
                fraction: Some(0.5),
                current: Some(148),
                total: Some(200),
                message: Some("Loading tensors".to_owned()),
            }),
            failure: None,
            provenance: None,
            parallel_requests: None,
            activities: Vec::new(),
            active_request_count: 0,
            primary_lease_count: 0,
            last_used_unix: 0,
            retiring: false,
        };
        let value = serde_json::to_value(&with_progress).expect("serialize");
        assert_eq!(value["lifecycle"], "loading");
        assert_eq!(value["load_progress"]["phase"], "loading_model");
        assert_eq!(value["load_progress"]["fraction"], 0.5);
        assert_eq!(value["load_progress"]["current"], 148);
        assert_eq!(value["load_progress"]["total"], 200);
        assert_eq!(value["load_progress"]["message"], "Loading tensors");

        let without_progress = BackendStatus {
            load_progress: None,
            ..with_progress
        };
        let value = serde_json::to_value(&without_progress).expect("serialize");
        assert!(
            value.get("load_progress").is_none(),
            "absent progress is skipped, not null"
        );
    }

    #[test]
    fn load_progress_sanitization_rejects_untrustworthy_numeric_evidence() {
        // Phase-only observations stay indeterminate.
        let phase_only =
            BackendLoadProgress::indeterminate(BackendLoadPhase::SpawningBackend).sanitized();
        assert_eq!(phase_only.fraction, None);
        assert_eq!(phase_only.current, None);
        assert_eq!(phase_only.total, None);

        // A trustworthy count derives a clamped fraction.
        let determinate = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: None,
            current: Some(50),
            total: Some(200),
            message: None,
        }
        .sanitized();
        assert_eq!(determinate.current, Some(50));
        assert_eq!(determinate.total, Some(200));
        assert_eq!(determinate.fraction, Some(0.25));

        // current > total is rejected, degrading to indeterminate.
        let inverted = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: Some(0.5),
            current: Some(300),
            total: Some(200),
            message: None,
        }
        .sanitized();
        assert_eq!(inverted.current, None);
        assert_eq!(inverted.total, None);
        assert_eq!(inverted.fraction, None);

        // A non-finite or out-of-range fraction is rejected.
        let nan = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: Some(f32::NAN),
            current: None,
            total: None,
            message: None,
        }
        .sanitized();
        assert_eq!(nan.fraction, None);

        let over_one = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: Some(1.5),
            current: None,
            total: None,
            message: None,
        }
        .sanitized();
        assert_eq!(over_one.fraction, None);

        // A lone current without total is rejected.
        let lone = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: None,
            current: Some(10),
            total: None,
            message: None,
        }
        .sanitized();
        assert_eq!(lone.current, None);
        assert_eq!(lone.total, None);
        assert_eq!(lone.fraction, None);

        // A zero total is rejected.
        let zero_total = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: None,
            current: Some(0),
            total: Some(0),
            message: None,
        }
        .sanitized();
        assert_eq!(zero_total.current, None);
        assert_eq!(zero_total.total, None);
        assert_eq!(zero_total.fraction, None);
    }
}
