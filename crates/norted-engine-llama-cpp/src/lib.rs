//! llama.cpp-specific launch, probe, health, and inference translation.

mod catalog;

pub use catalog::{LLAMA_CPP_RUNTIME_PROVIDER_ID, LlamaCppRuntimeCatalogProvider};

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use norted_core::{
    AcquisitionMethod, ArtifactFormat, AvailableRuntime, EngineConfig, EngineInstallation,
    EngineRevision, GpuOffload, HostCapabilities, InstalledRuntime, LoadSettingDefinition,
    LoadSettingId, LoadSettingKind, LoadSettingScope, LoadSettingValue, LoadSettingsSchema,
    ModelArtifact, RuntimeAcquisitionMethod, RuntimeCompatibility, RuntimeId,
    RuntimeProbeObservation,
};
use norted_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, InferenceEvent, InferenceFinishReason,
    InferenceMessage, InferenceOutput, InferenceRequest, InferenceRole, InferenceStream,
    InferenceUsage, InstallationState, LaunchRequest, LaunchSpec, LoadProgressReporter,
    NativeOption, OptionValueKind, PreparedModelInput, ProcessDescriptor, UpdateState,
    capture_command, common_load_setting_definitions, prepare_norted_package_input,
    prepare_norted_package_input_with_progress, revalidate_norted_package_before_launch,
    revalidate_norted_package_before_launch_with_progress,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const ENGINE_ID: &str = "llama.cpp";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/ggml-org/llama.cpp";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const PROPS_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;

// These variables select semantics that Norted must own for its private backend.
// Keep ordinary llama.cpp tuning variables inherited and available to users.
const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "LLAMA_ARG_MODEL",
    "LLAMA_ARG_MODEL_URL",
    "LLAMA_ARG_DOCKER_REPO",
    "LLAMA_ARG_HF_REPO",
    "LLAMA_ARG_HF_FILE",
    "LLAMA_ARG_ALIAS",
    "LLAMA_ARG_HOST",
    "LLAMA_ARG_PORT",
    "LLAMA_ARG_REUSE_PORT",
    "LLAMA_ARG_API_PREFIX",
    "LLAMA_API_KEY",
    "LLAMA_ARG_API_KEY_FILE",
    "LLAMA_ARG_SSL_KEY_FILE",
    "LLAMA_ARG_SSL_CERT_FILE",
    "LLAMA_ARG_EMBEDDINGS",
    "LLAMA_ARG_RERANKING",
    "LLAMA_ARG_MODELS_DIR",
    "LLAMA_ARG_MODELS_PRESET",
    "LLAMA_ARG_MODELS_MAX",
    "LLAMA_ARG_MODELS_AUTOLOAD",
    "LLAMA_ARG_NO_MODELS_AUTOLOAD",
    "LLAMA_ARG_ENDPOINT_PROPS",
    "LLAMA_SERVER_ROUTER_PORT",
    "LLAMA_SERVER_CHILD_MODE",
    "LLAMA_ARG_AGENT",
    "LLAMA_ARG_TOOLS",
    "LLAMA_ARG_MCP_CONFIG",
];

fn llama_profile_compatibility(
    artifact: CompatibilityDecision,
    model: &ModelArtifact,
    serve_profile: Option<&norted_core::ServeProfile>,
) -> RuntimeCompatibility {
    if let CompatibilityDecision::Unsupported { reason } = artifact {
        return RuntimeCompatibility::Incompatible(reason);
    }
    let Some(profile) = serve_profile else {
        return RuntimeCompatibility::Compatible;
    };
    if let Err(reason) = profile.basic_applicability(model) {
        return RuntimeCompatibility::Incompatible(reason);
    }
    if profile.requires_runtime_recipe() {
        RuntimeCompatibility::Incompatible(format!(
            "Serve Profile `{}` requires prompt/generation/strategy capabilities that the llama.cpp adapter does not yet implement; choose None/raw defaults or a load-only Serve Profile",
            profile.display_name
        ))
    } else {
        RuntimeCompatibility::Compatible
    }
}

const MANAGED_NATIVE_ARGUMENTS: &[&str] = &[
    "-m",
    "--model",
    "-mu",
    "--model-url",
    "-dr",
    "--docker-repo",
    "-hf",
    "-hfr",
    "--hf-repo",
    "-hff",
    "--hf-file",
    "-a",
    "--alias",
    "--host",
    "--port",
    "--reuse-port",
    "--api-prefix",
    "--api-key",
    "--api-key-file",
    "--ssl-key-file",
    "--ssl-cert-file",
    "--embedding",
    "--embeddings",
    "--rerank",
    "--reranking",
    "--models-dir",
    "--models-preset",
    "--models-max",
    "--models-autoload",
    "--no-models-autoload",
    "--props",
    "--agent",
    "--tools",
    "--tools-file",
    "--mcp-config",
    "--mcp-config-file",
    // Current llama-server convenience presets assign a different primary model
    // (and some also replace the managed port or generation mode).
    "--embd-gemma-default",
    "--fim-qwen-1.5b-default",
    "--fim-qwen-3b-default",
    "--fim-qwen-7b-default",
    "--fim-qwen-7b-spec",
    "--fim-qwen-14b-spec",
    "--fim-qwen-30b-default",
    "--gpt-oss-20b-default",
    "--gpt-oss-120b-default",
    "--vision-gemma-4b-default",
    "--vision-gemma-12b-default",
];

pub struct LlamaCppAdapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
}

impl LlamaCppAdapter {
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
                    "environment variable `{name}` conflicts with the Norted-managed llama.cpp backend contract"
                ));
            }
            for key in config.settings.keys() {
                if key != "binary_path" {
                    configuration_error = Some(format!(
                        "unsupported llama.cpp setting `{key}`; only `binary_path` is supported"
                    ));
                    break;
                }
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
                            Some("llama.cpp `binary_path` must be a non-empty string".to_owned());
                    }
                }
            }
            for key in config.native.keys() {
                if key != "arguments" {
                    configuration_error = Some(format!(
                        "unsupported llama.cpp native setting `{key}`; use `arguments = [...]`"
                    ));
                    break;
                }
            }
            if configuration_error.is_none()
                && let Some(value) = config.native.get("arguments")
            {
                match value.as_array() {
                    Some(values) => {
                        for value in values {
                            match value.as_str() {
                                Some(value) => native_arguments.push(value.to_owned()),
                                None => {
                                    configuration_error = Some(
                                        "llama.cpp native arguments must all be strings".to_owned(),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        configuration_error = Some(
                            "llama.cpp native `arguments` must be an array of strings".to_owned(),
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
                    "native argument `{argument}` conflicts with the Norted-managed llama.cpp backend contract"
                ));
            }
        }

        Self {
            enabled,
            binary_path,
            native_arguments,
            environment,
            configuration_error,
            client: reqwest::Client::new(),
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
        }
    }

    fn capability_key(runtime: &InstalledRuntime) -> String {
        format!(
            "{}:{}",
            runtime.manifest.runtime_id, runtime.manifest.entrypoint_sha256
        )
    }

    async fn cached_runtime_help(&self, runtime: &InstalledRuntime) -> Result<String, EngineError> {
        let key = Self::capability_key(runtime);
        if let Some(help) = self.capability_cache.read().await.get(&key).cloned() {
            return Ok(help);
        }
        let output = capture_command(
            &runtime.entrypoint_path(),
            &["--help"],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await?;
        if !output.success {
            return Err(EngineError::Operation(format!(
                "llama-server --help exited with {:?}: {}",
                output.code,
                command_detail(&output.stdout, &output.stderr)
            )));
        }
        let help = format!("{}\n{}", output.stdout, output.stderr);
        self.capability_cache
            .write()
            .await
            .insert(key, help.clone());
        Ok(help)
    }

    async fn probe_uncached(&self) -> EngineProbe {
        if let Some(error) = &self.configuration_error {
            return invalid_probe(error.clone());
        }
        if !self.enabled {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "llama.cpp adapter is disabled in configuration".to_owned(),
            };
        }
        let Some(configured_path) = &self.binary_path else {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "set [engine.\"llama.cpp\".settings].binary_path to an existing llama-server binary"
                    .to_owned(),
            };
        };
        let binary_path = match tokio::fs::canonicalize(configured_path).await {
            Ok(path) => path,
            Err(error) => {
                return invalid_probe(format!(
                    "configured llama-server binary could not be resolved: {error}"
                ));
            }
        };
        match tokio::fs::metadata(&binary_path).await {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return invalid_probe("configured llama-server path is not a file".to_owned()),
            Err(error) => {
                return invalid_probe(format!(
                    "configured llama-server binary could not be inspected: {error}"
                ));
            }
        }

        let version_output = match capture_command(
            &binary_path,
            &["--version"],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await
        {
            Ok(output) if output.success => output,
            Ok(output) => {
                return invalid_probe(format!(
                    "llama-server --version exited with {:?}: {}",
                    output.code,
                    command_detail(&output.stdout, &output.stderr)
                ));
            }
            Err(error) => return invalid_probe(error.to_string()),
        };
        let help_output = match capture_command(
            &binary_path,
            &["--help"],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await
        {
            Ok(output) if output.success => output,
            Ok(output) => {
                return invalid_probe(format!(
                    "llama-server --help exited with {:?}: {}",
                    output.code,
                    command_detail(&output.stdout, &output.stderr)
                ));
            }
            Err(error) => return invalid_probe(error.to_string()),
        };
        let help = format!("{}\n{}", help_output.stdout, help_output.stderr);
        for required in ["--model", "--alias", "--host", "--port"] {
            if !help.contains(required) {
                return invalid_probe(format!(
                    "configured binary does not advertise required llama-server flag `{required}`"
                ));
            }
        }
        let version_text = command_detail(&version_output.stdout, &version_output.stderr);
        let (version, revision) = parse_version(&version_text);
        let binary_sha256 = match hash_file(&binary_path).await {
            Ok(hash) => Some(hash),
            Err(error) => {
                return invalid_probe(format!(
                    "could not hash configured llama-server binary: {error}"
                ));
            }
        };
        EngineProbe {
            installation: InstallationState::Installed {
                installation: Box::new(EngineInstallation {
                    engine: EngineRevision {
                        engine_id: ENGINE_ID.to_owned(),
                        version,
                        revision,
                    },
                    source_repository: None,
                    acquisition_method: AcquisitionMethod::ExternalBinary,
                    binary_path,
                    binary_sha256,
                    build: None,
                    platform: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    runtime_variant: None,
                    acquired_at_unix: None,
                    observed_at_unix: unix_timestamp(),
                }),
            },
            update: UpdateState::Unknown,
            healthy: true,
            detail: format!(
                "external configured llama-server binary; {}",
                first_line(&version_text)
            ),
        }
    }

    fn backend_request(&self, request: &InferenceRequest, stream: bool) -> Value {
        let messages = backend_messages(&request.messages);
        let mut body = json!({
            "model": request.model_id.0,
            "messages": messages,
            "stream": stream,
        });
        if let Some(maximum) = request.max_output_tokens {
            body["max_completion_tokens"] = json!(maximum);
        }
        if let Some(temperature) = request.generation_settings.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(top_p) = request.generation_settings.top_p {
            body["top_p"] = json!(top_p);
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        body
    }
}

#[async_trait]
impl EngineAdapter for LlamaCppAdapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: ENGINE_ID.to_owned(),
            display_name: "llama.cpp".to_owned(),
            upstream_repository: UPSTREAM_REPOSITORY.to_owned(),
        }
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::Gguf],
            api: vec![ApiCapability::ChatCompletions],
            features: vec![EngineFeature::TextGeneration],
        }
    }

    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        _backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        if let Some(temperature) = settings.temperature
            && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp temperature must be finite and in the range 0..=2".to_owned(),
            ));
        }
        if let Some(top_p) = settings.top_p
            && (!top_p.is_finite() || !(0.0..=1.0).contains(&top_p))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp top_p must be finite and in the range 0..=1".to_owned(),
            ));
        }
        Ok(())
    }

    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        if !self.enabled {
            CompatibilityDecision::Unsupported {
                reason: "llama.cpp adapter is disabled in configuration".to_owned(),
            }
        } else if let Some(error) = &self.configuration_error {
            CompatibilityDecision::Unsupported {
                reason: error.clone(),
            }
        } else {
            CompatibilityDecision::Supported
        }
    }

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if let CompatibilityDecision::Unsupported { reason } =
            self.runtime_management_compatibility()
        {
            return CompatibilityDecision::Unsupported { reason };
        }
        if model.format == ArtifactFormat::Gguf {
            CompatibilityDecision::Supported
        } else {
            CompatibilityDecision::Unsupported {
                reason: format!(
                    "llama.cpp supports GGUF in this integration, not `{}`",
                    model.format.as_str()
                ),
            }
        }
    }

    fn runtime_model_compatibility(
        &self,
        _runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        serve_profile: Option<&norted_core::ServeProfile>,
    ) -> RuntimeCompatibility {
        llama_profile_compatibility(self.compatibility(model), model, serve_profile)
    }

    fn available_runtime_model_compatibility(
        &self,
        _runtime: &AvailableRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        serve_profile: Option<&norted_core::ServeProfile>,
    ) -> RuntimeCompatibility {
        llama_profile_compatibility(self.compatibility(model), model, serve_profile)
    }

    fn native_options(&self) -> Vec<NativeOption> {
        vec![NativeOption {
            name: "arguments".to_owned(),
            description: "Arguments appended exactly as configured after managed bind/model flags"
                .to_owned(),
            value_kind: OptionValueKind::String,
            repeatable: true,
        }]
    }

    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        prepare_norted_package_input(model).await
    }

    async fn prepare_model_input_with_progress(
        &self,
        model: &ModelArtifact,
        progress: LoadProgressReporter,
    ) -> Result<PreparedModelInput, EngineError> {
        prepare_norted_package_input_with_progress(model, &progress).await
    }

    fn load_setting_definitions(&self) -> Vec<LoadSettingDefinition> {
        llama_load_setting_definitions()
    }

    async fn load_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
        _serve_profile: Option<&norted_core::ServeProfile>,
    ) -> Result<LoadSettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self.cached_runtime_help(runtime).await?;
        let mut definitions = llama_load_setting_definitions();
        for definition in &mut definitions {
            let requirements = llama_setting_contract(definition.id.as_str());
            let missing = requirements
                .iter()
                .copied()
                .filter(|required| !help_has_option(&help, required))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                definition.supported = false;
                definition.unsupported_reason = Some(format!(
                    "the exact llama-server does not advertise {}",
                    missing.join(", ")
                ));
                continue;
            }
            let contract = requirements
                .first()
                .map(|option| help_option_context(&help, option))
                .unwrap_or_default();
            match definition.id.as_str() {
                "llama.cpp.flash_attention"
                    if !["auto", "on", "off"]
                        .into_iter()
                        .all(|choice| text_has_value(&contract, choice)) =>
                {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise the on/off/auto Flash Attention contract"
                            .to_owned(),
                    );
                }
                "llama.cpp.gpu_offload"
                    if !["auto", "all"]
                        .into_iter()
                        .all(|choice| text_has_value(&contract, choice)) =>
                {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise auto/all GPU-layer semantics"
                            .to_owned(),
                    );
                }
                "llama.cpp.kv_cache_k" | "llama.cpp.kv_cache_v" => {
                    let choices = llama_cache_types()
                        .into_iter()
                        .filter(|choice| text_has_value(&contract, choice))
                        .collect::<Vec<_>>();
                    if choices.is_empty() {
                        definition.supported = false;
                        definition.unsupported_reason = Some(
                            "the exact llama-server does not advertise any Norted-understood cache types"
                                .to_owned(),
                        );
                    } else {
                        definition.kind = LoadSettingKind::Choice { choices };
                    }
                }
                "llama.cpp.load_mode" => {
                    let choices = advertised_llama_load_modes(&help);
                    if choices.is_empty() {
                        definition.supported = false;
                        definition.unsupported_reason = Some(
                            "the exact llama-server does not advertise any Norted-understood load modes"
                                .to_owned(),
                        );
                    } else {
                        definition.kind = LoadSettingKind::Choice { choices };
                    }
                }
                _ => {}
            }
        }
        Ok(LoadSettingsSchema {
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
        if let Some(error) = &self.configuration_error {
            return Err(EngineError::InvalidConfiguration(error.clone()));
        }
        if !self.enabled {
            return Err(EngineError::InvalidConfiguration(
                "llama.cpp adapter is disabled in configuration".to_owned(),
            ));
        }
        if runtime.manifest.identity.engine_id != ENGINE_ID {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime `{}` belongs to engine `{}`, not `{ENGINE_ID}`",
                runtime.manifest.runtime_id, runtime.manifest.identity.engine_id
            )));
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
        let configured_path = runtime.entrypoint_path();
        let binary_path = tokio::fs::canonicalize(&configured_path)
            .await
            .map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "runtime entrypoint {} could not be resolved: {error}",
                    configured_path.display()
                ))
            })?;
        let metadata = tokio::fs::metadata(&binary_path).await.map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "runtime entrypoint {} could not be inspected: {error}",
                binary_path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(EngineError::InvalidConfiguration(
                "runtime entrypoint is not a regular file".to_owned(),
            ));
        }
        if !matches!(
            runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        ) {
            let root = tokio::fs::canonicalize(&runtime.installation_root)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime installation root could not be resolved: {error}"
                    ))
                })?;
            if !binary_path.starts_with(&root) {
                return Err(EngineError::InvalidConfiguration(
                    "managed runtime entrypoint escapes its installation root".to_owned(),
                ));
            }
        }
        let version_output = capture_command(
            &binary_path,
            &["--version"],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await?;
        if !version_output.success {
            return Err(EngineError::Operation(format!(
                "llama-server --version exited with {:?}: {}",
                version_output.code,
                command_detail(&version_output.stdout, &version_output.stderr)
            )));
        }
        let help = self.cached_runtime_help(runtime).await?;
        for required in ["--model", "--alias", "--host", "--port"] {
            if !help.contains(required) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "runtime entrypoint does not advertise required llama-server flag `{required}`"
                )));
            }
        }
        let entrypoint_sha256 = hash_file(&binary_path).await.map_err(|error| {
            EngineError::Operation(format!("could not hash entrypoint: {error}"))
        })?;
        if entrypoint_sha256 != runtime.manifest.entrypoint_sha256 {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime entrypoint SHA-256 mismatch: expected {}, observed {entrypoint_sha256}",
                runtime.manifest.entrypoint_sha256
            )));
        }
        let version_text = command_detail(&version_output.stdout, &version_output.stderr);
        let (version, revision) = parse_version(&version_text);
        if !matches!(
            runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        ) {
            if let Some(build) = runtime.manifest.identity.version.strip_prefix('b')
                && build.bytes().all(|byte| byte.is_ascii_digit())
                && !version_text.contains(&format!("build {build}"))
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "runtime reports a different llama.cpp build than managed identity {}",
                    runtime.manifest.identity.version
                )));
            }
            if let (Some(expected), Some(observed)) = (
                runtime.manifest.identity.upstream_revision.as_deref(),
                revision.as_deref(),
            ) && !expected.starts_with(observed)
                && !observed.starts_with(expected)
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "runtime revision `{observed}` does not match package revision `{expected}`"
                )));
            }
        }
        Ok(RuntimeProbeObservation {
            compatible: true,
            observed_engine_id: ENGINE_ID.to_owned(),
            observed_version: version,
            observed_revision: revision,
            detail: first_line(&version_text).to_owned(),
            observed_at_unix: unix_timestamp(),
        })
    }

    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "llama.cpp backend address must be loopback".to_owned(),
            ));
        }
        request
            .load_settings_schema
            .validate(&request.load_settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        let structured = translate_llama_load_settings(
            &request.load_settings,
            &self.native_arguments,
            &self.environment,
        )?;
        let observation = self.probe_runtime(&request.runtime).await?;
        let binary_path = request.runtime.entrypoint_path();
        let manifest = &request.runtime.manifest;
        let installation = Box::new(EngineInstallation {
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
        });
        let arguments = vec![
            OsString::from("--model"),
            request.model.primary.path.as_os_str().to_owned(),
            OsString::from("--alias"),
            OsString::from(request.model.primary.id.0.clone()),
            OsString::from("--host"),
            OsString::from(request.backend_address.ip().to_string()),
            OsString::from("--port"),
            OsString::from(request.backend_address.port().to_string()),
        ]
        .into_iter()
        .chain(structured.arguments)
        .chain(self.native_arguments.iter().map(OsString::from))
        .collect();
        let mut environment_remove = managed_environment_removals();
        environment_remove.extend(structured.environment_remove);
        Ok(LaunchSpec {
            executable: binary_path,
            arguments,
            environment: self.environment.clone(),
            environment_remove,
            inherits_parent_environment: true,
            working_directory: None,
            temporary_files: Vec::new(),
            endpoint: Some(http_endpoint(request.backend_address)),
            normalized_settings: BTreeMap::new(),
            load_settings: request.load_settings,
            native_arguments: self.native_arguments.clone(),
            installation: (*installation).clone(),
            runtime: request.runtime,
            model: request.model,
            accelerator: request.accelerator,
            serve_profile: request.serve_profile,
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
            EngineError::Operation("llama.cpp process has no backend endpoint".to_owned())
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
                "health endpoint returned HTTP {}",
                response.status()
            )));
        }
        let health = response
            .json::<HealthResponse>()
            .await
            .map_err(|error| EngineError::Operation(format!("invalid health response: {error}")))?;
        Ok(health.status == "ok")
    }

    fn startup_progress(&self, stderr_tail: &[String]) -> Option<BackendLoadProgress> {
        parse_llama_startup_progress(stderr_tail)
    }

    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("llama.cpp process has no backend endpoint".to_owned())
        })?;
        let response = self
            .client
            .get(format!("{endpoint}/props"))
            .timeout(PROPS_TIMEOUT)
            .send()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(backend_http_error(status, &body));
        }
        let props: PropsResponse = serde_json::from_slice(&body).map_err(|error| {
            EngineError::Operation(format!(
                "invalid llama.cpp /props effective generation settings: {error}"
            ))
        })?;
        let params = props.default_generation_settings.params;
        if !params.temperature.is_finite()
            || !(0.0..=2.0).contains(&params.temperature)
            || !params.top_p.is_finite()
            || !(0.0..=1.0).contains(&params.top_p)
        {
            return Err(EngineError::Operation(
                "llama.cpp /props returned effective generation settings outside the supported Response ranges (temperature 0..=2, top_p 0..=1)".to_owned(),
            ));
        }
        Ok(EffectiveGenerationSettings {
            temperature: params.temperature,
            top_p: params.top_p,
        })
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
            .json(&self.backend_request(&request, false))
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(backend_http_error(status, &body));
        }
        let response: ChatCompletionResponse = serde_json::from_slice(&body).map_err(|error| {
            EngineError::Operation(format!("invalid llama.cpp completion response: {error}"))
        })?;
        let choice = response.choices.into_iter().next().ok_or_else(|| {
            EngineError::Operation("llama.cpp response contained no completion choice".to_owned())
        })?;
        let finish_reason = map_finish_reason(choice.finish_reason.as_deref())?;
        let text = choice.message.content.ok_or_else(|| {
            EngineError::Operation("llama.cpp response contained no assistant text".to_owned())
        })?;
        Ok(InferenceOutput {
            text,
            usage: response.usage.map(Into::into),
            finish_reason,
        })
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
            .json(&self.backend_request(&request, true))
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
            return Err(backend_http_error(status, &body));
        }
        Ok(llama_sse_stream(response.bytes_stream().boxed()))
    }
}

#[derive(Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Deserialize)]
struct PropsResponse {
    default_generation_settings: DefaultGenerationSettings,
}

#[derive(Deserialize)]
struct DefaultGenerationSettings {
    params: PropsGenerationParams,
}

#[derive(Deserialize)]
struct PropsGenerationParams {
    temperature: f64,
    top_p: f64,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_tokens_details: Option<PromptTokenDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptTokenDetails {
    cached_tokens: Option<u64>,
}

impl From<ChatUsage> for InferenceUsage {
    fn from(usage: ChatUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens),
            cache_write_input_tokens: None,
            reasoning_output_tokens: None,
        }
    }
}

struct SseState {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: Option<InferenceFinishReason>,
    finished: bool,
}

fn llama_sse_stream(source: BoxStream<'static, Result<Bytes, reqwest::Error>>) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
        finished: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.queued.pop_front() {
                return Some((event, state));
            }
            if state.finished {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    parse_sse_frames(&mut state);
                    if !state.finished && state.buffer.len() > SSE_FRAME_LIMIT {
                        state.buffer.clear();
                        state.queued.push_back(Err(EngineError::Operation(
                            "llama.cpp SSE frame exceeded the local size limit".to_owned(),
                        )));
                        state.finished = true;
                    }
                }
                Some(Err(error)) => {
                    state.finished = true;
                    return Some((Err(map_transport_error(error)), state));
                }
                None => {
                    state.finished = true;
                    return Some((
                        Err(EngineError::BackendUnavailable(
                            "llama.cpp stream ended before the [DONE] marker".to_owned(),
                        )),
                        state,
                    ));
                }
            }
        }
    }))
}

fn parse_sse_frames(state: &mut SseState) {
    while let Some((boundary, boundary_length)) = find_sse_boundary(&state.buffer) {
        let frame = state.buffer.drain(..boundary).collect::<Vec<_>>();
        state.buffer.drain(..boundary_length);
        let frame = String::from_utf8_lossy(&frame);
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            match state.finish_reason.take() {
                Some(finish_reason) => {
                    state.queued.push_back(Ok(InferenceEvent::Completed {
                        usage: state.usage.take(),
                        finish_reason,
                    }));
                }
                None => state.queued.push_back(Err(EngineError::BackendUnavailable(
                    "llama.cpp stream reached [DONE] without a terminal finish reason".to_owned(),
                ))),
            }
            state.finished = true;
            return;
        }
        let value: Value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(error) => {
                state.queued.push_back(Err(EngineError::Operation(format!(
                    "invalid llama.cpp SSE payload: {error}"
                ))));
                state.finished = true;
                return;
            }
        };
        if let Some(error) = value.get("error") {
            state.queued.push_back(Err(EngineError::Operation(format!(
                "llama.cpp streaming error: {}",
                backend_error_message(error)
            ))));
            state.finished = true;
            return;
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            match serde_json::from_value::<ChatUsage>(usage.clone()) {
                Ok(usage) => state.usage = Some(usage.into()),
                Err(error) => {
                    state.queued.push_back(Err(EngineError::Operation(format!(
                        "invalid llama.cpp streaming usage: {error}"
                    ))));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(finish_reason) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("finish_reason"))
            .filter(|reason| !reason.is_null())
        {
            let Some(finish_reason) = finish_reason.as_str() else {
                state.queued.push_back(Err(EngineError::Operation(
                    "llama.cpp stream returned a non-string finish reason".to_owned(),
                )));
                state.finished = true;
                return;
            };
            match map_finish_reason(Some(finish_reason)) {
                Ok(finish_reason) => state.finish_reason = Some(finish_reason),
                Err(error) => {
                    state.queued.push_back(Err(error));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(delta) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            .filter(|delta| !delta.is_empty())
        {
            state.queued.push_back(Ok(InferenceEvent::TextDelta {
                delta: delta.to_owned(),
            }));
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    for index in 0..buffer.len().saturating_sub(1) {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if buffer[index..].starts_with(b"\n\n") {
            return Some((index, 2));
        }
    }
    None
}

fn backend_messages(messages: &[InferenceMessage]) -> Vec<Value> {
    let instructions = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                InferenceRole::System | InferenceRole::Developer
            )
        })
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut backend = Vec::with_capacity(messages.len());
    if !instructions.is_empty() {
        backend.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }
    backend.extend(
        messages
            .iter()
            .filter(|message| {
                matches!(message.role, InferenceRole::User | InferenceRole::Assistant)
            })
            .map(message_json),
    );
    backend
}

fn message_json(message: &InferenceMessage) -> Value {
    json!({
        "role": match message.role {
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
            InferenceRole::System | InferenceRole::Developer => "system",
        },
        "content": message.text,
    })
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

fn llama_load_setting_definitions() -> Vec<LoadSettingDefinition> {
    let mut definitions = common_load_setting_definitions();
    definitions.extend([
        llama_definition(
            "llama.cpp.threads",
            "CPU threads",
            "CPU threads used during token generation",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("runtime-selected"),
        ),
        llama_definition(
            "llama.cpp.batch_size",
            "Batch size",
            "Logical maximum prompt-processing batch size",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("runtime-selected"),
        ),
        llama_definition(
            "llama.cpp.micro_batch_size",
            "Micro-batch size",
            "Physical maximum prompt-processing batch size",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("runtime-selected"),
        ),
        llama_definition(
            "llama.cpp.gpu_offload",
            "GPU offload",
            "Weight layers placed in VRAM: none, auto, all, or an exact count",
            LoadSettingKind::GpuOffload,
            Some("auto in current runtimes"),
        ),
        llama_definition(
            "llama.cpp.flash_attention",
            "Flash attention",
            "Explicit llama.cpp Flash Attention mode",
            LoadSettingKind::Choice {
                choices: vec!["auto".to_owned(), "on".to_owned(), "off".to_owned()],
            },
            Some("auto in current runtimes"),
        ),
        llama_definition(
            "llama.cpp.kv_cache_k",
            "K-cache type",
            "Key cache storage type",
            LoadSettingKind::Choice {
                choices: llama_cache_types(),
            },
            Some("runtime-selected"),
        ),
        llama_definition(
            "llama.cpp.kv_cache_v",
            "V-cache type",
            "Value cache storage type",
            LoadSettingKind::Choice {
                choices: llama_cache_types(),
            },
            Some("runtime-selected"),
        ),
        llama_definition(
            "llama.cpp.load_mode",
            "Model load mode",
            "One unambiguous llama.cpp model-loading mode",
            LoadSettingKind::Choice {
                choices: llama_load_modes(),
            },
            Some("runtime-selected; omission preserves the exact runtime default"),
        ),
    ]);
    definitions
}

fn llama_definition(
    id: &str,
    label: &str,
    description: &str,
    kind: LoadSettingKind,
    upstream_default: Option<&str>,
) -> LoadSettingDefinition {
    LoadSettingDefinition {
        id: LoadSettingId::new(id).expect("static llama.cpp setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: LoadSettingScope::Engine {
            engine_id: ENGINE_ID.to_owned(),
        },
        supported: true,
        unsupported_reason: None,
        unit: None,
        upstream_default: upstream_default.map(str::to_owned),
        recommendation: None,
    }
}

fn llama_cache_types() -> Vec<String> {
    [
        "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn llama_load_modes() -> Vec<String> {
    ["auto", "none", "mmap", "mlock", "mmap+mlock", "dio"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn llama_setting_contract(id: &str) -> &'static [&'static str] {
    match id {
        "context_length" => &["--ctx-size"],
        "parallel_requests" => &["--parallel"],
        "llama.cpp.threads" => &["--threads"],
        "llama.cpp.batch_size" => &["--batch-size"],
        "llama.cpp.micro_batch_size" => &["--ubatch-size"],
        "llama.cpp.gpu_offload" => &["--n-gpu-layers"],
        "llama.cpp.flash_attention" => &["--flash-attn"],
        "llama.cpp.kv_cache_k" => &["--cache-type-k"],
        "llama.cpp.kv_cache_v" => &["--cache-type-v"],
        "llama.cpp.load_mode" => &["--load-mode"],
        _ => &[],
    }
}

fn help_has_option(help: &str, option: &str) -> bool {
    help.lines()
        .any(|line| help_line_has_option_header(line, option))
}

fn help_line_has_option_header(line: &str, option: &str) -> bool {
    line.match_indices(option).any(|(index, _)| {
        let prefix = &line[..index];
        let suffix = &line[index + option.len()..];
        let boundary_before = prefix
            .chars()
            .next_back()
            .is_none_or(|character| character.is_whitespace() || character == ',');
        let boundary_after = suffix
            .chars()
            .next()
            .is_none_or(|character| character.is_whitespace() || matches!(character, ',' | '='));
        let only_aliases_before = prefix
            .split(|character: char| character.is_whitespace() || character == ',')
            .filter(|token| !token.is_empty())
            .all(|token| token.starts_with('-'));
        boundary_before && boundary_after && only_aliases_before
    })
}

fn help_option_context(help: &str, option: &str) -> String {
    let lines = help.lines().collect::<Vec<_>>();
    let Some(index) = lines
        .iter()
        .position(|line| help_line_has_option_header(line, option))
    else {
        return String::new();
    };
    lines[index..lines.len().min(index + 4)].join(" ")
}

fn help_option_block(help: &str, option: &str) -> String {
    let lines = help.lines().collect::<Vec<_>>();
    let Some(start) = lines
        .iter()
        .position(|line| help_line_has_option_header(line, option))
    else {
        return String::new();
    };
    let start_indent = lines[start]
        .chars()
        .take_while(|character| character.is_whitespace())
        .count();
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            (!trimmed.is_empty() && indent <= start_indent && trimmed.starts_with('-'))
                .then_some(index)
        })
        .unwrap_or(lines.len());
    lines[start..end].join(" ")
}

fn advertised_llama_load_modes(help: &str) -> Vec<String> {
    let contract = help_option_block(help, "--load-mode");
    llama_load_modes()
        .into_iter()
        .filter(|choice| text_has_value(&contract, choice))
        .collect()
}

fn text_has_value(text: &str, expected: &str) -> bool {
    text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                ',' | '|' | '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | ':' | ';' | '\'' | '"'
            )
    })
    .any(|value| value == expected)
}

#[derive(Debug)]
struct LlamaStructuredArguments {
    arguments: Vec<OsString>,
    environment_remove: Vec<OsString>,
}

fn translate_llama_load_settings(
    settings: &norted_core::ResolvedLoadSettings,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<LlamaStructuredArguments, EngineError> {
    let mut arguments = Vec::new();
    let mut environment_remove = Vec::new();
    for (id, resolved) in &settings.effective {
        let (aliases, environment_names) = llama_setting_collision_contract(id.as_str());
        if let Some(argument) = find_native_option(native_arguments, aliases) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured load setting `{id}` conflicts with native llama.cpp argument `{argument}`"
            )));
        }
        if let Some(name) = configured_environment.keys().find(|name| {
            environment_names
                .iter()
                .any(|owned| name.eq_ignore_ascii_case(owned))
        }) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured load setting `{id}` conflicts with configured llama.cpp environment variable `{name}`"
            )));
        }
        environment_remove.extend(environment_names.iter().map(OsString::from));
        match (id.as_str(), &resolved.value) {
            ("context_length", LoadSettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ctx-size", *value);
            }
            ("parallel_requests", LoadSettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--parallel", *value);
            }
            ("llama.cpp.threads", LoadSettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--threads", *value);
            }
            ("llama.cpp.batch_size", LoadSettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--batch-size", *value);
            }
            ("llama.cpp.micro_batch_size", LoadSettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ubatch-size", *value);
            }
            ("llama.cpp.gpu_offload", LoadSettingValue::GpuOffload(value)) => {
                arguments.push(OsString::from("--n-gpu-layers"));
                arguments.push(OsString::from(match value {
                    GpuOffload::None => "0".to_owned(),
                    GpuOffload::Auto => "auto".to_owned(),
                    GpuOffload::All => "all".to_owned(),
                    GpuOffload::Layers(value) => value.to_string(),
                }));
            }
            ("llama.cpp.flash_attention", LoadSettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--flash-attn", value);
            }
            ("llama.cpp.kv_cache_k", LoadSettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--cache-type-k", value);
            }
            ("llama.cpp.kv_cache_v", LoadSettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--cache-type-v", value);
            }
            ("llama.cpp.load_mode", LoadSettingValue::Choice(value)) => {
                arguments.push(OsString::from("--load-mode"));
                arguments.push(OsString::from(value));
            }
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "load setting `{id}` has an invalid value for llama.cpp"
                )));
            }
        }
    }
    Ok(LlamaStructuredArguments {
        arguments,
        environment_remove,
    })
}

fn llama_setting_collision_contract(
    id: &str,
) -> (&'static [&'static str], &'static [&'static str]) {
    match id {
        "context_length" => (&["-c", "--ctx-size"], &["LLAMA_ARG_CTX_SIZE"]),
        "parallel_requests" => (&["-np", "--parallel"], &["LLAMA_ARG_N_PARALLEL"]),
        "llama.cpp.threads" => (&["-t", "--threads"], &["LLAMA_ARG_THREADS"]),
        "llama.cpp.batch_size" => (&["-b", "--batch-size"], &["LLAMA_ARG_BATCH"]),
        "llama.cpp.micro_batch_size" => (&["-ub", "--ubatch-size"], &["LLAMA_ARG_UBATCH"]),
        "llama.cpp.gpu_offload" => (
            &["-ngl", "--gpu-layers", "--n-gpu-layers"],
            &["LLAMA_ARG_N_GPU_LAYERS"],
        ),
        "llama.cpp.flash_attention" => (&["-fa", "--flash-attn"], &["LLAMA_ARG_FLASH_ATTN"]),
        "llama.cpp.kv_cache_k" => (&["-ctk", "--cache-type-k"], &["LLAMA_ARG_CACHE_TYPE_K"]),
        "llama.cpp.kv_cache_v" => (&["-ctv", "--cache-type-v"], &["LLAMA_ARG_CACHE_TYPE_V"]),
        "llama.cpp.load_mode" => (
            &[
                "-lm",
                "--load-mode",
                "--mmap",
                "--no-mmap",
                "--mlock",
                "-dio",
                "--direct-io",
                "-ndio",
                "--no-direct-io",
            ],
            &[
                "LLAMA_ARG_LOAD_MODE",
                "LLAMA_ARG_MMAP",
                "LLAMA_ARG_NO_MMAP",
                "LLAMA_ARG_MLOCK",
                "LLAMA_ARG_DIO",
                "LLAMA_ARG_NO_DIO",
            ],
        ),
        _ => (&[], &[]),
    }
}

fn find_native_option<'a>(arguments: &'a [String], aliases: &[&str]) -> Option<&'a str> {
    arguments.iter().find_map(|argument| {
        let normalized = argument.to_ascii_lowercase().replace('_', "-");
        aliases
            .iter()
            .any(|alias| {
                normalized == *alias
                    || normalized
                        .strip_prefix(alias)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
            .then_some(argument.as_str())
    })
}

fn push_value_argument(arguments: &mut Vec<OsString>, option: &str, value: impl ToString) {
    arguments.push(OsString::from(option));
    arguments.push(OsString::from(value.to_string()));
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

fn parse_version(output: &str) -> (Option<String>, Option<String>) {
    let version_line = output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("version:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    });
    let parenthesized_revision = version_line.and_then(|line| {
        let (_, value) = line.rsplit_once(" (")?;
        let value = value.strip_suffix(')')?.trim();
        (!value.is_empty() && !value.contains([' ', ',']) && !value.eq_ignore_ascii_case("unknown"))
            .then_some(value)
    });
    let version = version_line
        .map(|line| {
            let without_build = line.split(" (build").next().unwrap_or(line);
            without_build
                .rsplit_once(" (")
                .map_or(without_build, |(head, _)| head)
                .trim()
        })
        .filter(|value| !value.eq_ignore_ascii_case("unknown"))
        .map(ToOwned::to_owned);
    let revision = version_line
        .and_then(|line| line.split("commit ").nth(1))
        .map(|value| value.trim_end_matches(')').trim())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("unknown"))
        .or(parenthesized_revision)
        .map(ToOwned::to_owned);
    (version, revision)
}

fn map_finish_reason(reason: Option<&str>) -> Result<InferenceFinishReason, EngineError> {
    match reason {
        Some("stop") => Ok(InferenceFinishReason::Stop),
        Some("length") => Ok(InferenceFinishReason::MaxOutputTokens),
        Some(reason) => Err(EngineError::Operation(format!(
            "llama.cpp returned unsupported finish reason `{reason}`"
        ))),
        None => Err(EngineError::Operation(
            "llama.cpp response omitted its terminal finish reason".to_owned(),
        )),
    }
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
    Ok(hex_digest(hash.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn map_transport_error(error: reqwest::Error) -> EngineError {
    if error.is_timeout() {
        EngineError::TimedOut("llama.cpp inference request timed out".to_owned())
    } else {
        EngineError::BackendUnavailable(error.to_string())
    }
}

fn backend_http_error(status: reqwest::StatusCode, body: &[u8]) -> EngineError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .map_or_else(
            || {
                status
                    .canonical_reason()
                    .unwrap_or("backend request failed")
                    .to_owned()
            },
            |error| backend_error_message(&error),
        );
    EngineError::Operation(format!("llama.cpp returned HTTP {status}: {message}"))
}

fn backend_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("unknown backend error")
        .to_owned()
}

/// Best-effort UX progress from llama-server startup output. The supported
/// upstream build does not emit a stable machine-readable loading fraction, so
/// this reports truthful indeterminate phases from a few conservative line
/// prefixes and degrades to `None` for unknown or changed log formats. This is
/// never an admission gate: startup correctness stays with `health`.
fn parse_llama_startup_progress(stderr_tail: &[String]) -> Option<BackendLoadProgress> {
    let mut progress = None;
    for line in stderr_tail {
        let line = line.trim();
        if line.starts_with("main: server is listening")
            || line.starts_with("srv  init:")
            || line.starts_with("srv init:")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::VerifyingStartup,
                "Starting the llama.cpp server",
            ));
        } else if line.starts_with("llama_kv_cache") || line.starts_with("llama_context:") {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::AllocatingContext,
                "Allocating context and KV cache",
            ));
        } else if line.starts_with("load_tensors:") {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::LoadingModel,
                "Loading model tensors",
            ));
        } else if line.starts_with("llama_model_loader:") {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::LoadingModel,
                "Reading model metadata",
            ));
        }
    }
    progress
}

fn command_detail(stdout: &str, stderr: &str) -> String {
    match (stdout.trim(), stderr.trim()) {
        ("", "") => "no output".to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, "") => stdout.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or("version unavailable")
}

fn http_endpoint(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{address}")
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod startup_progress_tests {
    use super::*;

    #[test]
    fn unrecognized_output_yields_no_progress() {
        assert_eq!(parse_llama_startup_progress(&[]), None);
        assert_eq!(
            parse_llama_startup_progress(&["completely unrelated log line".to_owned()]),
            None
        );
    }

    #[test]
    fn known_prefixes_yield_indeterminate_phases_without_inventing_fractions() {
        let metadata = parse_llama_startup_progress(&[
            "llama_model_loader: - type f16:   1 tensors".to_owned(),
        ])
        .expect("model loader line yields a phase");
        assert_eq!(metadata.phase, BackendLoadPhase::LoadingModel);
        assert_eq!(metadata.fraction, None);
        assert_eq!(metadata.message.as_deref(), Some("Reading model metadata"));

        let tensors = parse_llama_startup_progress(&["load_tensors: layer 0 processed".to_owned()])
            .expect("load_tensors line yields a phase");
        assert_eq!(tensors.phase, BackendLoadPhase::LoadingModel);
        assert_eq!(tensors.fraction, None);

        let context =
            parse_llama_startup_progress(&["llama_context: constructing llama_context".to_owned()])
                .expect("context line yields a phase");
        assert_eq!(context.phase, BackendLoadPhase::AllocatingContext);

        let listening = parse_llama_startup_progress(&[
            "main: server is listening on 127.0.0.1:8080".to_owned(),
        ])
        .expect("listening line yields a phase");
        assert_eq!(listening.phase, BackendLoadPhase::VerifyingStartup);
    }

    #[test]
    fn later_phase_wins_over_earlier_phase() {
        let progress = parse_llama_startup_progress(&[
            "llama_model_loader: - type f16:   1 tensors".to_owned(),
            "load_tensors: layer 0 processed".to_owned(),
            "llama_context: constructing llama_context".to_owned(),
            "main: server is listening on 127.0.0.1:8080".to_owned(),
        ])
        .expect("multi-line startup yields the latest phase");
        assert_eq!(progress.phase, BackendLoadPhase::VerifyingStartup);
    }
}

#[cfg(test)]
mod generation_settings_tests {
    use norted_core::ModelId;

    use super::*;

    fn request(generation_settings: GenerationSettingsPatch) -> InferenceRequest {
        InferenceRequest {
            model_id: ModelId("model".to_owned()),
            messages: vec![InferenceMessage {
                role: InferenceRole::User,
                text: "hello".to_owned(),
            }],
            generation_settings,
            max_output_tokens: Some(123),
            stream: false,
        }
    }

    #[test]
    fn backend_request_sends_only_explicit_sampler_fields() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        let omitted = adapter.backend_request(&request(GenerationSettingsPatch::default()), false);
        assert!(omitted.get("temperature").is_none());
        assert!(omitted.get("top_p").is_none());
        assert_eq!(omitted["max_completion_tokens"], 123);

        let explicit = adapter.backend_request(
            &request(GenerationSettingsPatch {
                temperature: Some(0.25),
                top_p: Some(0.8),
                reasoning_effort: None,
            }),
            false,
        );
        assert_eq!(explicit["temperature"], 0.25);
        assert_eq!(explicit["top_p"], 0.8);
        assert_eq!(explicit["max_completion_tokens"], 123);
    }

    #[test]
    fn validates_supported_sampler_ranges() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        assert!(
            adapter
                .validate_generation_settings(
                    &GenerationSettingsPatch {
                        temperature: Some(2.0),
                        top_p: Some(0.0),
                        reasoning_effort: None,
                    },
                    &EffectiveGenerationSettings {
                        temperature: 0.8,
                        top_p: 0.95,
                    },
                )
                .is_ok()
        );
        for invalid in [
            GenerationSettingsPatch {
                temperature: Some(-0.1),
                top_p: None,
                reasoning_effort: None,
            },
            GenerationSettingsPatch {
                temperature: Some(2.1),
                top_p: None,
                reasoning_effort: None,
            },
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(1.1),
                reasoning_effort: None,
            },
            GenerationSettingsPatch {
                temperature: Some(f64::NAN),
                top_p: None,
                reasoning_effort: None,
            },
        ] {
            assert!(matches!(
                adapter.validate_generation_settings(
                    &invalid,
                    &EffectiveGenerationSettings {
                        temperature: 0.8,
                        top_p: 0.95,
                    },
                ),
                Err(EngineError::InvalidGenerationSettings(_))
            ));
        }
    }
}

#[cfg(test)]
mod load_settings_tests {
    use norted_core::{LoadSettingSource, ResolvedLoadSetting, ResolvedLoadSettings};

    use super::*;

    fn resolved(values: &[(&str, LoadSettingValue)]) -> ResolvedLoadSettings {
        let mut effective = BTreeMap::new();
        for (id, value) in values {
            effective.insert(
                LoadSettingId::new(*id).expect("setting ID"),
                ResolvedLoadSetting {
                    value: value.clone(),
                    source: LoadSettingSource::Invocation,
                },
            );
        }
        ResolvedLoadSettings {
            engine_id: ENGINE_ID.to_owned(),
            selected_profile: None,
            effective,
        }
    }

    fn strings(arguments: Vec<OsString>) -> Vec<String> {
        arguments
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn omitted_settings_emit_no_llama_arguments() {
        let translated = translate_llama_load_settings(&resolved(&[]), &[], &BTreeMap::new())
            .expect("empty translation");
        assert!(translated.arguments.is_empty());
        assert!(translated.environment_remove.is_empty());
    }

    #[test]
    fn common_and_kv_settings_translate_independently() {
        let translated = translate_llama_load_settings(
            &resolved(&[
                ("context_length", LoadSettingValue::UnsignedInteger(131_072)),
                ("parallel_requests", LoadSettingValue::UnsignedInteger(3)),
                (
                    "llama.cpp.kv_cache_k",
                    LoadSettingValue::Choice("q8_0".to_owned()),
                ),
            ]),
            &[],
            &BTreeMap::new(),
        )
        .expect("structured translation");
        assert_eq!(
            strings(translated.arguments),
            [
                "--ctx-size",
                "131072",
                "--cache-type-k",
                "q8_0",
                "--parallel",
                "3",
            ]
        );
    }

    #[test]
    fn exact_advertised_load_modes_translate_once() {
        let help = "  --mlock  DEPRECATED in favor of --load-mode mlock\n\
                    \x20\x20-lm, --load-mode MODE  model loading mode\n\
                    \x20\x20\x20\x20- auto: automatic\n\
                    \x20\x20\x20\x20- none: ordinary reads\n\
                    \x20\x20\x20\x20- mmap: memory map\n\
                    \x20\x20\x20\x20- mlock: lock memory\n\
                    \x20\x20\x20\x20- mmap+mlock: map and lock\n\
                    \x20\x20\x20\x20- dio: direct I/O\n\
                    \x20\x20--next-option VALUE  unrelated";
        assert_eq!(advertised_llama_load_modes(help), llama_load_modes());
        assert!(help_has_option(help, "--load-mode"));

        let translated = translate_llama_load_settings(
            &resolved(&[(
                "llama.cpp.load_mode",
                LoadSettingValue::Choice("mmap+mlock".to_owned()),
            )]),
            &[],
            &BTreeMap::new(),
        )
        .expect("load-mode translation");
        assert_eq!(strings(translated.arguments), ["--load-mode", "mmap+mlock"]);
    }

    #[test]
    fn structured_load_mode_owns_every_equivalent_native_argument() {
        let settings = resolved(&[(
            "llama.cpp.load_mode",
            LoadSettingValue::Choice("mmap".to_owned()),
        )]);
        for argument in [
            "-lm",
            "--load-mode=none",
            "--mmap",
            "--mmap=false",
            "--no-mmap",
            "--mlock",
            "-dio",
            "--direct-io",
            "-ndio",
            "--no-direct-io=true",
        ] {
            let error =
                translate_llama_load_settings(&settings, &[argument.to_owned()], &BTreeMap::new())
                    .expect_err("native load-mode collision");
            assert!(error.to_string().contains(argument), "{error}");
        }
    }

    #[test]
    fn structured_load_mode_owns_equivalent_environment_only_when_active() {
        let settings = resolved(&[(
            "llama.cpp.load_mode",
            LoadSettingValue::Choice("dio".to_owned()),
        )]);
        let equivalent = [
            "LLAMA_ARG_LOAD_MODE",
            "LLAMA_ARG_MMAP",
            "LLAMA_ARG_NO_MMAP",
            "LLAMA_ARG_MLOCK",
            "LLAMA_ARG_DIO",
            "LLAMA_ARG_NO_DIO",
        ];
        for name in equivalent {
            let error = translate_llama_load_settings(
                &settings,
                &[],
                &BTreeMap::from([(name.to_owned(), "1".to_owned())]),
            )
            .expect_err("configured environment collision");
            assert!(error.to_string().contains(name), "{error}");
        }

        let translated =
            translate_llama_load_settings(&settings, &[], &BTreeMap::new()).expect("translation");
        assert_eq!(
            translated.environment_remove,
            equivalent.map(OsString::from)
        );

        let absent = translate_llama_load_settings(
            &resolved(&[]),
            &["--load-mode=none".to_owned()],
            &BTreeMap::from([("LLAMA_ARG_MMAP".to_owned(), "0".to_owned())]),
        )
        .expect("native controls remain available without structured ownership");
        assert!(absent.arguments.is_empty());
        assert!(absent.environment_remove.is_empty());
    }

    #[test]
    fn structured_llama_setting_rejects_both_native_argument_forms() {
        let settings = resolved(&[("context_length", LoadSettingValue::UnsignedInteger(8192))]);
        for native in [
            vec!["--ctx-size=4096".to_owned()],
            vec!["-c".to_owned(), "4096".to_owned()],
        ] {
            let error = translate_llama_load_settings(&settings, &native, &BTreeMap::new())
                .expect_err("native collision");
            assert!(error.to_string().contains("conflicts"));
        }
    }

    #[test]
    fn malformed_values_are_rejected_by_the_definition() {
        let definition = llama_load_setting_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "context_length")
            .expect("context definition");
        assert!(definition.parse("0").is_err());
        assert!(definition.parse("many").is_err());
    }
}
