use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifact, AuxiliaryArtifactRole,
    LoadSettingId, LoadSettingSource, LoadSettingValue, ModelArtifact, NinferArtifactIdentity,
    ResolvedLoadSetting, ResolvedLoadSettings, inspect_ninfer_container,
};

pub(crate) const MAX_PACKAGE_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HASHED_SIDECAR_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NortedPackageKind {
    Gguf,
    Q27,
    Ninfer,
}

impl std::fmt::Display for NortedPackageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gguf => formatter.write_str("Norted GGUF"),
            Self::Q27 => formatter.write_str("Norted q27"),
            Self::Ninfer => formatter.write_str("Norted NInfer"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NortedPackageStatus {
    Valid,
    NeedsRuntimeCapability { requirements: Vec<String> },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum NortedPackagePolicy {
    Gguf,
    Q27(Q27PackagePolicy),
    Ninfer(NinferPackagePolicy),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Q27PackagePolicy {
    pub profile: String,
    pub target: String,
    pub thinking_enabled: bool,
    pub default_reasoning_effort: String,
    pub unlimited_thinking_budget: bool,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u64,
    pub min_p: f64,
    pub maximum_mtp_depth: String,
    pub mtp_minimum_probability: f64,
    pub suffix_drafting: bool,
    pub suffix_width_from_runtime_w_max: bool,
    pub fast_head_default: bool,
    pub fast_head_override_allowed: bool,
    pub preferred_context_tokens: u64,
    pub minimum_context_tokens: u64,
    pub kv_preference: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NinferBenchmarkProfile {
    pub speculative_decoding: bool,
    pub backend: Option<String>,
    pub draft_tokens: Option<u64>,
    pub lm_head_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NinferPackagePolicy {
    pub policy_id: String,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u64,
    pub min_p: f64,
    pub thinking_enabled: bool,
    pub cuda_graph_decode: bool,
    pub compatible_prefix_reuse: bool,
    pub text_only_default: bool,
    pub kv_preference: String,
    pub minimum_context_tokens: u64,
    pub benchmark_profiles: BTreeMap<String, NinferBenchmarkProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageBinding {
    pub kind: NortedPackageKind,
    pub manifest_schema: String,
    pub manifest_version: u32,
    pub package_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_sha256: String,
    pub output_key: String,
    pub expected_primary_size: u64,
    pub expected_primary_sha256: String,
    pub build_key: Option<String>,
    pub master_id: Option<String>,
    pub quant_recipe_key: Option<String>,
    pub canonical_source_lineage_key: Option<String>,
    pub runtime_policy: Option<NortedPackageFile>,
    pub runtime_policy_id: Option<String>,
    pub runtime_policy_profile: Option<String>,
    pub sharp: Option<NortedPackageFile>,
    pub sharp_revision: Option<String>,
    pub sharp_version: Option<String>,
    pub tokenizer: Option<NortedPackageFile>,
    pub projector: Option<NortedPackageFile>,
    pub policy: NortedPackagePolicy,
    pub allowed_user_overrides: Vec<String>,
    pub status: NortedPackageStatus,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageMember {
    pub binding: NortedPackageBinding,
    pub auxiliary: Vec<AuxiliaryArtifact>,
    pub native_identity: Option<ArtifactNativeIdentity>,
}

#[derive(Debug, Clone)]
pub(crate) enum PackageDirectory {
    Valid {
        members: HashMap<PathBuf, PackageMember>,
        suppressed: HashSet<PathBuf>,
    },
    Invalid {
        reason: String,
        /// `None` means the manifest could not be read well enough to recover
        /// its claims, so every same-format artifact remains quarantined.
        claimed: Option<HashSet<PathBuf>>,
    },
}

pub fn apply_norted_package_load_policy(
    model: &ModelArtifact,
    settings: &mut ResolvedLoadSettings,
) -> Result<(), String> {
    let Some(package) = &model.norted_package else {
        return Ok(());
    };
    let policy_id = package
        .runtime_policy_id
        .clone()
        .or_else(|| package.runtime_policy_profile.clone())
        .unwrap_or_else(|| format!("{}-v{}", package.manifest_schema, package.manifest_version));
    let source = LoadSettingSource::NortedPackagePolicy { policy_id };
    match &package.policy {
        NortedPackagePolicy::Gguf => {}
        NortedPackagePolicy::Q27(policy) => {
            enforce_context(
                settings,
                policy.minimum_context_tokens,
                policy.preferred_context_tokens,
                &source,
            )?;
            if settings.value("q27.kv_fp16").is_some() {
                return Err(
                    "Norted q27 package KV mode is selected by its quality-order policy and cannot be forced to FP16"
                        .to_owned(),
                );
            }
            let fast_head = setting_id("q27.fast_head")?;
            match settings
                .effective
                .get(&fast_head)
                .map(|setting| &setting.value)
            {
                Some(LoadSettingValue::Toggle(true)) if policy.fast_head_override_allowed => {}
                Some(LoadSettingValue::Toggle(false)) => {}
                Some(_) => {
                    return Err(
                        "q27.fast_head has an invalid value for the package policy".to_owned()
                    );
                }
                None => {
                    settings.effective.insert(
                        fast_head,
                        ResolvedLoadSetting {
                            value: LoadSettingValue::Toggle(policy.fast_head_default),
                            source,
                        },
                    );
                }
            }
        }
        NortedPackagePolicy::Ninfer(policy) => {
            enforce_context(
                settings,
                policy.minimum_context_tokens,
                policy.minimum_context_tokens,
                &source,
            )?;
            reject_enabled_flag(
                settings,
                "ninfer.no_thinking",
                "NInfer package requires thinking enabled",
            )?;
            reject_enabled_flag(
                settings,
                "ninfer.no_cuda_graph",
                "NInfer package requires CUDA graph decode",
            )?;
            reject_enabled_flag(
                settings,
                "ninfer.no_prefix_reuse",
                "NInfer package requires compatible prefix reuse",
            )?;
            insert_default(
                settings,
                "ninfer.kv_dtype",
                LoadSettingValue::Choice(policy.kv_preference.clone()),
                &source,
            )?;
            let selected_profile = settings.value("ninfer.package_profile").cloned();
            if selected_profile.is_none()
                && [
                    "ninfer.speculative_backend",
                    "ninfer.draft_tokens",
                    "ninfer.lm_head_draft",
                ]
                .iter()
                .any(|id| settings.value(id).is_some())
            {
                return Err(
                    "Norted NInfer package speculation must be selected through ninfer.package_profile (mtp0 or mtp3); DFlash and ad-hoc speculative combinations are not declared by the Builder policy"
                        .to_owned(),
                );
            }
            if let Some(profile) = selected_profile {
                let LoadSettingValue::Choice(profile) = profile else {
                    return Err("ninfer.package_profile must be a choice".to_owned());
                };
                match profile.as_str() {
                    "mtp0" => {
                        settings
                            .effective
                            .remove(&setting_id("ninfer.speculative_backend")?);
                        settings
                            .effective
                            .remove(&setting_id("ninfer.draft_tokens")?);
                        settings
                            .effective
                            .remove(&setting_id("ninfer.lm_head_draft")?);
                    }
                    "mtp3" => {
                        insert_package_value(
                            settings,
                            "ninfer.speculative_backend",
                            LoadSettingValue::Choice("mtp".to_owned()),
                            &source,
                        )?;
                        insert_package_value(
                            settings,
                            "ninfer.draft_tokens",
                            LoadSettingValue::UnsignedInteger(3),
                            &source,
                        )?;
                        insert_package_value(
                            settings,
                            "ninfer.lm_head_draft",
                            LoadSettingValue::FlagEnabled,
                            &source,
                        )?;
                    }
                    other => {
                        return Err(format!(
                            "unsupported NInfer package benchmark profile `{other}`"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn enforce_context(
    settings: &mut ResolvedLoadSettings,
    minimum: u64,
    default: u64,
    source: &LoadSettingSource,
) -> Result<(), String> {
    match settings.value("context_length") {
        Some(LoadSettingValue::UnsignedInteger(value)) if *value >= minimum => Ok(()),
        Some(LoadSettingValue::UnsignedInteger(value)) => Err(format!(
            "Norted package requires at least {minimum} served tokens; requested {value}"
        )),
        Some(_) => Err("context_length has an invalid value for the Norted package".to_owned()),
        None => insert_package_value(
            settings,
            "context_length",
            LoadSettingValue::UnsignedInteger(default),
            source,
        ),
    }
}

fn reject_enabled_flag(
    settings: &ResolvedLoadSettings,
    id: &str,
    message: &str,
) -> Result<(), String> {
    if settings.value(id).is_some() {
        Err(message.to_owned())
    } else {
        Ok(())
    }
}

fn insert_default(
    settings: &mut ResolvedLoadSettings,
    id: &str,
    value: LoadSettingValue,
    source: &LoadSettingSource,
) -> Result<(), String> {
    let id = setting_id(id)?;
    settings
        .effective
        .entry(id)
        .or_insert_with(|| ResolvedLoadSetting {
            value,
            source: source.clone(),
        });
    Ok(())
}

fn insert_package_value(
    settings: &mut ResolvedLoadSettings,
    id: &str,
    value: LoadSettingValue,
    source: &LoadSettingSource,
) -> Result<(), String> {
    settings.effective.insert(
        setting_id(id)?,
        ResolvedLoadSetting {
            value,
            source: source.clone(),
        },
    );
    Ok(())
}

fn setting_id(value: &str) -> Result<LoadSettingId, String> {
    LoadSettingId::new(value).map_err(|error| error.to_string())
}

#[derive(Debug, Deserialize)]
struct FileRecord {
    filename: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Q27Manifest {
    source_lineage: SourceLineage,
    sharp: Q27Sharp,
    tokenizer: FileRecord,
    runtime_policy: ManifestPolicyRecord,
    outputs: BTreeMap<String, Q27Output>,
}

#[derive(Debug, Deserialize)]
struct SourceLineage {
    key: String,
}

#[derive(Debug, Deserialize)]
struct Q27Sharp {
    filename: String,
    template_sha256: String,
    resolved_commit: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct ManifestPolicyRecord {
    filename: String,
    sha256: String,
    #[serde(default)]
    policy_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Q27Output {
    filename: String,
    size: u64,
    sha256: String,
    source_lineage: SourceLineage,
    tokenizer_sha256: String,
}

#[derive(Debug, Deserialize)]
struct Q27RuntimePolicy {
    schema: u32,
    profile: String,
    targets: Vec<String>,
    sharp: Q27PolicySharp,
    reasoning: Q27Reasoning,
    sampling: Sampler,
    mtp: Q27Mtp,
    quality: Q27Quality,
    context: Q27Context,
    vision: Q27Vision,
}

#[derive(Debug, Deserialize)]
struct Q27PolicySharp {
    required: bool,
    sha256: String,
    application: String,
}

#[derive(Debug, Deserialize)]
struct Q27Reasoning {
    thinking_enabled: bool,
    default_effort: String,
    thinking_budget: Q27ThinkingBudget,
}

#[derive(Debug, Deserialize)]
struct Q27ThinkingBudget {
    policy: String,
}

#[derive(Debug, Deserialize)]
struct Sampler {
    temperature: f64,
    top_p: f64,
    top_k: u64,
    min_p: f64,
}

#[derive(Debug, Deserialize)]
struct Q27Mtp {
    enabled: bool,
    required: bool,
    runtime_depth_policy: String,
    adaptive: Q27Adaptive,
}

#[derive(Debug, Deserialize)]
struct Q27Adaptive {
    maximum_depth: String,
    confidence_gate: Q27Confidence,
    suffix_drafting: bool,
    suffix_width: Q27SuffixWidth,
}

#[derive(Debug, Deserialize)]
struct Q27Confidence {
    enabled: bool,
    minimum_probability: f64,
}

#[derive(Debug, Deserialize)]
struct Q27SuffixWidth {
    policy: String,
}

#[derive(Debug, Deserialize)]
struct Q27Quality {
    fast_head_default: bool,
    fast_head_allowed_only_by_explicit_override: bool,
}

#[derive(Debug, Deserialize)]
struct Q27Context {
    minimum_required_served_tokens: u64,
    preferred_served_tokens: u64,
    artifact_alone_proves_served_context: bool,
    kv_cache_resolution: Q27KvResolution,
}

#[derive(Debug, Deserialize)]
struct Q27KvResolution {
    quality_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Q27Vision {
    supported: bool,
}

#[derive(Debug, Deserialize)]
struct NinferManifest {
    schema: String,
    canonical_source_lineage_key: String,
    outputs: BTreeMap<String, NinferOutput>,
    sharp: NinferSharp,
    runtime_policy: ManifestPolicyRecord,
}

#[derive(Debug, Deserialize)]
struct NinferOutput {
    artifact: NinferArtifact,
    source_lineage: SourceLineage,
}

#[derive(Debug, Deserialize)]
struct NinferArtifact {
    filename: String,
    model_id: String,
    weights_id: String,
    container_version: u32,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct NinferSharp {
    filename: String,
    revision: String,
    version: String,
    size: u64,
    sha256: String,
    required_for_dirk_equivalence: bool,
}

#[derive(Debug, Deserialize)]
struct NinferRuntimePolicy {
    schema: String,
    schema_version: u32,
    policy_id: String,
    artifact_identities: Vec<NinferPolicyIdentity>,
    benchmark_profiles: BTreeMap<String, NinferRawBenchmark>,
    serving: NinferServing,
    context: NinferContext,
    sampler: Sampler,
    sharp: NinferPolicySharp,
}

#[derive(Debug, Deserialize)]
struct NinferPolicyIdentity {
    model_id: String,
    weights_id: String,
}

#[derive(Debug, Deserialize)]
struct NinferRawBenchmark {
    speculative_decoding: bool,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    draft_tokens: Option<u64>,
    #[serde(default)]
    optimized_proposal_head: Option<bool>,
    cli: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NinferServing {
    thinking_enabled: bool,
    cuda_graph_decode: bool,
    compatible_prefix_reuse: bool,
    vision_loaded: bool,
    kv_preference: String,
}

#[derive(Debug, Deserialize)]
struct NinferContext {
    hard_minimum_served_tokens: u64,
    artifact_size_is_capacity_evidence: bool,
}

#[derive(Debug, Deserialize)]
struct NinferPolicySharp {
    required_for_dirk_equivalence: bool,
    sha256: String,
    application: String,
}

#[derive(Debug, Deserialize)]
struct BuildManifest {
    build_key: String,
    #[serde(default)]
    master: Option<BuildMaster>,
    #[serde(default)]
    route: Option<BuildRoute>,
    #[serde(default)]
    lineage: Option<BuildLineage>,
    outputs: BTreeMap<String, BuildOutput>,
}

#[derive(Debug, Deserialize)]
struct BuildMaster {
    master_id: String,
}

#[derive(Debug, Deserialize)]
struct BuildRoute {
    #[serde(default)]
    quants: BTreeMap<String, BuildQuantRoute>,
}

#[derive(Debug, Deserialize)]
struct BuildQuantRoute {
    #[serde(default)]
    cache_source_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BuildLineage {
    #[serde(default)]
    quants: BTreeMap<String, BuildQuantLineage>,
}

#[derive(Debug, Deserialize)]
struct BuildQuantLineage {
    raw_quant_key: String,
    #[serde(default)]
    unsloth_quant_recipe_key: Option<String>,
    #[serde(default)]
    provider_recipe_key: Option<String>,
    #[serde(default)]
    effective_quant_source: Option<SourceLineage>,
}

#[derive(Debug, Deserialize)]
struct BuildOutput {
    filename: String,
    size: u64,
    sha256: String,
    #[serde(default)]
    quant: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    projector_key: Option<String>,
}

pub(crate) fn discover_package_directory(
    root: &Path,
    format: ArtifactFormat,
) -> Option<PackageDirectory> {
    let manifest_name = match format {
        ArtifactFormat::Gguf => "BUILD-MANIFEST.json",
        ArtifactFormat::Q27 => "Q27-MANIFEST.json",
        ArtifactFormat::Ninfer => "NINFER-MANIFEST.json",
    };
    let manifest = root.join(manifest_name);
    if !manifest.exists() {
        return None;
    }
    let result = match format {
        ArtifactFormat::Gguf => discover_gguf(root, &manifest),
        ArtifactFormat::Q27 => discover_q27(root, &manifest),
        ArtifactFormat::Ninfer => discover_ninfer(root, &manifest),
    };
    Some(result.unwrap_or_else(|reason| PackageDirectory::Invalid {
        reason,
        claimed: recover_manifest_claims(root, &manifest, format),
    }))
}

fn discover_q27(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (manifest_value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let observed_schema = manifest_value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "Q27-MANIFEST schema must be an integer".to_owned())?;
    if observed_schema != 3 {
        return Err(format!(
            "unsupported Q27-MANIFEST schema {observed_schema}; expected 3"
        ));
    }
    validate_lineage_value(
        &manifest_value["source_lineage"],
        "q27 package source lineage",
    )?;
    let manifest: Q27Manifest = serde_json::from_value(manifest_value.clone())
        .map_err(|error| format!("invalid schema-3 Q27-MANIFEST: {error}"))?;
    validate_sha(&manifest.source_lineage.key, "q27 source lineage key")?;
    let tokenizer = resolve_file(
        &root,
        &manifest.tokenizer.filename,
        Some(manifest.tokenizer.size),
        &manifest.tokenizer.sha256,
        true,
    )?;
    super::model::validate_q27_tokenizer_header(&tokenizer.path).map_err(|reason| {
        format!(
            "manifest-bound q27 tokenizer {} is invalid: {reason}",
            tokenizer.path.display()
        )
    })?;
    let sharp = resolve_file(
        &root,
        &manifest.sharp.filename,
        None,
        &manifest.sharp.template_sha256,
        true,
    )?;
    let runtime = resolve_file(
        &root,
        &manifest.runtime_policy.filename,
        None,
        &manifest.runtime_policy.sha256,
        true,
    )?;
    require_distinct_files(&[
        ("tokenizer", &tokenizer.path),
        ("Sharp", &sharp.path),
        ("runtime policy", &runtime.path),
    ])?;
    let (policy_value, observed_policy_sha): (serde_json::Value, _) = read_json(&runtime.path)?;
    if observed_policy_sha != runtime.sha256 {
        return Err("q27 runtime policy SHA256 changed while parsing".to_owned());
    }
    let policy: Q27RuntimePolicy = serde_json::from_value(policy_value)
        .map_err(|error| format!("invalid q27 runtime policy: {error}"))?;
    validate_q27_policy(&policy, &manifest, &sharp)?;
    let mut members = HashMap::new();
    let mut bound = HashSet::new();
    for (target, output) in &manifest.outputs {
        if !matches!(target.as_str(), "q6" | "q6k") {
            return Err(format!(
                "q27 manifest contains unsupported output target `{target}`"
            ));
        }
        if output.source_lineage.key != manifest.source_lineage.key
            || output.tokenizer_sha256 != tokenizer.sha256
        {
            return Err(format!(
                "q27 output `{target}` is not bound to package lineage/tokenizer"
            ));
        }
        validate_lineage_value(
            &manifest_value["outputs"][target]["source_lineage"],
            &format!("q27 output `{target}` source lineage"),
        )?;
        let primary = resolve_file(
            &root,
            &output.filename,
            Some(output.size),
            &output.sha256,
            false,
        )?;
        if [
            tokenizer.path.as_path(),
            sharp.path.as_path(),
            runtime.path.as_path(),
        ]
        .contains(&primary.path.as_path())
        {
            return Err(format!(
                "q27 output `{target}` is ambiguously also bound as a sidecar"
            ));
        }
        if !bound.insert(primary.path.clone()) {
            return Err(format!(
                "q27 manifest ambiguously binds {}",
                output.filename
            ));
        }
        let q27_policy = Q27PackagePolicy {
            profile: policy.profile.clone(),
            target: target.clone(),
            thinking_enabled: policy.reasoning.thinking_enabled,
            default_reasoning_effort: policy.reasoning.default_effort.clone(),
            unlimited_thinking_budget: policy.reasoning.thinking_budget.policy == "unlimited",
            temperature: policy.sampling.temperature,
            top_p: policy.sampling.top_p,
            top_k: policy.sampling.top_k,
            min_p: policy.sampling.min_p,
            maximum_mtp_depth: policy.mtp.adaptive.maximum_depth.clone(),
            mtp_minimum_probability: policy.mtp.adaptive.confidence_gate.minimum_probability,
            suffix_drafting: policy.mtp.adaptive.suffix_drafting,
            suffix_width_from_runtime_w_max: policy.mtp.adaptive.suffix_width.policy
                == "runtime-compiled-maximum",
            fast_head_default: policy.quality.fast_head_default,
            fast_head_override_allowed: policy.quality.fast_head_allowed_only_by_explicit_override,
            preferred_context_tokens: policy.context.preferred_served_tokens,
            minimum_context_tokens: policy.context.minimum_required_served_tokens,
            kv_preference: policy.context.kv_cache_resolution.quality_order.clone(),
        };
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Q27,
            manifest_schema: "norted.q27-manifest".to_owned(),
            manifest_version: 3,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: target.clone(),
            expected_primary_size: output.size,
            expected_primary_sha256: output.sha256.clone(),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: Some(manifest.source_lineage.key.clone()),
            runtime_policy: Some(runtime.clone()),
            runtime_policy_id: None,
            runtime_policy_profile: Some(policy.profile.clone()),
            sharp: Some(sharp.clone()),
            sharp_revision: Some(manifest.sharp.resolved_commit.clone()),
            sharp_version: Some(manifest.sharp.version.clone()),
            tokenizer: Some(tokenizer.clone()),
            projector: None,
            policy: NortedPackagePolicy::Q27(q27_policy),
            allowed_user_overrides: vec![
                "temperature".to_owned(),
                "top_p".to_owned(),
                "q27.fast_head".to_owned(),
                "reasoning_effort".to_owned(),
            ],
            status: NortedPackageStatus::Valid,
        };
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary: vec![
                    as_auxiliary(&tokenizer, AuxiliaryArtifactRole::Tokenizer),
                    as_auxiliary(&sharp, AuxiliaryArtifactRole::Sharp),
                    as_auxiliary(&runtime, AuxiliaryArtifactRole::RuntimePolicy),
                    as_auxiliary(&manifest_file, AuxiliaryArtifactRole::Manifest),
                ],
                native_identity: None,
            },
        );
    }
    if members.is_empty() {
        return Err("q27 manifest declares no outputs".to_owned());
    }
    Ok(PackageDirectory::Valid {
        members,
        suppressed: HashSet::new(),
    })
}

fn validate_q27_policy(
    policy: &Q27RuntimePolicy,
    manifest: &Q27Manifest,
    sharp: &NortedPackageFile,
) -> Result<(), String> {
    if policy.schema != 3 {
        return Err(format!(
            "unsupported q27 runtime policy schema {}; expected 3",
            policy.schema
        ));
    }
    let targets = manifest.outputs.keys().cloned().collect::<HashSet<_>>();
    if policy.targets.iter().cloned().collect::<HashSet<_>>() != targets
        || policy.profile != "norted-dirk-quality-reference"
        || !policy.sharp.required
        || policy.sharp.sha256 != sharp.sha256
        || policy.sharp.application != "runtime-render-before-tokenization"
        || !policy.reasoning.thinking_enabled
        || policy.reasoning.default_effort != "medium"
        || policy.reasoning.thinking_budget.policy != "unlimited"
        || policy.sampling.temperature != 1.0
        || policy.sampling.top_p != 0.95
        || policy.sampling.top_k != 20
        || policy.sampling.min_p != 0.05
        || !policy.mtp.enabled
        || !policy.mtp.required
        || policy.mtp.runtime_depth_policy != "adaptive"
        || policy.mtp.adaptive.maximum_depth != "auto7"
        || !policy.mtp.adaptive.confidence_gate.enabled
        || policy.mtp.adaptive.confidence_gate.minimum_probability != 0.5
        || !policy.mtp.adaptive.suffix_drafting
        || policy.mtp.adaptive.suffix_width.policy != "runtime-compiled-maximum"
        || policy.quality.fast_head_default
        || !policy.quality.fast_head_allowed_only_by_explicit_override
        || policy.context.minimum_required_served_tokens != 200000
        || policy.context.preferred_served_tokens != 262144
        || policy.context.artifact_alone_proves_served_context
        || policy.context.kv_cache_resolution.quality_order != ["fp8", "turbo5k", "turbo3"]
        || policy.vision.supported
    {
        return Err("q27 runtime policy does not match schema-3 Dirk quality semantics".to_owned());
    }
    Ok(())
}

fn discover_ninfer(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (manifest_value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let observed_schema = manifest_value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("missing");
    let observed_version = manifest_value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if observed_schema != "norted.ninfer-manifest" || observed_version != 3 {
        return Err(format!(
            "unsupported NINFER-MANIFEST schema {observed_schema} v{observed_version}; expected norted.ninfer-manifest v3"
        ));
    }
    let manifest: NinferManifest = serde_json::from_value(manifest_value.clone())
        .map_err(|error| format!("invalid NINFER-MANIFEST v3: {error}"))?;
    validate_sha(
        &manifest.canonical_source_lineage_key,
        "NInfer canonical source lineage key",
    )?;
    let sharp = resolve_file(
        &root,
        &manifest.sharp.filename,
        Some(manifest.sharp.size),
        &manifest.sharp.sha256,
        true,
    )?;
    let runtime = resolve_file(
        &root,
        &manifest.runtime_policy.filename,
        None,
        &manifest.runtime_policy.sha256,
        true,
    )?;
    require_distinct_files(&[("Sharp", &sharp.path), ("runtime policy", &runtime.path)])?;
    let (policy_value, policy_sha): (serde_json::Value, _) = read_json(&runtime.path)?;
    if policy_sha != runtime.sha256 {
        return Err("NInfer runtime policy SHA256 changed while parsing".to_owned());
    }
    let policy: NinferRuntimePolicy = serde_json::from_value(policy_value.clone())
        .map_err(|error| format!("invalid NInfer runtime policy: {error}"))?;
    if canonical_hash_without_key(&policy_value, "policy_id")? != policy.policy_id {
        return Err("NInfer runtime policy_id does not match its canonical content".to_owned());
    }
    validate_ninfer_policy(&policy, &manifest, &sharp)?;
    let benchmark_profiles = policy
        .benchmark_profiles
        .iter()
        .map(|(name, profile)| {
            (
                name.clone(),
                NinferBenchmarkProfile {
                    speculative_decoding: profile.speculative_decoding,
                    backend: profile.backend.clone(),
                    draft_tokens: profile.draft_tokens,
                    lm_head_draft: profile.optimized_proposal_head.unwrap_or(false),
                },
            )
        })
        .collect();
    let package_policy = NinferPackagePolicy {
        policy_id: policy.policy_id.clone(),
        temperature: policy.sampler.temperature,
        top_p: policy.sampler.top_p,
        top_k: policy.sampler.top_k,
        min_p: policy.sampler.min_p,
        thinking_enabled: policy.serving.thinking_enabled,
        cuda_graph_decode: policy.serving.cuda_graph_decode,
        compatible_prefix_reuse: policy.serving.compatible_prefix_reuse,
        text_only_default: !policy.serving.vision_loaded,
        kv_preference: policy.serving.kv_preference.clone(),
        minimum_context_tokens: policy.context.hard_minimum_served_tokens,
        benchmark_profiles,
    };
    let mut members = HashMap::new();
    let mut bound = HashSet::new();
    for (weights_key, output) in &manifest.outputs {
        if weights_key != &output.artifact.weights_id
            || output.source_lineage.key != manifest.canonical_source_lineage_key
        {
            return Err(format!(
                "NInfer output `{weights_key}` is not bound to its identity/lineage"
            ));
        }
        validate_lineage_value(
            &manifest_value["outputs"][weights_key]["source_lineage"],
            &format!("NInfer output `{weights_key}` source lineage"),
        )?;
        let primary = resolve_file(
            &root,
            &output.artifact.filename,
            Some(output.artifact.size),
            &output.artifact.sha256,
            false,
        )?;
        if [sharp.path.as_path(), runtime.path.as_path()].contains(&primary.path.as_path()) {
            return Err(format!(
                "NInfer output `{weights_key}` is ambiguously also bound as a sidecar"
            ));
        }
        if !bound.insert(primary.path.clone()) {
            return Err(format!(
                "NInfer manifest ambiguously binds {}",
                output.artifact.filename
            ));
        }
        let inspected = inspect_ninfer_container(&primary.path).map_err(|error| {
            format!(
                "NInfer package artifact {} was rejected: {error}",
                primary.path.display()
            )
        })?;
        let expected = NinferArtifactIdentity {
            container_version: output.artifact.container_version,
            model_id: output.artifact.model_id.clone(),
            weights_id: output.artifact.weights_id.clone(),
        };
        if inspected.identity != expected
            || !policy.artifact_identities.iter().any(|identity| {
                identity.model_id == expected.model_id && identity.weights_id == expected.weights_id
            })
        {
            return Err(format!(
                "NInfer native identity for {} disagrees with manifest/runtime policy",
                output.artifact.filename
            ));
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Ninfer,
            manifest_schema: manifest.schema.clone(),
            manifest_version: 3,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: weights_key.clone(),
            expected_primary_size: output.artifact.size,
            expected_primary_sha256: output.artifact.sha256.clone(),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: Some(manifest.canonical_source_lineage_key.clone()),
            runtime_policy: Some(runtime.clone()),
            runtime_policy_id: Some(policy.policy_id.clone()),
            runtime_policy_profile: None,
            sharp: Some(sharp.clone()),
            sharp_revision: Some(manifest.sharp.revision.clone()),
            sharp_version: Some(manifest.sharp.version.clone()),
            tokenizer: None,
            projector: None,
            policy: NortedPackagePolicy::Ninfer(package_policy.clone()),
            allowed_user_overrides: vec![
                "temperature".to_owned(),
                "top_p".to_owned(),
                "ninfer.package_profile".to_owned(),
            ],
            status: NortedPackageStatus::Valid,
        };
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary: vec![
                    as_auxiliary(&sharp, AuxiliaryArtifactRole::Sharp),
                    as_auxiliary(&runtime, AuxiliaryArtifactRole::RuntimePolicy),
                    as_auxiliary(&manifest_file, AuxiliaryArtifactRole::Manifest),
                ],
                native_identity: Some(ArtifactNativeIdentity::Ninfer(expected)),
            },
        );
    }
    if members.is_empty() {
        return Err("NInfer manifest declares no outputs".to_owned());
    }
    Ok(PackageDirectory::Valid {
        members,
        suppressed: HashSet::new(),
    })
}

fn validate_ninfer_policy(
    policy: &NinferRuntimePolicy,
    manifest: &NinferManifest,
    sharp: &NortedPackageFile,
) -> Result<(), String> {
    let mtp0 = policy.benchmark_profiles.get("mtp0");
    let mtp3 = policy.benchmark_profiles.get("mtp3");
    if policy.schema != "norted.ninfer-runtime"
        || policy.schema_version != 2
        || manifest.runtime_policy.policy_id.as_deref() != Some(policy.policy_id.as_str())
        || !manifest.sharp.required_for_dirk_equivalence
        || !policy.sharp.required_for_dirk_equivalence
        || policy.sharp.sha256 != sharp.sha256
        || policy.sharp.application != "runtime-sidecar"
        || !policy.serving.thinking_enabled
        || !policy.serving.cuda_graph_decode
        || !policy.serving.compatible_prefix_reuse
        || policy.serving.vision_loaded
        || policy.serving.kv_preference != "int8"
        || policy.context.hard_minimum_served_tokens != 200000
        || policy.context.artifact_size_is_capacity_evidence
        || policy.sampler.temperature != 1.0
        || policy.sampler.top_p != 0.95
        || policy.sampler.top_k != 20
        || policy.sampler.min_p != 0.05
        || mtp0.is_none_or(|p| p.speculative_decoding || !p.cli.is_empty())
        || mtp3.is_none_or(|p| {
            !p.speculative_decoding
                || p.backend.as_deref() != Some("mtp")
                || p.draft_tokens != Some(3)
                || p.optimized_proposal_head != Some(true)
                || p.cli != ["--spec", "mtp", "--draft-tokens", "3", "--lm-head-draft"]
        })
    {
        return Err(
            "NInfer runtime policy does not match schema-v2 serving/MTP semantics".to_owned(),
        );
    }
    Ok(())
}

fn discover_gguf(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (manifest_value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let observed_schema = manifest_value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "BUILD-MANIFEST schema must be an integer".to_owned())?;
    if !matches!(observed_schema, 2 | 3) {
        return Err(format!(
            "unsupported BUILD-MANIFEST schema {observed_schema}; expected 2 or 3"
        ));
    }
    let manifest: BuildManifest = serde_json::from_value(manifest_value.clone())
        .map_err(|error| format!("invalid BUILD-MANIFEST schema {observed_schema}: {error}"))?;
    validate_sha(&manifest.build_key, "GGUF build key")?;
    if let Some(master) = &manifest.master {
        validate_sha(&master.master_id, "GGUF master ID")?;
    }
    let mut records = Vec::new();
    let mut projector = None;
    let mut bound = HashSet::new();
    for (key, output) in &manifest.outputs {
        let file = resolve_file(
            &root,
            &output.filename,
            Some(output.size),
            &output.sha256,
            false,
        )?;
        if !bound.insert(file.path.clone()) {
            return Err(format!(
                "BUILD-MANIFEST ambiguously binds {}",
                output.filename
            ));
        }
        if output.format.as_deref() == Some("high-precision-projector")
            || output.projector_key.is_some()
        {
            if projector.replace(file).is_some() {
                return Err("BUILD-MANIFEST declares multiple projectors".to_owned());
            }
        } else if output.filename.to_ascii_lowercase().ends_with(".gguf") {
            records.push((key.clone(), output, file));
        }
    }
    if records.is_empty() {
        return Err("BUILD-MANIFEST declares no primary GGUF outputs".to_owned());
    }
    let mut members = HashMap::new();
    for (key, output, primary) in records {
        let quant_lineage = output
            .quant
            .as_ref()
            .and_then(|quant| manifest.lineage.as_ref()?.quants.get(quant));
        if let Some(lineage) = quant_lineage {
            validate_sha(&lineage.raw_quant_key, "GGUF raw quant key")?;
            if let Some(recipe) = lineage
                .unsloth_quant_recipe_key
                .as_ref()
                .or(lineage.provider_recipe_key.as_ref())
            {
                validate_sha(recipe, "GGUF quant recipe key")?;
            }
        }
        let lineage_key = if observed_schema == 3 {
            let quant = output
                .quant
                .as_ref()
                .ok_or_else(|| format!("schema-3 GGUF output `{key}` has no quant identity"))?;
            let lineage = quant_lineage.ok_or_else(|| {
                format!("schema-3 GGUF output `{key}` has no durable quant lineage")
            })?;
            let effective = lineage.effective_quant_source.as_ref().ok_or_else(|| {
                format!("schema-3 GGUF output `{key}` has no effective quant source")
            })?;
            validate_effective_quant_source(
                &manifest_value["lineage"]["quants"][quant]["effective_quant_source"],
                &format!("schema-3 GGUF output `{key}` effective quant source"),
            )?;
            Some(effective.key.clone())
        } else {
            None
        };
        if let Some(route_key) = output
            .quant
            .as_ref()
            .and_then(|quant| manifest.route.as_ref()?.quants.get(quant))
            .and_then(|route| route.cache_source_key.as_ref())
        {
            validate_sha(route_key, "GGUF route cache source key")?;
            if lineage_key.as_ref().is_some_and(|key| key != route_key) {
                return Err(format!(
                    "schema-3 GGUF output `{key}` route cache identity disagrees with durable lineage"
                ));
            }
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Gguf,
            manifest_schema: "norted.build-manifest".to_owned(),
            manifest_version: observed_schema as u32,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: output.quant.clone().unwrap_or(key),
            expected_primary_size: output.size,
            expected_primary_sha256: output.sha256.clone(),
            build_key: Some(manifest.build_key.clone()),
            master_id: manifest
                .master
                .as_ref()
                .map(|master| master.master_id.clone()),
            quant_recipe_key: quant_lineage.and_then(|lineage| {
                lineage
                    .unsloth_quant_recipe_key
                    .clone()
                    .or_else(|| lineage.provider_recipe_key.clone())
            }),
            canonical_source_lineage_key: lineage_key,
            runtime_policy: None,
            runtime_policy_id: None,
            runtime_policy_profile: None,
            sharp: None,
            sharp_revision: None,
            sharp_version: None,
            tokenizer: None,
            projector: projector.clone(),
            policy: NortedPackagePolicy::Gguf,
            allowed_user_overrides: Vec::new(),
            status: NortedPackageStatus::Valid,
        };
        let auxiliary = projector
            .as_ref()
            .map(|file| vec![as_auxiliary(file, AuxiliaryArtifactRole::Projector)])
            .unwrap_or_default();
        let mut auxiliary = auxiliary;
        auxiliary.push(as_auxiliary(
            &manifest_file,
            AuxiliaryArtifactRole::Manifest,
        ));
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary,
                native_identity: None,
            },
        );
    }
    let suppressed = projector.into_iter().map(|file| file.path).collect();
    Ok(PackageDirectory::Valid {
        members,
        suppressed,
    })
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(io_string)?;
    if !root.is_dir() {
        return Err(format!(
            "package root {} is not a directory",
            root.display()
        ));
    }
    Ok(root)
}

fn recover_manifest_claims(
    root: &Path,
    manifest_path: &Path,
    format: ArtifactFormat,
) -> Option<HashSet<PathBuf>> {
    let root = root.canonicalize().ok()?;
    let manifest = manifest_path.canonicalize().ok()?;
    if !manifest.starts_with(&root) {
        return None;
    }
    let (value, _): (serde_json::Value, String) = read_json(&manifest).ok()?;
    let outputs = value.get("outputs")?.as_object()?;
    let mut claims = HashSet::new();
    for output in outputs.values() {
        let filename = match format {
            ArtifactFormat::Ninfer => output.get("artifact")?.get("filename")?.as_str(),
            ArtifactFormat::Gguf | ArtifactFormat::Q27 => output.get("filename")?.as_str(),
        }?;
        let relative = Path::new(filename);
        if filename.is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return None;
        }
        let candidate = root.join(relative);
        match candidate.canonicalize() {
            Ok(path) if path.starts_with(&root) => {
                claims.insert(path);
            }
            Ok(_) => return None,
            Err(_) => {
                // A missing claimed file still makes the package invalid, but
                // cannot accidentally identify an unrelated existing model.
            }
        }
    }
    (!claims.is_empty()).then_some(claims)
}

fn canonical_manifest(root: &Path, manifest: &Path) -> Result<PathBuf, String> {
    let manifest = manifest.canonicalize().map_err(io_string)?;
    if !manifest.starts_with(root) {
        return Err("Norted package manifest escapes its package root".to_owned());
    }
    if !manifest.metadata().map_err(io_string)?.is_file() {
        return Err("Norted package manifest is not a regular file".to_owned());
    }
    Ok(manifest)
}

fn require_distinct_files(files: &[(&str, &PathBuf)]) -> Result<(), String> {
    let mut seen = HashMap::new();
    for (role, path) in files {
        if let Some(previous) = seen.insert((*path).clone(), *role) {
            return Err(format!(
                "package ambiguously binds {} as both {previous} and {role}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn resolve_file(
    root: &Path,
    relative: &str,
    expected_size: Option<u64>,
    sha256: &str,
    verify_hash: bool,
) -> Result<NortedPackageFile, String> {
    validate_sha(sha256, "package file SHA256")?;
    let relative_path = Path::new(relative);
    if relative.is_empty()
        || relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("unsafe package-relative path `{relative}`"));
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .map_err(|error| format!("package file `{relative}` could not be resolved: {error}"))?;
    if !path.starts_with(root) {
        return Err(format!("package file `{relative}` escapes package root"));
    }
    let metadata = path.metadata().map_err(io_string)?;
    if !metadata.is_file() {
        return Err(format!("package file `{relative}` is not a regular file"));
    }
    if expected_size.is_some_and(|size| size != metadata.len()) {
        return Err(format!(
            "package file `{relative}` size mismatch: expected {}, observed {}",
            expected_size.unwrap(),
            metadata.len()
        ));
    }
    if verify_hash {
        if metadata.len() > MAX_HASHED_SIDECAR_BYTES {
            return Err(format!(
                "package sidecar `{relative}` exceeds the {MAX_HASHED_SIDECAR_BYTES}-byte validation limit"
            ));
        }
        let observed = sha256_file(&path)?;
        if observed != sha256 {
            return Err(format!("package file `{relative}` SHA256 mismatch"));
        }
    }
    Ok(NortedPackageFile {
        path,
        size_bytes: metadata.len(),
        sha256: sha256.to_owned(),
    })
}

fn as_auxiliary(file: &NortedPackageFile, role: AuxiliaryArtifactRole) -> AuxiliaryArtifact {
    AuxiliaryArtifact {
        role,
        path: file.path.clone(),
        size_bytes: file.size_bytes,
        hash: Some(file.sha256.clone()),
    }
}

fn package_file_from_observed(path: &Path, sha256: &str) -> Result<NortedPackageFile, String> {
    Ok(NortedPackageFile {
        path: path.to_path_buf(),
        size_bytes: path.metadata().map_err(io_string)?.len(),
        sha256: sha256.to_owned(),
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<(T, String), String> {
    let metadata = path.metadata().map_err(io_string)?;
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a regular manifest/sidecar",
            path.display()
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_PACKAGE_JSON_BYTES {
        return Err(format!(
            "{} must be between 1 and {} bytes",
            path.display(),
            MAX_PACKAGE_JSON_BYTES
        ));
    }
    let mut file = File::open(path).map_err(io_string)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_PACKAGE_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_string)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("{} changed while being read", path.display()));
    }
    let sha = hex_digest(Sha256::digest(&bytes));
    serde_json::from_slice(&bytes)
        .map(|value| (value, sha))
        .map_err(|error| format!("invalid bounded JSON in {}: {error}", path.display()))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(io_string)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_string)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize()))
}

fn validate_sha(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not a lowercase SHA256"));
    }
    Ok(())
}

fn validate_lineage_value(value: &serde_json::Value, label: &str) -> Result<(), String> {
    let expected = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label} lacks a key"))?;
    validate_sha(expected, label)?;
    if canonical_hash_without_key(value, "key")? != expected {
        return Err(format!("{label} key does not match its canonical content"));
    }
    Ok(())
}

fn validate_effective_quant_source(value: &serde_json::Value, label: &str) -> Result<(), String> {
    let expected = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label} lacks a key"))?;
    validate_sha(expected, label)?;
    let key_material = value
        .get("key_material")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{label} lacks Builder key_material"))?;
    let observed = serde_json::to_vec(key_material)
        .map(|bytes| hex_digest(Sha256::digest(bytes)))
        .map_err(|error| format!("could not canonicalize {label}: {error}"))?;
    if observed != expected {
        return Err(format!("{label} key does not match Builder key_material"));
    }
    let repository = value.get("repository").and_then(serde_json::Value::as_str);
    let route = value.get("route").and_then(serde_json::Value::as_str);
    let artifact_key = value
        .get("artifact_identity")
        .and_then(|identity| identity.get("key"))
        .and_then(serde_json::Value::as_str);
    if key_material
        .get("repository")
        .and_then(serde_json::Value::as_str)
        != repository
        || key_material
            .get("route")
            .and_then(serde_json::Value::as_str)
            != route
        || key_material
            .get("artifact")
            .and_then(serde_json::Value::as_str)
            != artifact_key
    {
        return Err(format!(
            "{label} Builder key_material disagrees with its declared source identity"
        ));
    }
    if let Some(restoration) = value.get("restoration").filter(|value| !value.is_null()) {
        let restoration_key = restoration
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("{label} restoration lacks a key"))?;
        validate_sha(restoration_key, &format!("{label} restoration key"))?;
        if key_material
            .get("restoration")
            .and_then(serde_json::Value::as_str)
            != Some(restoration_key)
        {
            return Err(format!(
                "{label} Builder key_material disagrees with restoration identity"
            ));
        }
    } else if key_material.contains_key("restoration") {
        return Err(format!(
            "{label} Builder key_material invents a restoration identity"
        ));
    }
    Ok(())
}

fn canonical_hash_without_key(value: &serde_json::Value, key: &str) -> Result<String, String> {
    let mut material = value.clone();
    material
        .as_object_mut()
        .ok_or_else(|| "canonical identity material must be an object".to_owned())?
        .remove(key);
    serde_json::to_vec(&material)
        .map(|bytes| hex_digest(Sha256::digest(bytes)))
        .map_err(|error| format!("could not canonicalize package identity: {error}"))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn io_string(error: std::io::Error) -> String {
    error.to_string()
}
