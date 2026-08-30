//! NInfer source runtime catalog, process adapter, and private protocol bridge.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use norted_core::{
    AcceleratorDevice, AcquisitionMethod, ArtifactFormat, ArtifactNativeIdentity, AvailableRuntime,
    EngineConfig, EngineInstallation, EngineRevision, HostCapabilities, InstalledRuntime,
    LoadSettingValue, ModelArtifact, ModelId, NinferArtifactIdentity, NinferPackagePolicy,
    NortedPackageBinding, NortedPackagePolicy, ResolvedLoadSettings, RuntimeAcquisitionMethod,
    RuntimeCompatibility, RuntimeId, RuntimeProbeObservation, RuntimeRequirements,
    inspect_ninfer_container,
};
use norted_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, InferenceOutput, InferenceRequest,
    InferenceStream, InstallationState, LaunchRequest, LaunchSpec, LoadProgressReporter,
    NativeOption, OptionValueKind, PreparedModelInput, ProcessDescriptor, UpdateState,
    capture_command, compatibility_for, isolated_cuda_environment, prepare_norted_package_input,
    prepare_norted_package_input_with_progress, revalidate_norted_package_before_launch,
    revalidate_norted_package_before_launch_with_progress, visible_nvidia_devices,
};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;

mod catalog;
mod protocol;
mod settings;

pub use catalog::NinferRuntimeCatalogProvider;

pub const ENGINE_ID: &str = "ninfer";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/Neroued/ninfer";
pub const GITHUB_REPOSITORY: &str = "Neroued/ninfer";
pub const PROVIDER_ID: &str = "ninfer-official-source";
const PACKAGE_CAPABILITY_REVISION: &str = "6b94b8c5721f075624c4f36d18279a848ba8b6c9";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const STARTUP_LOG_LIMIT: u64 = 512 * 1024;
const STARTUP_LOG_LINE_LIMIT: usize = 128 * 1024;

const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &["CUDA_VISIBLE_DEVICES"];

// Norted owns identity, private transport, device selection, structured load
// settings, request semantics, and all sampler behavior.
const MANAGED_NATIVE_ARGUMENTS: &[&str] = &[
    "--host",
    "--port",
    "--api-key",
    "--model-id",
    "--max-context",
    "--kv-capacity",
    "--max-concurrency",
    "--prefill-chunk",
    "--device",
    "--request-log-jsonl",
    "--kv-dtype",
    "--spec",
    "--draft-tokens",
    "--lm-head-draft",
    "--no-cuda-graph",
    "--no-prefix-reuse",
    "--no-thinking",
    "--preserve-thinking",
    "--device-state-slots",
    "--host-state-slots",
    "--host-kv-mib",
    "--max-private-continuations",
    "--max-shared-prefixes",
    "--max-long-anchors-per-continuation",
    "--max-cache-markers-per-request",
    "--media-cache-mib",
    "--media-live-mib",
    "--media-preprocess-threads",
    "--response-store-max-records",
    "--response-store-max-mib",
    "--default-max-tokens",
    "--default-thinking-budget",
    "--vision",
    "--cors",
    "--temperature",
    "--top-p",
    "--top-k",
    "--min-p",
    "--presence-penalty",
    "--frequency-penalty",
    "--seed",
    "--greedy",
];

// These operational tuning options do not change model identity, private
// binding, device isolation, or request-generation semantics.
const ALLOWED_VALUE_NATIVE_ARGUMENTS: &[&str] = &[
    "--max-pending-requests",
    "--pending-timeout-ms",
    "--log-stats-interval-ms",
    "--max-request-mib",
    "--context-cost-presets",
];

#[derive(Debug, Clone)]
struct PendingStartup {
    request_log_path: PathBuf,
    native_identity: NinferArtifactIdentity,
    public_model_id: ModelId,
    accelerator: AcceleratorDevice,
    package_requirements: Option<NinferStartupRequirements>,
}

#[derive(Debug, Clone, PartialEq)]
struct NinferStartupRequirements {
    minimum_context_tokens: u64,
    kv_cache: String,
    kv_dtype: String,
    cuda_graph: bool,
    prefix_reuse: bool,
    speculative_backend: String,
    speculative_draft_window: u64,
    proposal_head: String,
    temperature: f64,
    top_p: f64,
    top_k: u64,
    min_p: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct NinferPackageRuntimeCapabilities {
    trustworthy_identity: bool,
    external_sharp: bool,
    process_sampler_overrides: bool,
    bounded_server_start: bool,
}

fn evaluate_ninfer_package_runtime(
    capabilities: NinferPackageRuntimeCapabilities,
) -> RuntimeCompatibility {
    match validate_ninfer_package_prelaunch(capabilities) {
        Ok(()) => RuntimeCompatibility::NeedsAttention(
            "runtime capabilities satisfy pre-launch requirements; context, KV capacity, sampler, and MTP policy still require server_start proof"
                .to_owned(),
        ),
        Err(reasons) => RuntimeCompatibility::Incompatible(format!(
            "{reasons}; actual context/KV capacity, graph/prefix state, sampler defaults, and MTP profile remain unproven until server_start"
        )),
    }
}

fn validate_ninfer_package_prelaunch(
    capabilities: NinferPackageRuntimeCapabilities,
) -> Result<(), String> {
    if !capabilities.trustworthy_identity {
        return Err(
            "the exact NInfer executable has no trustworthy package-capability observation; external binaries are not credited from filenames or upstream assumptions"
                .to_owned(),
        );
    }
    let reasons = ninfer_package_prelaunch_failures(capabilities);
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons.join("; "))
    }
}

fn ninfer_package_prelaunch_failures(
    capabilities: NinferPackageRuntimeCapabilities,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !capabilities.external_sharp {
        reasons.push("required external Sharp application is unsupported/unproven");
    }
    if !capabilities.process_sampler_overrides {
        reasons.push("package temperature/top-p/top-k/min-p process overrides are unproven");
    }
    if !capabilities.bounded_server_start {
        reasons.push("server_start context/KV/graph/prefix/speculation observation is unproven");
    }
    reasons
}

fn ninfer_package_capabilities_for_installed(
    runtime: &InstalledRuntime,
) -> NinferPackageRuntimeCapabilities {
    let managed_source = runtime.manifest.acquisition_method
        == RuntimeAcquisitionMethod::SourceBuild
        && runtime.manifest.identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && runtime.manifest.identity.upstream_revision.as_deref()
            == Some(PACKAGE_CAPABILITY_REVISION)
        && runtime.manifest.identity.variant == format!("{}-sm120a", catalog::RECIPE_VERSION);
    NinferPackageRuntimeCapabilities {
        trustworthy_identity: managed_source,
        // The current managed source recipe embeds its frontend template and
        // advertises no external template or raw pre-rendered prompt input.
        external_sharp: false,
        process_sampler_overrides: managed_source,
        bounded_server_start: managed_source,
    }
}

fn ninfer_package_capabilities_for_available(
    runtime: &AvailableRuntime,
) -> NinferPackageRuntimeCapabilities {
    let managed_source = matches!(
        runtime.acquisition,
        norted_core::RuntimeAcquisitionPlan::SourceBuild(_)
    ) && runtime.identity.package.repository.as_deref()
        == Some(GITHUB_REPOSITORY)
        && runtime.identity.upstream_revision.as_deref() == Some(PACKAGE_CAPABILITY_REVISION)
        && runtime.identity.variant == format!("{}-sm120a", catalog::RECIPE_VERSION);
    NinferPackageRuntimeCapabilities {
        trustworthy_identity: managed_source,
        external_sharp: false,
        process_sampler_overrides: managed_source,
        bounded_server_start: managed_source,
    }
}

fn ninfer_package_policy(
    package: &NortedPackageBinding,
) -> Result<&NinferPackagePolicy, EngineError> {
    match &package.policy {
        NortedPackagePolicy::Ninfer(policy) => Ok(policy),
        _ => Err(EngineError::InvalidConfiguration(
            "NInfer received a non-NInfer Norted package policy".to_owned(),
        )),
    }
}

fn ninfer_package_sampler_arguments(
    package: &NortedPackageBinding,
) -> Result<Vec<OsString>, EngineError> {
    let policy = ninfer_package_policy(package)?;
    Ok([
        "--temperature".to_owned(),
        policy.temperature.to_string(),
        "--top-p".to_owned(),
        policy.top_p.to_string(),
        "--top-k".to_owned(),
        policy.top_k.to_string(),
        "--min-p".to_owned(),
        policy.min_p.to_string(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect())
}

fn ninfer_startup_requirements(
    package: &NortedPackageBinding,
    settings: &ResolvedLoadSettings,
) -> Result<NinferStartupRequirements, EngineError> {
    let policy = ninfer_package_policy(package)?;
    let profile_name = match settings.value("ninfer.package_profile") {
        Some(LoadSettingValue::Choice(profile)) => profile.as_str(),
        Some(_) => {
            return Err(EngineError::InvalidConfiguration(
                "ninfer.package_profile must be a choice".to_owned(),
            ));
        }
        None => "mtp0",
    };
    let profile = policy.benchmark_profiles.get(profile_name).ok_or_else(|| {
        EngineError::InvalidConfiguration(format!(
            "NInfer package benchmark profile `{profile_name}` is not declared by the package"
        ))
    })?;
    Ok(NinferStartupRequirements {
        minimum_context_tokens: policy.minimum_context_tokens,
        kv_cache: policy.kv_cache.clone(),
        kv_dtype: policy.kv_dtype.clone(),
        cuda_graph: policy.cuda_graph_decode,
        prefix_reuse: policy.compatible_prefix_reuse,
        speculative_backend: profile.backend.clone().unwrap_or_else(|| "none".to_owned()),
        speculative_draft_window: profile.draft_tokens.unwrap_or(0),
        proposal_head: if profile.lm_head_draft {
            "optimized".to_owned()
        } else {
            "full".to_owned()
        },
        temperature: policy.temperature,
        top_p: policy.top_p,
        top_k: policy.top_k,
        min_p: policy.min_p,
    })
}

pub struct NinferAdapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
    pending_startups: tokio::sync::RwLock<BTreeMap<String, PendingStartup>>,
    observed_defaults: tokio::sync::RwLock<BTreeMap<String, EffectiveGenerationSettings>>,
}

impl NinferAdapter {
    pub fn from_config(config: Option<&EngineConfig>, config_directory: &Path) -> Self {
        let mut enabled = config.is_none();
        let mut binary_path = None;
        let mut native_arguments = Vec::new();
        let mut environment = BTreeMap::new();
        let mut configuration_error = None;

        if let Some(config) = config {
            enabled = config.enabled;
            environment = config.env.clone();
            if let Some(name) = environment
                .keys()
                .find(|name| conflicts_with_managed_environment(name))
            {
                configuration_error = Some(format!(
                    "environment variable `{name}` conflicts with Norted's exact NInfer CUDA device isolation"
                ));
            }
            if let Some(key) = config.settings.keys().find(|key| *key != "binary_path") {
                configuration_error = Some(format!(
                    "unsupported NInfer setting `{key}`; only `binary_path` is supported"
                ));
            }
            if configuration_error.is_none()
                && let Some(value) = config.settings.get("binary_path")
            {
                match value.as_str() {
                    Some(value) if !value.trim().is_empty() => {
                        let configured = PathBuf::from(value);
                        binary_path = Some(if configured.is_absolute() {
                            configured
                        } else {
                            config_directory.join(configured)
                        });
                    }
                    _ => {
                        configuration_error =
                            Some("NInfer `binary_path` must be a non-empty string".to_owned());
                    }
                }
            }
            if let Some(key) = config.native.keys().find(|key| *key != "arguments") {
                configuration_error = Some(format!(
                    "unsupported NInfer native setting `{key}`; use `arguments = [...]`"
                ));
            }
            if configuration_error.is_none()
                && let Some(value) = config.native.get("arguments")
            {
                match value.as_array() {
                    Some(values) => {
                        for value in values {
                            match value.as_str() {
                                Some(value) if !value.contains('\0') => {
                                    native_arguments.push(value.to_owned());
                                }
                                Some(_) => {
                                    configuration_error = Some(
                                        "NInfer native arguments cannot contain NUL bytes"
                                            .to_owned(),
                                    );
                                    break;
                                }
                                None => {
                                    configuration_error = Some(
                                        "NInfer native arguments must all be strings".to_owned(),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        configuration_error = Some(
                            "NInfer native `arguments` must be an array of strings".to_owned(),
                        );
                    }
                }
            }
            if configuration_error.is_none()
                && let Some(argument) = native_arguments
                    .iter()
                    .find(|argument| conflicts_with_managed_argument(argument))
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` conflicts with the Norted-managed NInfer contract"
                ));
            }
            if configuration_error.is_none()
                && let Some(argument) = invalid_native_argument(&native_arguments)
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` is unsupported or has invalid arity; only documented non-semantic NInfer operational tuning options are accepted"
                ));
            }
        }

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|error| {
                configuration_error.get_or_insert_with(|| {
                    format!("could not create the private NInfer HTTP client: {error}")
                });
                reqwest::Client::new()
            });
        Self {
            enabled,
            binary_path,
            native_arguments,
            environment,
            configuration_error,
            client,
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
            pending_startups: tokio::sync::RwLock::new(BTreeMap::new()),
            observed_defaults: tokio::sync::RwLock::new(BTreeMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn probe_uncached(&self) -> EngineProbe {
        if !self.enabled {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "NInfer adapter is disabled in configuration".to_owned(),
            };
        }
        if let Some(error) = &self.configuration_error {
            return invalid_probe(error.clone());
        }
        let Some(configured_path) = &self.binary_path else {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "no external ninfer-serve is configured; managed NInfer source runtimes remain available"
                    .to_owned(),
            };
        };
        let (binary_path, binary_sha256, observation) =
            match self.inspect_binary(configured_path, None).await {
                Ok(observation) => observation,
                Err(error) => return invalid_probe(error.to_string()),
            };
        EngineProbe {
            installation: InstallationState::Installed {
                installation: Box::new(EngineInstallation {
                    engine: EngineRevision {
                        engine_id: ENGINE_ID.to_owned(),
                        version: None,
                        revision: None,
                    },
                    source_repository: None,
                    acquisition_method: AcquisitionMethod::ExternalBinary,
                    binary_path,
                    binary_sha256: Some(binary_sha256),
                    build: None,
                    platform: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    runtime_variant: Some("external-binary".to_owned()),
                    acquired_at_unix: None,
                    observed_at_unix: observation.observed_at_unix,
                }),
            },
            update: UpdateState::Unknown,
            healthy: true,
            detail: format!("external configured ninfer-serve; {}", observation.detail),
        }
    }

    async fn inspect_binary(
        &self,
        configured_path: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<(PathBuf, String, RuntimeProbeObservation), EngineError> {
        let binary_path =
            canonical_regular_file(configured_path, "ninfer-serve entrypoint").await?;
        let binary_sha256 = hash_file(&binary_path).await.map_err(|error| {
            EngineError::Operation(format!("could not hash entrypoint: {error}"))
        })?;
        if let Some(expected) = expected_sha256
            && !binary_sha256.eq_ignore_ascii_case(expected)
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime entrypoint SHA-256 mismatch: expected {expected}, observed {binary_sha256}"
            )));
        }
        let output = capture_command(
            &binary_path,
            &["--help"],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await?;
        let help = command_detail(&output.stdout, &output.stderr);
        if !output.success {
            return Err(EngineError::InvalidConfiguration(format!(
                "ninfer-serve --help failed with exit code {:?}: {help}",
                output.code
            )));
        }
        if let Some(reason) = help_contract_error(&help, &self.native_arguments) {
            return Err(EngineError::InvalidConfiguration(format!(
                "entrypoint does not satisfy the ninfer-serve launch contract ({reason}): {help}"
            )));
        }
        self.capability_cache
            .write()
            .await
            .insert(binary_sha256.clone(), help);
        Ok((
            binary_path,
            binary_sha256,
            RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: ENGINE_ID.to_owned(),
                observed_version: None,
                observed_revision: None,
                detail: "ninfer-serve help contract recognized; executable reports no version or revision"
                    .to_owned(),
                observed_at_unix: unix_timestamp(),
            },
        ))
    }
}

#[async_trait]
impl EngineAdapter for NinferAdapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: ENGINE_ID.to_owned(),
            display_name: "NInfer".to_owned(),
            upstream_repository: UPSTREAM_REPOSITORY.to_owned(),
        }
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::Ninfer],
            api: vec![ApiCapability::Responses, ApiCapability::ChatCompletions],
            features: vec![EngineFeature::TextGeneration],
        }
    }

    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        if !self.enabled {
            CompatibilityDecision::Unsupported {
                reason: "NInfer adapter is disabled in configuration".to_owned(),
            }
        } else if let Some(error) = &self.configuration_error {
            CompatibilityDecision::Unsupported {
                reason: error.clone(),
            }
        } else {
            CompatibilityDecision::Supported
        }
    }

    fn available_runtime_compatibility(&self, runtime: &AvailableRuntime) -> CompatibilityDecision {
        if runtime.identity.engine_id != ENGINE_ID {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "runtime belongs to engine `{}`, not `{ENGINE_ID}`",
                    runtime.identity.engine_id
                ),
            };
        }
        if !runtime.supported_formats.contains(&ArtifactFormat::Ninfer) {
            return CompatibilityDecision::Unsupported {
                reason: "runtime does not declare NInfer artifact support".to_owned(),
            };
        }
        CompatibilityDecision::Supported
    }

    fn runtime_compatibility(&self, runtime: &InstalledRuntime) -> CompatibilityDecision {
        if runtime.manifest.identity.engine_id != ENGINE_ID {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "runtime belongs to engine `{}`, not `{ENGINE_ID}`",
                    runtime.manifest.identity.engine_id
                ),
            };
        }
        if !runtime
            .manifest
            .supported_formats
            .contains(&ArtifactFormat::Ninfer)
        {
            return CompatibilityDecision::Unsupported {
                reason: "runtime does not declare NInfer artifact support".to_owned(),
            };
        }
        CompatibilityDecision::Supported
    }

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if let CompatibilityDecision::Unsupported { reason } =
            self.runtime_management_compatibility()
        {
            return CompatibilityDecision::Unsupported { reason };
        }
        if model.format != ArtifactFormat::Ninfer {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "NInfer requires a `.ninfer` artifact, not `{}`",
                    model.format.as_str()
                ),
            };
        }
        match model.native_identity.as_ref() {
            Some(ArtifactNativeIdentity::Ninfer(identity)) if identity.container_version == 2 => {
                CompatibilityDecision::Supported
            }
            Some(ArtifactNativeIdentity::Ninfer(identity)) => CompatibilityDecision::Unsupported {
                reason: format!(
                    "NInfer container version {} is unsupported; version 2 is required",
                    identity.container_version
                ),
            },
            None => CompatibilityDecision::Unsupported {
                reason: "NInfer model is missing inspected native container identity; rediscover the artifact"
                    .to_owned(),
            },
        }
    }

    fn runtime_model_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        if let CompatibilityDecision::Unsupported { reason } = self.compatibility(model) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        if let CompatibilityDecision::Unsupported { reason } = self.runtime_compatibility(runtime) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        let native = native_compatibility(
            &runtime.manifest.supported_native_identities,
            model
                .native_identity
                .as_ref()
                .expect("compatibility checked identity"),
        );
        let device = ninfer_device_evaluation(
            &runtime.manifest.identity.platform,
            &runtime.manifest.identity.architecture,
            &runtime.manifest.requirements,
            host,
            runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
        )
        .compatibility;
        let base = combine_compatibility(native, device);
        if model.norted_package.is_some() {
            combine_compatibility(
                base,
                evaluate_ninfer_package_runtime(ninfer_package_capabilities_for_installed(runtime)),
            )
        } else {
            base
        }
    }

    fn available_runtime_model_compatibility(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        if let CompatibilityDecision::Unsupported { reason } = self.compatibility(model) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        if let CompatibilityDecision::Unsupported { reason } =
            self.available_runtime_compatibility(runtime)
        {
            return RuntimeCompatibility::Incompatible(reason);
        }
        let native = native_compatibility(
            &runtime.supported_native_identities,
            model
                .native_identity
                .as_ref()
                .expect("compatibility checked identity"),
        );
        let device = ninfer_device_evaluation(
            &runtime.identity.platform,
            &runtime.identity.architecture,
            &runtime.requirements,
            host,
            false,
        )
        .compatibility;
        let base = combine_compatibility(native, device);
        if model.norted_package.is_some() {
            combine_compatibility(
                base,
                evaluate_ninfer_package_runtime(ninfer_package_capabilities_for_available(runtime)),
            )
        } else {
            base
        }
    }

    fn runtime_package_sharp_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
    ) -> Option<RuntimeCompatibility> {
        model.norted_package.as_ref()?;
        let capabilities = ninfer_package_capabilities_for_installed(runtime);
        Some(if !capabilities.trustworthy_identity {
            RuntimeCompatibility::Incompatible(
                "external Sharp application capability is unproven for this exact NInfer executable"
                    .to_owned(),
            )
        } else if capabilities.external_sharp {
            RuntimeCompatibility::Compatible
        } else {
            RuntimeCompatibility::Incompatible(
                "required external Sharp application is unsupported/unproven".to_owned(),
            )
        })
    }

    fn runtime_model_accelerator(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> Option<AcceleratorDevice> {
        matches!(self.compatibility(model), CompatibilityDecision::Supported).then(|| {
            ninfer_device_evaluation(
                &runtime.manifest.identity.platform,
                &runtime.manifest.identity.architecture,
                &runtime.manifest.requirements,
                host,
                runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
            )
            .accelerator
        })?
    }

    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        if model.format != ArtifactFormat::Ninfer {
            return Err(EngineError::InvalidConfiguration(
                "NInfer can only prepare `.ninfer` artifacts".to_owned(),
            ));
        }
        let expected = model.native_identity.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer artifact is missing its inspected native identity".to_owned(),
            )
        })?;
        let canonical = canonical_regular_file(&model.path, "NInfer model").await?;
        let inspected = inspect_ninfer_container(&canonical).map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not re-inspect NInfer artifact {}: {error}",
                canonical.display()
            ))
        })?;
        let observed = ArtifactNativeIdentity::Ninfer(inspected.identity);
        if observed != expected {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after discovery".to_owned(),
            ));
        }
        if model.norted_package.is_some() {
            let mut prepared = prepare_norted_package_input(model).await?;
            prepared.primary.native_identity = Some(observed);
            Ok(prepared)
        } else {
            let mut primary = model.clone();
            primary.path = canonical;
            primary.native_identity = Some(observed);
            Ok(PreparedModelInput {
                primary,
                auxiliary: Vec::new(),
                primary_file_identity: None,
            })
        }
    }

    async fn prepare_model_input_with_progress(
        &self,
        model: &ModelArtifact,
        progress: LoadProgressReporter,
    ) -> Result<PreparedModelInput, EngineError> {
        if model.format != ArtifactFormat::Ninfer {
            return Err(EngineError::InvalidConfiguration(
                "NInfer can only prepare `.ninfer` artifacts".to_owned(),
            ));
        }
        let expected = model.native_identity.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer artifact is missing its inspected native identity".to_owned(),
            )
        })?;
        let canonical = canonical_regular_file(&model.path, "NInfer model").await?;
        let inspected = inspect_ninfer_container(&canonical).map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not re-inspect NInfer artifact {}: {error}",
                canonical.display()
            ))
        })?;
        let observed = ArtifactNativeIdentity::Ninfer(inspected.identity);
        if observed != expected {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after discovery".to_owned(),
            ));
        }
        if model.norted_package.is_some() {
            let mut prepared = prepare_norted_package_input_with_progress(model, &progress).await?;
            prepared.primary.native_identity = Some(observed);
            Ok(prepared)
        } else {
            let mut primary = model.clone();
            primary.path = canonical;
            primary.native_identity = Some(observed);
            Ok(PreparedModelInput {
                primary,
                auxiliary: Vec::new(),
                primary_file_identity: None,
            })
        }
    }

    fn native_options(&self) -> Vec<NativeOption> {
        vec![NativeOption {
            name: "arguments".to_owned(),
            description:
                "Strictly allowlisted non-semantic ninfer-serve operational tuning options"
                    .to_owned(),
            value_kind: OptionValueKind::String,
            repeatable: true,
        }]
    }

    fn load_setting_definitions(&self) -> Vec<norted_core::LoadSettingDefinition> {
        settings::definitions()
    }

    async fn load_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> Result<norted_core::LoadSettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self
            .capability_cache
            .read()
            .await
            .get(&runtime.manifest.entrypoint_sha256.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                EngineError::Operation("NInfer help observation was not cached".to_owned())
            })?;
        let mut definitions = settings::definitions();
        settings::apply_runtime_bounds(&mut definitions);
        for definition in &mut definitions {
            if definition.id.as_str() == "ninfer.package_profile" {
                if model.norted_package.is_none() {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "benchmark profile selection is available only for a manifest-bound Norted NInfer package"
                            .to_owned(),
                    );
                }
                continue;
            }
            let option = settings::option_for_setting(definition.id.as_str());
            if option.is_empty() || !usage_has_token(&help, option) {
                definition.supported = false;
                definition.unsupported_reason = Some(format!(
                    "the exact ninfer-serve help contract does not advertise `{option}`"
                ));
            }
        }
        Ok(norted_core::LoadSettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions,
        })
    }

    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        Ok(self.probe_uncached().await)
    }

    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        if !self.enabled {
            return Err(EngineError::InvalidConfiguration(
                "NInfer adapter is disabled in configuration".to_owned(),
            ));
        }
        if let Some(error) = &self.configuration_error {
            return Err(EngineError::InvalidConfiguration(error.clone()));
        }
        runtime
            .manifest
            .identity
            .validate()
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        if runtime.manifest.runtime_id != RuntimeId::from_identity(&runtime.manifest.identity) {
            return Err(EngineError::InvalidConfiguration(
                "runtime ID does not match its structured identity".to_owned(),
            ));
        }
        if let CompatibilityDecision::Unsupported { reason } = self.runtime_compatibility(runtime) {
            return Err(EngineError::InvalidConfiguration(reason));
        }
        let configured_path = runtime.entrypoint_path();
        if runtime.manifest.acquisition_method != RuntimeAcquisitionMethod::ExternalBinary {
            let root = tokio::fs::canonicalize(&runtime.installation_root)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime installation root could not be resolved: {error}"
                    ))
                })?;
            let entrypoint = tokio::fs::canonicalize(&configured_path)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime entrypoint could not be resolved: {error}"
                    ))
                })?;
            if !entrypoint.starts_with(&root) {
                return Err(EngineError::InvalidConfiguration(
                    "managed NInfer entrypoint escapes its installation root".to_owned(),
                ));
            }
        }
        let (_, _, observation) = self
            .inspect_binary(&configured_path, Some(&runtime.manifest.entrypoint_sha256))
            .await?;
        Ok(observation)
    }

    fn source_native_identities(
        &self,
        source_root: &Path,
    ) -> Result<Vec<ArtifactNativeIdentity>, EngineError> {
        Ok(inspect_source_native_identities(source_root))
    }

    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "NInfer backend address must be loopback".to_owned(),
            ));
        }
        if request.model.primary.norted_package.is_some() {
            let capabilities = ninfer_package_capabilities_for_installed(&request.runtime);
            if let Err(reason) = validate_ninfer_package_prelaunch(capabilities) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "selected NInfer runtime cannot launch the Norted package: {reason}"
                )));
            }
        }
        request
            .load_settings_schema
            .validate(&request.load_settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        let mut structured = settings::translate(
            &request.load_settings,
            &request.model.primary,
            &self.native_arguments,
        )?;
        let package_requirements = request
            .model
            .primary
            .norted_package
            .as_ref()
            .map(|package| ninfer_startup_requirements(package, &request.load_settings))
            .transpose()?;
        if let Some(package) = request.model.primary.norted_package.as_ref() {
            structured.extend(ninfer_package_sampler_arguments(package)?);
        }
        let model_path =
            canonical_regular_file(&request.model.primary.path, "NInfer model").await?;
        if model_path != request.model.primary.path {
            return Err(EngineError::InvalidConfiguration(
                "prepared NInfer model path changed before launch".to_owned(),
            ));
        }
        let expected_identity = request
            .model
            .primary
            .native_identity
            .as_ref()
            .map(|identity| match identity {
                ArtifactNativeIdentity::Ninfer(identity) => identity.clone(),
            })
            .ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "prepared NInfer model has no native identity".to_owned(),
                )
            })?;
        let observed_identity = inspect_ninfer_container(&model_path)
            .map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "could not revalidate NInfer artifact before launch: {error}"
                ))
            })?
            .identity;
        if observed_identity != expected_identity {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after prepared-input validation"
                    .to_owned(),
            ));
        }
        let observation = self.probe_runtime(&request.runtime).await?;
        let accelerator = request.accelerator.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer launch has no exact NVIDIA GPU selected by compatibility evaluation"
                    .to_owned(),
            )
        })?;
        let environment = isolated_cuda_environment(&self.environment, &accelerator, "NInfer")
            .map_err(EngineError::InvalidConfiguration)?;
        let request_log_path = create_private_request_log()?;
        let endpoint = http_endpoint(request.backend_address);
        let public_model_id = request.model.primary.id.clone();
        let mut arguments = managed_launch_arguments(
            &model_path,
            request.backend_address,
            &public_model_id,
            &request_log_path,
        );
        arguments.extend(structured);
        arguments.extend(self.native_arguments.iter().map(OsString::from));

        let manifest = &request.runtime.manifest;
        let binary_path = request.runtime.entrypoint_path();
        let installation = EngineInstallation {
            engine: EngineRevision {
                engine_id: ENGINE_ID.to_owned(),
                version: observation.observed_version.clone(),
                revision: observation.observed_revision.clone(),
            },
            source_repository: manifest.identity.package.repository.clone(),
            acquisition_method: match manifest.acquisition_method {
                RuntimeAcquisitionMethod::OfficialReleaseAsset
                | RuntimeAcquisitionMethod::PreseededOfficialPack => {
                    AcquisitionMethod::OfficialBinary
                }
                RuntimeAcquisitionMethod::SourceBuild => AcquisitionMethod::SourceBuild,
                RuntimeAcquisitionMethod::ExternalBinary => AcquisitionMethod::ExternalBinary,
            },
            binary_path: binary_path.clone(),
            binary_sha256: Some(manifest.entrypoint_sha256.clone()),
            build: None,
            platform: manifest.identity.platform.clone(),
            architecture: manifest.identity.architecture.clone(),
            runtime_variant: Some(manifest.identity.variant.clone()),
            acquired_at_unix: manifest.installed_at_unix,
            observed_at_unix: observation.observed_at_unix,
        };

        if let Some(previous) = self.pending_startups.write().await.insert(
            endpoint.clone(),
            PendingStartup {
                request_log_path: request_log_path.clone(),
                native_identity: expected_identity.clone(),
                public_model_id: public_model_id.clone(),
                accelerator: accelerator.clone(),
                package_requirements: package_requirements.clone(),
            },
        ) {
            let _ = tokio::fs::remove_file(previous.request_log_path).await;
        }
        self.observed_defaults.write().await.remove(&endpoint);

        let mut normalized_settings = BTreeMap::from([
            (
                "container_version".to_owned(),
                json!(expected_identity.container_version),
            ),
            (
                "native_model_id".to_owned(),
                json!(expected_identity.model_id),
            ),
            (
                "native_weights_id".to_owned(),
                json!(expected_identity.weights_id),
            ),
            ("request_log_schema".to_owned(), json!(18)),
        ]);
        if let Some(requirements) = package_requirements.as_ref() {
            normalized_settings.extend([
                (
                    "package_temperature".to_owned(),
                    json!(requirements.temperature),
                ),
                ("package_top_p".to_owned(), json!(requirements.top_p)),
                ("package_top_k".to_owned(), json!(requirements.top_k)),
                ("package_min_p".to_owned(), json!(requirements.min_p)),
                (
                    "package_minimum_context_tokens".to_owned(),
                    json!(requirements.minimum_context_tokens),
                ),
                ("package_kv_cache".to_owned(), json!(requirements.kv_cache)),
                ("package_kv_dtype".to_owned(), json!(requirements.kv_dtype)),
                (
                    "package_speculative_backend".to_owned(),
                    json!(requirements.speculative_backend),
                ),
                (
                    "package_speculative_draft_window".to_owned(),
                    json!(requirements.speculative_draft_window),
                ),
                (
                    "package_proposal_head".to_owned(),
                    json!(requirements.proposal_head),
                ),
            ]);
        }

        Ok(LaunchSpec {
            executable: binary_path.clone(),
            arguments,
            environment,
            environment_remove: managed_environment_removals(),
            inherits_parent_environment: true,
            working_directory: binary_path.parent().map(PathBuf::from),
            temporary_files: vec![request_log_path],
            endpoint: Some(endpoint),
            normalized_settings,
            load_settings: request.load_settings,
            native_arguments: self.native_arguments.clone(),
            installation,
            runtime: request.runtime,
            model: request.model,
            accelerator: Some(accelerator),
        })
    }

    fn prepare_launch_progress(&self, spec: &LaunchSpec) -> Option<BackendLoadProgress> {
        spec.model.primary.norted_package.as_ref().map(|_| {
            BackendLoadProgress::with_message(
                BackendLoadPhase::PreparingLaunch,
                "Revalidating package before launch",
            )
        })
    }

    async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
        revalidate_norted_package_before_launch(&spec.model).await
    }

    async fn prepare_launch_attempt_with_progress(
        &self,
        spec: &LaunchSpec,
        progress: LoadProgressReporter,
    ) -> Result<(), EngineError> {
        revalidate_norted_package_before_launch_with_progress(&spec.model, &progress).await
    }

    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("NInfer process has no backend endpoint".to_owned())
        })?;
        let response = self
            .client
            .get(format!("{endpoint}/health"))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(EngineError::BackendUnavailable(format!(
                "NInfer health endpoint returned HTTP {}",
                response.status()
            )));
        }
        let health = response.json::<HealthResponse>().await.map_err(|error| {
            EngineError::Operation(format!("invalid NInfer health response: {error}"))
        })?;
        if health.status != "ok" {
            return Ok(false);
        }
        if self.observed_defaults.read().await.contains_key(endpoint) {
            return Ok(true);
        }
        let pending = self
            .pending_startups
            .read()
            .await
            .get(endpoint)
            .cloned()
            .ok_or_else(|| {
                EngineError::Operation(
                    "NInfer startup provenance is unavailable for this process".to_owned(),
                )
            })?;
        let result = match read_and_validate_startup_log(&pending).await {
            Ok(Some(defaults)) => defaults,
            Ok(None) => return Ok(false),
            Err(error) => {
                self.pending_startups.write().await.remove(endpoint);
                let _ = tokio::fs::remove_file(&pending.request_log_path).await;
                return Err(error);
            }
        };
        self.pending_startups.write().await.remove(endpoint);
        if let Err(error) = tokio::fs::remove_file(&pending.request_log_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(EngineError::Operation(format!(
                "could not unlink the private NInfer startup log before serving requests: {error}"
            )));
        }
        self.observed_defaults
            .write()
            .await
            .insert(endpoint.to_owned(), result);
        Ok(true)
    }

    fn startup_progress(&self, stderr_tail: &[String]) -> Option<BackendLoadProgress> {
        parse_ninfer_startup_progress(stderr_tail)
    }

    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("NInfer process has no backend endpoint".to_owned())
        })?;
        self.observed_defaults
            .read()
            .await
            .get(endpoint)
            .copied()
            .ok_or_else(|| {
                EngineError::Operation(
                    "NInfer effective sampler defaults were not validated at startup".to_owned(),
                )
            })
    }

    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        _backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        if settings
            .temperature
            .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer temperature must be finite and in 0..=2".to_owned(),
            ));
        }
        if settings
            .top_p
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer top_p must be finite and in 0..=1".to_owned(),
            ));
        }
        Ok(())
    }

    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&protocol::backend_request(&request, false))
            .send()
            .await
            .map_err(protocol::map_transport_error)?;
        protocol::parse_completion_response(response).await
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceStream, EngineError> {
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&protocol::backend_request(&request, true))
            .send()
            .await
            .map_err(protocol::map_transport_error)?;
        protocol::parse_stream_response(response).await
    }
}

#[derive(Debug)]
struct DeviceEvaluation {
    compatibility: RuntimeCompatibility,
    accelerator: Option<AcceleratorDevice>,
}

fn ninfer_device_evaluation(
    platform: &str,
    architecture: &str,
    requirements: &RuntimeRequirements,
    host: &HostCapabilities,
    external: bool,
) -> DeviceEvaluation {
    let devices = match visible_nvidia_devices(host, "NInfer") {
        Ok(devices) => devices,
        Err(compatibility) => {
            return DeviceEvaluation {
                compatibility,
                accelerator: None,
            };
        }
    };
    let mut requirements = requirements.clone();
    requirements.requires_nvidia_gpu = true;
    if external {
        requirements.unverified_requirements.push(
            "external ninfer-serve GPU targets and exact native artifact registry are unverified"
                .to_owned(),
        );
    }
    let mut evaluated = devices
        .into_iter()
        .map(|device| {
            let device_host = HostCapabilities {
                platform: host.platform.clone(),
                architecture: host.architecture.clone(),
                accelerators: vec![device.clone()],
                cuda_visible_devices: None,
                observations: Vec::new(),
            };
            (
                device.clone(),
                compatibility_for(platform, architecture, "cuda", &requirements, &device_host),
            )
        })
        .collect::<Vec<_>>();
    evaluated.sort_by(|left, right| {
        left.1
            .preference_rank()
            .cmp(&right.1.preference_rank())
            .then_with(|| {
                right
                    .0
                    .vram_bytes
                    .unwrap_or(0)
                    .cmp(&left.0.vram_bytes.unwrap_or(0))
            })
            .then_with(|| left.0.stable_id.cmp(&right.0.stable_id))
    });
    match evaluated.into_iter().next() {
        Some((accelerator, compatibility)) => DeviceEvaluation {
            compatibility,
            accelerator: Some(accelerator),
        },
        None => DeviceEvaluation {
            compatibility: RuntimeCompatibility::NeedsAttention(
                "NInfer requires a stable NVIDIA GPU UUID, but none was observed".to_owned(),
            ),
            accelerator: None,
        },
    }
}

fn native_compatibility(
    supported: &[ArtifactNativeIdentity],
    model: &ArtifactNativeIdentity,
) -> RuntimeCompatibility {
    if supported.is_empty() {
        RuntimeCompatibility::NeedsAttention(
            "the exact ninfer-serve target registry could not be enumerated independently"
                .to_owned(),
        )
    } else if supported.contains(model) {
        RuntimeCompatibility::Recommended
    } else {
        let ArtifactNativeIdentity::Ninfer(identity) = model;
        RuntimeCompatibility::Incompatible(format!(
            "runtime does not declare support for NInfer target `{}/{}`",
            identity.model_id, identity.weights_id
        ))
    }
}

fn combine_compatibility(
    left: RuntimeCompatibility,
    right: RuntimeCompatibility,
) -> RuntimeCompatibility {
    match (left, right) {
        (RuntimeCompatibility::Incompatible(reason), _)
        | (_, RuntimeCompatibility::Incompatible(reason)) => {
            RuntimeCompatibility::Incompatible(reason)
        }
        (RuntimeCompatibility::NeedsAttention(reason), _)
        | (_, RuntimeCompatibility::NeedsAttention(reason)) => {
            RuntimeCompatibility::NeedsAttention(reason)
        }
        (RuntimeCompatibility::Recommended, RuntimeCompatibility::Recommended) => {
            RuntimeCompatibility::Recommended
        }
        _ => RuntimeCompatibility::Compatible,
    }
}

#[derive(Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Deserialize)]
struct StartupRecord {
    artifact_type: String,
    schema_version: u32,
    event: String,
    server: StartupServer,
    artifact: StartupArtifact,
    engine: StartupEngine,
    sampling_defaults: StartupSamplingDefaults,
    environment: StartupEnvironment,
}

#[derive(Deserialize)]
struct StartupServer {
    public_model_id: String,
    default_thinking: bool,
}

#[derive(Deserialize)]
struct StartupArtifact {
    target: String,
    weights_id: String,
}

#[derive(Deserialize)]
struct StartupEngine {
    max_context: u64,
    kv_capacity_mode: String,
    kv_capacity: u64,
    kv_cache: String,
    cuda_graph: bool,
    prefix_reuse: bool,
    speculative_backend: String,
    speculative_draft_window: u64,
    proposal_head: String,
    context_cost: StartupContextCost,
}

#[derive(Deserialize)]
struct StartupContextCost {
    model_id: String,
    weights_id: String,
}

#[derive(Deserialize)]
struct StartupSamplingDefaults {
    thinking: StartupPreset,
    non_thinking: StartupPreset,
    server_overrides: StartupOverrides,
    greedy: bool,
}

#[derive(Clone, Copy, Deserialize)]
struct StartupPreset {
    temperature: f64,
    top_p: f64,
    top_k: u64,
    min_p: f64,
}

#[derive(Deserialize)]
struct StartupOverrides {
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u64>,
    min_p: Option<f64>,
}

#[derive(Deserialize)]
struct StartupEnvironment {
    gpu_name: String,
    gpu_uuid: String,
    compute_capability_major: u16,
    compute_capability_minor: u16,
}

/// Best-effort UX progress from ninfer-serve stderr. NInfer's authoritative
/// startup contract is the private JSONL request log validated in `health`,
/// not stderr; this parser only recognizes a few conservative, stable
/// prefixes and degrades to `None` for unknown output. It never invents a
/// fraction and is never an admission gate.
fn parse_ninfer_startup_progress(stderr_tail: &[String]) -> Option<BackendLoadProgress> {
    let mut progress = None;
    for line in stderr_tail {
        let line = line.trim();
        if line.contains("listening on")
            || line.contains("server started")
            || line.contains("ninfer-serve ready")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::VerifyingStartup,
                "Starting the NInfer server",
            ));
        } else if line.contains("loading model")
            || line.contains("loading weights")
            || line.contains("loading artifact")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::LoadingModel,
                "Loading model weights",
            ));
        } else if line.contains("allocating kv")
            || line.contains("allocating context")
            || line.contains("initializing kv cache")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::AllocatingContext,
                "Allocating context and KV cache",
            ));
        }
    }
    progress
}

async fn read_and_validate_startup_log(
    pending: &PendingStartup,
) -> Result<Option<EffectiveGenerationSettings>, EngineError> {
    let file = tokio::fs::File::open(&pending.request_log_path)
        .await
        .map_err(|error| {
            EngineError::Operation(format!("could not open NInfer startup log: {error}"))
        })?;
    let mut bytes = Vec::new();
    file.take(STARTUP_LOG_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            EngineError::Operation(format!("could not read NInfer startup log: {error}"))
        })?;
    if bytes.len() as u64 > STARTUP_LOG_LIMIT {
        return Err(EngineError::Operation(
            "NInfer startup log exceeded the local size limit".to_owned(),
        ));
    }
    let mut startup = None;
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line.len() > STARTUP_LOG_LINE_LIMIT {
            return Err(EngineError::Operation(
                "NInfer startup log record exceeded the local size limit".to_owned(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(line).map_err(|error| {
            EngineError::Operation(format!("invalid NInfer startup JSONL record: {error}"))
        })?;
        if value.get("event").and_then(serde_json::Value::as_str) == Some("server_start") {
            if startup.is_some() {
                return Err(EngineError::Operation(
                    "NInfer startup log contained multiple server_start records".to_owned(),
                ));
            }
            startup = Some(
                serde_json::from_value::<StartupRecord>(value).map_err(|error| {
                    EngineError::Operation(format!("invalid NInfer server_start record: {error}"))
                })?,
            );
        }
    }
    let Some(startup) = startup else {
        return Ok(None);
    };
    if startup.artifact_type != "ninfer_serve_request_log"
        || startup.schema_version != 18
        || startup.event != "server_start"
    {
        return Err(EngineError::Operation(
            "NInfer startup record has an unsupported artifact type or schema".to_owned(),
        ));
    }
    if startup.server.public_model_id != pending.public_model_id.0 {
        return Err(EngineError::Operation(
            "NInfer startup public model ID differs from the Norted model ID".to_owned(),
        ));
    }
    if startup.artifact.target != pending.native_identity.model_id
        || startup.artifact.weights_id != pending.native_identity.weights_id
        || startup.engine.context_cost.model_id != pending.native_identity.model_id
        || startup.engine.context_cost.weights_id != pending.native_identity.weights_id
    {
        return Err(EngineError::Operation(
            "NInfer startup native target differs from the inspected artifact identity".to_owned(),
        ));
    }
    let expected_uuid = pending.accelerator.stable_id.as_deref().ok_or_else(|| {
        EngineError::Operation("selected NInfer accelerator has no GPU UUID".to_owned())
    })?;
    if startup.environment.gpu_uuid != expected_uuid {
        return Err(EngineError::Operation(
            "NInfer reported a different GPU UUID than the isolated selected device".to_owned(),
        ));
    }
    if let Some(expected_name) = pending.accelerator.name.as_deref()
        && startup.environment.gpu_name != expected_name
    {
        return Err(EngineError::Operation(
            "NInfer reported a different GPU name than the selected device".to_owned(),
        ));
    }
    if let Some(expected) = pending.accelerator.compute_capability
        && (startup.environment.compute_capability_major != expected.major
            || startup.environment.compute_capability_minor != expected.minor)
    {
        return Err(EngineError::Operation(
            "NInfer reported a different compute capability than the selected device".to_owned(),
        ));
    }
    if startup.sampling_defaults.greedy {
        return Err(EngineError::Operation(
            "NInfer unexpectedly started with process-wide greedy sampling".to_owned(),
        ));
    }
    let preset = if startup.server.default_thinking {
        startup.sampling_defaults.thinking
    } else {
        startup.sampling_defaults.non_thinking
    };
    let defaults = EffectiveGenerationSettings {
        temperature: startup
            .sampling_defaults
            .server_overrides
            .temperature
            .unwrap_or(preset.temperature),
        top_p: startup
            .sampling_defaults
            .server_overrides
            .top_p
            .unwrap_or(preset.top_p),
    };
    let effective_top_k = startup
        .sampling_defaults
        .server_overrides
        .top_k
        .unwrap_or(preset.top_k);
    let effective_min_p = startup
        .sampling_defaults
        .server_overrides
        .min_p
        .unwrap_or(preset.min_p);
    if !defaults.temperature.is_finite()
        || !(0.0..=2.0).contains(&defaults.temperature)
        || !defaults.top_p.is_finite()
        || !(0.0..=1.0).contains(&defaults.top_p)
    {
        return Err(EngineError::Operation(
            "NInfer startup sampler defaults are outside the supported API ranges".to_owned(),
        ));
    }
    if let Some(requirements) = pending.package_requirements.as_ref() {
        if !startup.server.default_thinking {
            return Err(EngineError::Operation(
                "NInfer package startup did not enable thinking by default".to_owned(),
            ));
        }
        if startup.engine.max_context < requirements.minimum_context_tokens {
            return Err(EngineError::Operation(format!(
                "NInfer package startup served only {} context tokens; at least {} are required",
                startup.engine.max_context, requirements.minimum_context_tokens
            )));
        }
        if !matches!(
            startup.engine.kv_capacity_mode.as_str(),
            "auto" | "explicit"
        ) || startup.engine.kv_capacity < startup.engine.max_context
            || startup.engine.kv_capacity < requirements.minimum_context_tokens
        {
            return Err(EngineError::Operation(format!(
                "NInfer package startup KV capacity {} ({}) does not prove the {}-token serving contract",
                startup.engine.kv_capacity,
                startup.engine.kv_capacity_mode,
                requirements.minimum_context_tokens
            )));
        }
        if startup.engine.kv_cache != requirements.kv_cache
            || startup.engine.cuda_graph != requirements.cuda_graph
            || startup.engine.prefix_reuse != requirements.prefix_reuse
        {
            return Err(EngineError::Operation(
                "NInfer package startup did not prove the required KV/CUDA-graph/prefix-reuse policy"
                    .to_owned(),
            ));
        }
        if startup.engine.speculative_backend != requirements.speculative_backend
            || startup.engine.speculative_draft_window != requirements.speculative_draft_window
            || startup.engine.proposal_head != requirements.proposal_head
        {
            return Err(EngineError::Operation(
                "NInfer package startup did not prove the selected MTP0/MTP3 profile".to_owned(),
            ));
        }
        if !approximately_equal(defaults.temperature, requirements.temperature)
            || !approximately_equal(defaults.top_p, requirements.top_p)
            || effective_top_k != requirements.top_k
            || !approximately_equal(effective_min_p, requirements.min_p)
        {
            return Err(EngineError::Operation(
                "NInfer package startup did not resolve the required temperature/top-p/top-k/min-p defaults"
                    .to_owned(),
            ));
        }
    }
    Ok(Some(defaults))
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= f64::EPSILON * left.abs().max(right.abs()).max(1.0) * 8.0
}

fn create_private_request_log() -> Result<PathBuf, EngineError> {
    let temporary = tempfile::Builder::new()
        .prefix("norted-ninfer-")
        .suffix(".jsonl")
        .tempfile()
        .map_err(|error| {
            EngineError::Operation(format!(
                "could not create private NInfer startup log: {error}"
            ))
        })?;
    let (_file, path) = temporary.keep().map_err(|error| {
        EngineError::Operation(format!(
            "could not retain private NInfer startup log: {error}"
        ))
    })?;
    Ok(path)
}

fn managed_launch_arguments(
    model_path: &Path,
    backend_address: SocketAddr,
    public_model_id: &ModelId,
    request_log_path: &Path,
) -> Vec<OsString> {
    vec![
        model_path.as_os_str().to_owned(),
        OsString::from("--host"),
        OsString::from(backend_address.ip().to_string()),
        OsString::from("--port"),
        OsString::from(backend_address.port().to_string()),
        OsString::from("--model-id"),
        OsString::from(public_model_id.0.clone()),
        OsString::from("--device"),
        OsString::from("0"),
        OsString::from("--request-log-jsonl"),
        request_log_path.as_os_str().to_owned(),
    ]
}

fn inspect_source_native_identities(source_root: &Path) -> Vec<ArtifactNativeIdentity> {
    let targets = source_root.join("src").join("targets");
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return Vec::new();
    };
    let mut identities = BTreeSet::new();
    let mut package_count = 0_usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return Vec::new();
        };
        let target = entry.path();
        if !target.is_dir() {
            continue;
        }
        let package = target.join("impl").join("package.cpp");
        if !package.is_file() {
            continue;
        }
        package_count += 1;
        let Some(symbols) = parse_target_symbols(&target) else {
            return Vec::new();
        };
        let Ok(package_source) = std::fs::read_to_string(&package) else {
            return Vec::new();
        };
        let mut conditions = 0_usize;
        for line in package_source.lines() {
            if !line.contains("identity.model_id ==") {
                continue;
            }
            conditions += 1;
            let Some((symbol, weights_id)) = parse_identity_condition(line) else {
                return Vec::new();
            };
            let Some(model_id) = symbols.get(symbol) else {
                return Vec::new();
            };
            identities.insert(ArtifactNativeIdentity::Ninfer(NinferArtifactIdentity {
                container_version: 2,
                model_id: model_id.clone(),
                weights_id: weights_id.to_owned(),
            }));
        }
        if conditions == 0 {
            return Vec::new();
        }
    }
    if package_count == 0 {
        Vec::new()
    } else {
        identities.into_iter().collect()
    }
}

fn parse_target_symbols(target: &Path) -> Option<BTreeMap<String, String>> {
    let export = target.join("export");
    let mut symbols = BTreeMap::new();
    for entry in WalkDir::new(export).follow_links(false) {
        let entry = entry.ok()?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("h")
        {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).ok()?;
        for line in source.lines() {
            let Some(rest) = line
                .trim()
                .strip_prefix("static constexpr std::string_view ")
            else {
                continue;
            };
            let (symbol, value) = rest.split_once('=')?;
            let symbol = symbol.trim();
            let value = value.trim().strip_suffix(';')?.trim();
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            if symbol.is_empty() || value.is_empty() {
                return None;
            }
            if symbols
                .insert(symbol.to_owned(), value.to_owned())
                .is_some()
            {
                return None;
            }
        }
    }
    (!symbols.is_empty()).then_some(symbols)
}

fn parse_identity_condition(line: &str) -> Option<(&str, &str)> {
    let (_, after_model) = line.split_once("identity.model_id ==")?;
    let (symbol, after_and) = after_model.split_once("&&")?;
    let symbol = symbol.trim();
    let after_weights = after_and
        .trim()
        .strip_prefix("identity.weights_id ==")?
        .trim();
    let after_quote = after_weights.strip_prefix('"')?;
    let (weights_id, suffix) = after_quote.split_once('"')?;
    if symbol.is_empty() || weights_id.is_empty() || !suffix.trim_start().starts_with(')') {
        return None;
    }
    Some((symbol, weights_id))
}

async fn canonical_regular_file(path: &Path, description: &str) -> Result<PathBuf, EngineError> {
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be resolved: {error}",
            path.display()
        ))
    })?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be inspected: {error}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(EngineError::InvalidConfiguration(format!(
            "{description} is not a regular file"
        )));
    }
    Ok(canonical)
}

async fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn help_contract_error(help: &str, native_arguments: &[String]) -> Option<String> {
    for required in [
        "<model.ninfer>",
        "--host",
        "--port",
        "--model-id",
        "--device",
        "--request-log-jsonl",
        "Responses/Chat",
    ] {
        if !help.contains(required) {
            return Some(format!(
                "required capability `{required}` was not advertised"
            ));
        }
    }
    native_arguments
        .iter()
        .filter(|argument| argument.starts_with("--"))
        .find(|argument| !usage_has_token(help, argument))
        .map(|argument| format!("configured native option `{argument}` was not advertised"))
}

fn usage_has_token(output: &str, expected: &str) -> bool {
    output.split_whitespace().any(|token| {
        token.trim_matches(|character: char| {
            matches!(
                character,
                '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | ',' | '.' | ':' | ';' | '`'
            )
        }) == expected
    })
}

fn conflicts_with_managed_argument(argument: &str) -> bool {
    let argument = argument.to_ascii_lowercase().replace('_', "-");
    MANAGED_NATIVE_ARGUMENTS.iter().any(|managed| {
        argument == *managed
            || argument
                .strip_prefix(managed)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn invalid_native_argument(arguments: &[String]) -> Option<&str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if !ALLOWED_VALUE_NATIVE_ARGUMENTS.contains(&argument.as_str()) {
            return Some(argument);
        }
        let Some(value) = arguments.get(index + 1) else {
            return Some(argument);
        };
        if value.is_empty() || value.starts_with("--") {
            return Some(value);
        }
        index += 2;
    }
    None
}

fn conflicts_with_managed_environment(name: &str) -> bool {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn managed_environment_removals() -> Vec<OsString> {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .map(OsString::from)
        .collect()
}

fn command_detail(stdout: &str, stderr: &str) -> String {
    match (stdout.trim(), stderr.trim()) {
        ("", "") => "no output".to_owned(),
        (stdout, "") => stdout.to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn invalid_probe(reason: String) -> EngineProbe {
    EngineProbe {
        installation: InstallationState::Invalid {
            reason: reason.clone(),
        },
        update: UpdateState::Unknown,
        healthy: false,
        detail: reason,
    }
}

fn http_endpoint(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{}:{}", address.ip(), address.port())
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_progress_is_conservative_and_never_invents_fractions() {
        assert_eq!(parse_ninfer_startup_progress(&[]), None);
        assert_eq!(
            parse_ninfer_startup_progress(&["unrelated log line".to_owned()]),
            None
        );

        let loading = parse_ninfer_startup_progress(&["loading model weights".to_owned()])
            .expect("loading line yields a phase");
        assert_eq!(loading.phase, BackendLoadPhase::LoadingModel);
        assert_eq!(loading.fraction, None);

        let context = parse_ninfer_startup_progress(&["allocating kv cache".to_owned()])
            .expect("kv line yields a phase");
        assert_eq!(context.phase, BackendLoadPhase::AllocatingContext);

        let listening = parse_ninfer_startup_progress(&[
            "loading model weights".to_owned(),
            "allocating kv cache".to_owned(),
            "ninfer-serve ready on 127.0.0.1:8080".to_owned(),
        ])
        .expect("ready line yields the latest phase");
        assert_eq!(listening.phase, BackendLoadPhase::VerifyingStartup);
    }

    #[test]
    fn native_arguments_are_strict_and_semantic_flags_are_reserved() {
        assert!(
            invalid_native_argument(&["--max-pending-requests".to_owned(), "12".to_owned(),])
                .is_none()
        );
        assert!(invalid_native_argument(&["--unknown".to_owned()]).is_some());
        assert!(conflicts_with_managed_argument("--temperature=0.8"));
        assert!(conflicts_with_managed_argument("--no-thinking"));
    }

    #[test]
    fn exact_help_contract_gates_runtime_and_native_capabilities() {
        let help = "Usage: ninfer-serve <model.ninfer> --host HOST --port PORT --model-id ID --device N --request-log-jsonl FILE Responses/Chat --max-pending-requests N";
        assert!(help_contract_error(help, &[]).is_none());
        assert!(
            help_contract_error(help, &["--max-pending-requests".to_owned(), "8".to_owned()])
                .is_none()
        );
        assert!(help_contract_error(help, &["--not-advertised".to_owned()]).is_some());
        assert!(
            help_contract_error("Usage: ninfer-serve <model.ninfer> --host HOST", &[]).is_some()
        );
    }

    #[test]
    fn package_compatibility_is_capability_driven_and_current_sharp_is_precise() {
        let current = evaluate_ninfer_package_runtime(NinferPackageRuntimeCapabilities {
            trustworthy_identity: true,
            external_sharp: false,
            process_sampler_overrides: true,
            bounded_server_start: true,
        });
        assert!(matches!(
            current,
            RuntimeCompatibility::Incompatible(ref reason)
                if reason.contains("required external Sharp application is unsupported/unproven")
                    && reason.contains("remain unproven until server_start")
        ));
        let future = evaluate_ninfer_package_runtime(NinferPackageRuntimeCapabilities {
            trustworthy_identity: true,
            external_sharp: true,
            process_sampler_overrides: true,
            bounded_server_start: true,
        });
        assert!(matches!(future, RuntimeCompatibility::NeedsAttention(_)));
        assert!(
            validate_ninfer_package_prelaunch(NinferPackageRuntimeCapabilities {
                trustworthy_identity: true,
                external_sharp: true,
                process_sampler_overrides: true,
                bounded_server_start: true,
            })
            .is_ok()
        );

        let untrusted = NinferPackageRuntimeCapabilities {
            trustworthy_identity: false,
            external_sharp: false,
            process_sampler_overrides: false,
            bounded_server_start: false,
        };
        let untrusted_compatibility = evaluate_ninfer_package_runtime(untrusted);
        assert!(matches!(
            untrusted_compatibility,
            RuntimeCompatibility::Incompatible(ref reason)
                if reason.contains("no trustworthy package-capability observation")
        ));
        assert!(!untrusted_compatibility.is_usable());
        assert!(validate_ninfer_package_prelaunch(untrusted).is_err());
    }

    #[test]
    fn package_sampler_arguments_include_top_k_and_min_p() {
        let package = ninfer_package_fixture();
        assert_eq!(
            ninfer_package_sampler_arguments(&package)
                .expect("package sampler")
                .into_iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "--temperature",
                "1",
                "--top-p",
                "0.95",
                "--top-k",
                "20",
                "--min-p",
                "0.05",
            ]
        );
    }

    #[test]
    fn package_startup_requirements_preserve_semantic_cache_and_cli_dtype() {
        let requirements = ninfer_startup_requirements(
            &ninfer_package_fixture(),
            &ResolvedLoadSettings::default(),
        )
        .expect("package startup requirements");

        assert_eq!(requirements.kv_cache, "int8-group64");
        assert_eq!(requirements.kv_dtype, "int8");
    }

    #[test]
    fn current_upstream_target_registry_is_enumerated_exactly() {
        let fixture = tempfile::tempdir().expect("source registry fixture");
        write_registry_target(
            fixture.path(),
            "qwen3_6_27b",
            concat!(
                "static constexpr std::string_view model_id = \"qwen3.6-27b\";\n",
                "static constexpr std::string_view target_key = \"qwen3_6_27b\";\n",
                "static constexpr std::string_view qwen3_8_model_id = \"qwen3.8-27b\";\n",
            ),
            concat!(
                "if (identity.model_id == model_id && identity.weights_id == \"groupwise-int\") {\n",
                "if (identity.model_id == qwen3_8_model_id && identity.weights_id == \"groupwise-int\") {\n",
                "if (identity.model_id == model_id && identity.weights_id == \"nvfp4\") {\n",
                "if (identity.model_id == qwen3_8_model_id && identity.weights_id == \"nvfp4\") {\n",
            ),
        );
        write_registry_target(
            fixture.path(),
            "qwen3_6_35b_a3b",
            "static constexpr std::string_view model_id = \"qwen3.6-35b-a3b\";\n",
            "if (identity.model_id == model_id && identity.weights_id == \"groupwise-int\") {\n",
        );
        let identities = inspect_source_native_identities(fixture.path());
        let observed = identities
            .into_iter()
            .map(|identity| match identity {
                ArtifactNativeIdentity::Ninfer(identity) => {
                    format!("{}/{}", identity.model_id, identity.weights_id)
                }
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            observed,
            BTreeSet::from([
                "qwen3.6-27b/groupwise-int".to_owned(),
                "qwen3.6-27b/nvfp4".to_owned(),
                "qwen3.6-35b-a3b/groupwise-int".to_owned(),
                "qwen3.8-27b/groupwise-int".to_owned(),
                "qwen3.8-27b/nvfp4".to_owned(),
            ])
        );
    }

    #[test]
    fn changed_target_registry_degrades_to_truthful_uncertainty() {
        let fixture = tempfile::tempdir().expect("source registry fixture");
        write_registry_target(
            fixture.path(),
            "changed",
            "static constexpr std::string_view model_id = \"changed\";\n",
            "if (identity.model_id == renamed_expression()) {\n",
        );
        assert!(inspect_source_native_identities(fixture.path()).is_empty());
    }

    #[test]
    fn exact_supported_gpu_beats_a_larger_unsupported_gpu_and_is_isolated_as_device_zero() {
        let supported_uuid = "GPU-11111111-2222-3333-4444-555555555555";
        let host = HostCapabilities {
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerators: vec![
                AcceleratorDevice {
                    accelerator: "cuda".to_owned(),
                    stable_id: Some("GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned()),
                    name: Some("NVIDIA RTX PRO 6000 Blackwell".to_owned()),
                    vram_bytes: Some(96 * 1024 * 1024 * 1024),
                    driver_version: Some("600".to_owned()),
                    compute_capability: Some(norted_core::ComputeCapability::new(12, 0)),
                },
                AcceleratorDevice {
                    accelerator: "cuda".to_owned(),
                    stable_id: Some(supported_uuid.to_owned()),
                    name: Some("NVIDIA GeForce RTX 5090".to_owned()),
                    vram_bytes: Some(32 * 1024 * 1024 * 1024),
                    driver_version: Some("600".to_owned()),
                    compute_capability: Some(norted_core::ComputeCapability::new(12, 0)),
                },
            ],
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        let requirements = RuntimeRequirements {
            requires_nvidia_gpu: true,
            supported_cuda_compute_capabilities: vec![norted_core::ComputeCapability::new(12, 0)],
            required_nvidia_device_names: vec!["NVIDIA GeForce RTX 5090".to_owned()],
            ..RuntimeRequirements::default()
        };
        let evaluation = ninfer_device_evaluation(
            &host.platform,
            &host.architecture,
            &requirements,
            &host,
            false,
        );
        assert_eq!(evaluation.compatibility, RuntimeCompatibility::Recommended);
        let accelerator = evaluation.accelerator.expect("selected accelerator");
        assert_eq!(accelerator.stable_id.as_deref(), Some(supported_uuid));
        let environment = isolated_cuda_environment(&BTreeMap::new(), &accelerator, "NInfer")
            .expect("isolated environment");
        assert_eq!(
            environment.get("CUDA_VISIBLE_DEVICES").map(String::as_str),
            Some(supported_uuid)
        );
        let arguments = managed_launch_arguments(
            Path::new("model.ninfer"),
            "127.0.0.1:1234".parse().expect("address"),
            &ModelId("model".to_owned()),
            Path::new("startup.jsonl"),
        );
        let device = arguments
            .iter()
            .position(|argument| argument == "--device")
            .expect("device option");
        assert_eq!(arguments[device + 1], "0");
    }

    #[test]
    fn unknown_exact_gpu_name_remains_needs_attention() {
        let host = HostCapabilities {
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerators: vec![AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: Some("GPU-11111111-2222-3333-4444-555555555555".to_owned()),
                name: None,
                vram_bytes: Some(32 * 1024 * 1024 * 1024),
                driver_version: Some("600".to_owned()),
                compute_capability: Some(norted_core::ComputeCapability::new(12, 0)),
            }],
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        let requirements = RuntimeRequirements {
            requires_nvidia_gpu: true,
            supported_cuda_compute_capabilities: vec![norted_core::ComputeCapability::new(12, 0)],
            required_nvidia_device_names: vec!["NVIDIA GeForce RTX 5090".to_owned()],
            ..RuntimeRequirements::default()
        };
        assert!(matches!(
            ninfer_device_evaluation(
                &host.platform,
                &host.architecture,
                &requirements,
                &host,
                false,
            )
            .compatibility,
            RuntimeCompatibility::NeedsAttention(_)
        ));
    }

    #[tokio::test]
    async fn startup_observation_supplies_exact_effective_sampler_defaults() {
        let temporary = tempfile::NamedTempFile::new().expect("startup log fixture");
        let record = startup_record_fixture(false, false);
        std::fs::write(
            temporary.path(),
            format!("{}\n", serde_json::to_string(&record).expect("JSON")),
        )
        .expect("write startup log");
        let pending = startup_pending_fixture(temporary.path());
        let defaults = read_and_validate_startup_log(&pending)
            .await
            .expect("startup defaults")
            .expect("server_start record");
        assert_eq!(
            defaults,
            EffectiveGenerationSettings {
                temperature: 0.42,
                top_p: 0.73,
            }
        );
    }

    #[tokio::test]
    async fn startup_observation_waits_for_the_exact_server_start_record() {
        let temporary = tempfile::NamedTempFile::new().expect("startup log fixture");
        std::fs::write(temporary.path(), b"").expect("empty startup log");
        assert!(
            read_and_validate_startup_log(&startup_pending_fixture(temporary.path()))
                .await
                .expect("pending startup observation")
                .is_none()
        );
    }

    #[tokio::test]
    async fn package_startup_observation_proves_context_capacity_and_policy() {
        let temporary = tempfile::NamedTempFile::new().expect("startup log fixture");
        let mut record = startup_record_fixture(false, false);
        record["server"]["default_thinking"] = json!(true);
        record["sampling_defaults"]["server_overrides"] = json!({
            "temperature": 1.0,
            "top_p": 0.95,
            "top_k": 20,
            "min_p": 0.05
        });
        std::fs::write(
            temporary.path(),
            format!("{}\n", serde_json::to_string(&record).expect("JSON")),
        )
        .expect("write startup log");
        let mut pending = startup_pending_fixture(temporary.path());
        pending.package_requirements = Some(NinferStartupRequirements {
            minimum_context_tokens: 200_000,
            kv_cache: "int8-group64".to_owned(),
            kv_dtype: "int8".to_owned(),
            cuda_graph: true,
            prefix_reuse: true,
            speculative_backend: "none".to_owned(),
            speculative_draft_window: 0,
            proposal_head: "full".to_owned(),
            temperature: 1.0,
            top_p: 0.95,
            top_k: 20,
            min_p: 0.05,
        });
        assert!(
            read_and_validate_startup_log(&pending)
                .await
                .expect("validated package startup")
                .is_some()
        );

        record["engine"]["kv_capacity"] = json!(199_999);
        std::fs::write(
            temporary.path(),
            format!("{}\n", serde_json::to_string(&record).expect("JSON")),
        )
        .expect("write insufficient startup log");
        assert!(read_and_validate_startup_log(&pending).await.is_err());
    }

    #[test]
    fn request_sampler_ranges_are_validated_without_clamping() {
        let adapter = NinferAdapter::from_config(None, Path::new("."));
        assert!(
            adapter
                .validate_generation_settings(
                    &GenerationSettingsPatch {
                        temperature: Some(2.0),
                        top_p: Some(1.0),
                    },
                    &EffectiveGenerationSettings {
                        temperature: 0.5,
                        top_p: 0.9,
                    },
                )
                .is_ok()
        );
        assert!(
            adapter
                .validate_generation_settings(
                    &GenerationSettingsPatch {
                        temperature: Some(2.01),
                        top_p: None,
                    },
                    &EffectiveGenerationSettings {
                        temperature: 0.5,
                        top_p: 0.9,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn relative_external_binary_paths_resolve_without_invented_source_identity() {
        let config: EngineConfig = serde_json::from_value(json!({
            "enabled": true,
            "settings": {"binary_path": "tools/ninfer-serve"}
        }))
        .expect("external NInfer config fixture");
        let adapter = NinferAdapter::from_config(Some(&config), Path::new("/config"));
        assert_eq!(
            adapter.binary_path.as_deref(),
            Some(Path::new("/config/tools/ninfer-serve"))
        );
        assert!(adapter.configuration_error.is_none());
    }

    #[tokio::test]
    async fn startup_observation_rejects_greedy_or_wrong_artifact_identity() {
        let greedy = tempfile::NamedTempFile::new().expect("startup log fixture");
        std::fs::write(
            greedy.path(),
            format!(
                "{}\n",
                serde_json::to_string(&startup_record_fixture(true, false)).expect("JSON")
            ),
        )
        .expect("write startup log");
        assert!(
            read_and_validate_startup_log(&startup_pending_fixture(greedy.path()))
                .await
                .is_err()
        );

        let wrong = tempfile::NamedTempFile::new().expect("startup log fixture");
        std::fs::write(
            wrong.path(),
            format!(
                "{}\n",
                serde_json::to_string(&startup_record_fixture(false, true)).expect("JSON")
            ),
        )
        .expect("write startup log");
        assert!(
            read_and_validate_startup_log(&startup_pending_fixture(wrong.path()))
                .await
                .is_err()
        );
    }

    fn startup_pending_fixture(path: &Path) -> PendingStartup {
        PendingStartup {
            request_log_path: path.to_path_buf(),
            native_identity: NinferArtifactIdentity {
                container_version: 2,
                model_id: "qwen3.6-27b".to_owned(),
                weights_id: "groupwise-int".to_owned(),
            },
            public_model_id: ModelId("stable-model-id".to_owned()),
            accelerator: AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: Some("GPU-11111111-2222-3333-4444-555555555555".to_owned()),
                name: Some("NVIDIA GeForce RTX 5090".to_owned()),
                vram_bytes: Some(32 * 1024 * 1024 * 1024),
                driver_version: Some("600".to_owned()),
                compute_capability: Some(norted_core::ComputeCapability::new(12, 0)),
            },
            package_requirements: None,
        }
    }

    fn startup_record_fixture(greedy: bool, wrong_identity: bool) -> serde_json::Value {
        let model_id = if wrong_identity {
            "different-model"
        } else {
            "qwen3.6-27b"
        };
        json!({
            "artifact_type": "ninfer_serve_request_log",
            "schema_version": 18,
            "event": "server_start",
            "server": {
                "public_model_id": "stable-model-id",
                "default_thinking": false
            },
            "artifact": {
                "target": model_id,
                "weights_id": "groupwise-int"
            },
            "engine": {
                "max_context": 262144,
                "kv_capacity_mode": "auto",
                "kv_capacity": 262144,
                "kv_cache": "int8-group64",
                "cuda_graph": true,
                "prefix_reuse": true,
                "speculative_backend": "none",
                "speculative_draft_window": 0,
                "proposal_head": "full",
                "context_cost": {
                    "model_id": model_id,
                    "weights_id": "groupwise-int"
                }
            },
            "sampling_defaults": {
                "thinking": {"temperature": 0.6, "top_p": 0.95, "top_k": 20, "min_p": 0.05},
                "non_thinking": {"temperature": 0.25, "top_p": 0.73, "top_k": 20, "min_p": 0.05},
                "server_overrides": {
                    "temperature": 0.42,
                    "top_p": null,
                    "top_k": null,
                    "min_p": null
                },
                "greedy": greedy
            },
            "environment": {
                "gpu_name": "NVIDIA GeForce RTX 5090",
                "gpu_uuid": "GPU-11111111-2222-3333-4444-555555555555",
                "compute_capability_major": 12,
                "compute_capability_minor": 0
            }
        })
    }

    fn ninfer_package_fixture() -> NortedPackageBinding {
        let benchmark_profiles = BTreeMap::from([
            (
                "mtp0".to_owned(),
                norted_core::NinferBenchmarkProfile {
                    speculative_decoding: false,
                    backend: None,
                    draft_tokens: None,
                    lm_head_draft: false,
                },
            ),
            (
                "mtp3".to_owned(),
                norted_core::NinferBenchmarkProfile {
                    speculative_decoding: true,
                    backend: Some("mtp".to_owned()),
                    draft_tokens: Some(3),
                    lm_head_draft: true,
                },
            ),
        ]);
        NortedPackageBinding {
            kind: norted_core::NortedPackageKind::Ninfer,
            manifest_schema: "norted.ninfer-manifest".to_owned(),
            manifest_version: 1,
            package_root: PathBuf::new(),
            manifest_path: PathBuf::new(),
            manifest_sha256: "00".repeat(32),
            output_key: "groupwise-int".to_owned(),
            expected_primary_size: 1,
            expected_primary_sha256: "11".repeat(32),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: None,
            runtime_policy: None,
            runtime_policy_id: Some("dirk".to_owned()),
            runtime_policy_profile: None,
            sharp: None,
            sharp_revision: None,
            sharp_version: None,
            tokenizer: None,
            projector: None,
            policy: NortedPackagePolicy::Ninfer(NinferPackagePolicy {
                policy_id: "dirk".to_owned(),
                temperature: 1.0,
                top_p: 0.95,
                top_k: 20,
                min_p: 0.05,
                thinking_enabled: true,
                cuda_graph_decode: true,
                compatible_prefix_reuse: true,
                text_only_default: true,
                kv_cache: "int8-group64".to_owned(),
                kv_dtype: "int8".to_owned(),
                minimum_context_tokens: 200_000,
                benchmark_profiles,
            }),
            allowed_user_overrides: vec!["temperature".to_owned(), "top_p".to_owned()],
            status: norted_core::NortedPackageStatus::NeedsRuntimeCapability {
                requirements: vec!["Sharp".to_owned()],
            },
        }
    }

    fn write_registry_target(root: &Path, target: &str, header: &str, package: &str) {
        let target = root.join("src").join("targets").join(target);
        let export = target.join("export").join("ninfer").join("targets");
        let implementation = target.join("impl");
        std::fs::create_dir_all(&export).expect("create export fixture");
        std::fs::create_dir_all(&implementation).expect("create implementation fixture");
        std::fs::write(export.join("package.h"), header).expect("write header fixture");
        std::fs::write(implementation.join("package.cpp"), package).expect("write package fixture");
    }
}
