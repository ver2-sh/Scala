//! llama.cpp-specific launch, probe, health, and inference translation.

mod catalog;
mod source_catalog;

pub use catalog::{LLAMA_CPP_RUNTIME_PROVIDER_ID, LlamaCppRuntimeCatalogProvider};
pub use source_catalog::{
    LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID, LlamaCppSourceRuntimeCatalogProvider,
};

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
    AcquisitionMethod, ArtifactFormat, ArtifactNativeIdentity, AvailableRuntime, EngineConfig,
    EngineInstallation, EngineRevision, GpuOffload, HostCapabilities, InstalledRuntime,
    ModelArtifact, RuntimeAcquisitionMethod, RuntimeCompatibility, RuntimeId, RuntimeIdentity,
    RuntimeProbeObservation, SettingDefaultPreview, SettingDefaultSource, SettingDefinition,
    SettingId, SettingKind, SettingScope, SettingValue, SettingsSchema,
    UnsignedIntegerOrChoiceValue,
};
use norted_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, InferenceActivityReporter,
    InferenceActivityUpdate, InferenceEvent, InferenceFinishReason, InferenceMessage,
    InferenceOutput, InferenceRequest, InferenceRole, InferenceStream, InferenceUsage,
    InstallationState, LaunchRequest, LaunchSpec, LoadProgressReporter, NativeOption,
    OptionValueKind, OutputFormat, PreparedModelInput, ProcessDescriptor,
    RuntimeVariantUpdateIdentity, StartupObservation, UpdateState, capture_command,
    common_setting_definitions_for, configurable_setting_definitions, prepare_norted_package_input,
    prepare_norted_package_input_with_progress, revalidate_norted_package_before_launch,
    revalidate_norted_package_before_launch_with_progress,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const ENGINE_ID: &str = "llama.cpp";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/ggml-org/llama.cpp";

const MANAGED_LLAMA_PACKAGE_FAMILY: &str = "llama-cpp-managed-source";
const MANAGED_LLAMA_REPOSITORY: &str = "ggml-org/llama.cpp";
const MANAGED_CUDA12_FUNCTIONAL_VARIANT: &str = "managed-linux-x86_64-cuda12-portable";
const MANAGED_CUDA13_FUNCTIONAL_VARIANT: &str = "managed-linux-x86_64-cuda13-portable";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const PROPS_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;

fn is_managed_llama_linux_cuda(identity: &RuntimeIdentity) -> bool {
    identity.engine_id == ENGINE_ID
        && identity.package_family == MANAGED_LLAMA_PACKAGE_FAMILY
        && identity.platform == "linux"
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.package.provider_id == LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
        && identity.package.repository.as_deref() == Some(MANAGED_LLAMA_REPOSITORY)
}

fn managed_llama_variant_update_identity(
    identity: &RuntimeIdentity,
) -> Option<RuntimeVariantUpdateIdentity> {
    if !is_managed_llama_linux_cuda(identity) {
        return None;
    }
    let (functional_variant, generation) = match identity.variant.as_str() {
        "managed-portable-v1" => (MANAGED_CUDA12_FUNCTIONAL_VARIANT, 1),
        "managed-portable-v2" => (MANAGED_CUDA12_FUNCTIONAL_VARIANT, 2),
        "managed-portable-v3" => (MANAGED_CUDA12_FUNCTIONAL_VARIANT, 3),
        "managed-portable-v4" => (MANAGED_CUDA12_FUNCTIONAL_VARIANT, 4),
        "managed-portable-cuda13-v1" => (MANAGED_CUDA13_FUNCTIONAL_VARIANT, 1),
        "managed-portable-cuda13-v2" => (MANAGED_CUDA13_FUNCTIONAL_VARIANT, 2),
        _ => return None,
    };
    Some(RuntimeVariantUpdateIdentity {
        functional_variant: functional_variant.to_owned(),
        source_recipe_generation: Some(generation),
    })
}

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

fn llama_model_compatibility(artifact: CompatibilityDecision) -> RuntimeCompatibility {
    match artifact {
        CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
        CompatibilityDecision::Unsupported { reason } => RuntimeCompatibility::Incompatible(reason),
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
            "model": request.model_profile_id.as_str(),
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
        if let Some(seed) = request.generation_settings.seed {
            body["seed"] = json!(seed);
        }
        if let Some(repeat_penalty) = request.generation_settings.repeat_penalty {
            body["repeat_penalty"] = json!(repeat_penalty);
        }
        if let Some(presence_penalty) = request.generation_settings.presence_penalty {
            body["presence_penalty"] = json!(presence_penalty);
        }
        if let Some(frequency_penalty) = request.generation_settings.frequency_penalty {
            body["frequency_penalty"] = json!(frequency_penalty);
        }
        if let Some(stop) = &request.generation_settings.stop {
            body["stop"] = json!(stop);
        }
        if let Some(reasoning_effort) = request.generation_settings.reasoning_effort {
            body["reasoning_effort"] = json!(reasoning_effort.as_str());
        }
        if let Some(output_format) = &request.output_format {
            body["response_format"] = match output_format {
                OutputFormat::Text => json!({ "type": "text" }),
                OutputFormat::JsonObject => json!({ "type": "json_object" }),
                OutputFormat::JsonSchema {
                    name,
                    description,
                    schema,
                    strict,
                } => {
                    let mut wrapper = json!({ "schema": schema });
                    let object = wrapper.as_object_mut().expect("static object");
                    if let Some(name) = name {
                        object.insert("name".to_owned(), json!(name));
                    }
                    if let Some(description) = description {
                        object.insert("description".to_owned(), json!(description));
                    }
                    if let Some(strict) = strict {
                        object.insert("strict".to_owned(), json!(strict));
                    }
                    json!({ "type": "json_schema", "json_schema": wrapper })
                }
            };
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
            body["return_progress"] = json!(true);
            body["timings_per_token"] = json!(true);
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

    fn runtime_variant_update_identity(
        &self,
        identity: &RuntimeIdentity,
    ) -> RuntimeVariantUpdateIdentity {
        managed_llama_variant_update_identity(identity)
            .unwrap_or_else(|| RuntimeVariantUpdateIdentity::exact(identity))
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
        if settings.seed.is_some_and(|seed| seed > u64::from(u32::MAX)) {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp seed must fit in an unsigned 32-bit integer".to_owned(),
            ));
        }
        if let Some(value) = settings.repeat_penalty
            && (!value.is_finite() || value < 0.0)
        {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp repeat penalty must be finite and non-negative".to_owned(),
            ));
        }
        if let Some(value) = settings.presence_penalty
            && (!value.is_finite() || !(-2.0..=2.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp presence penalty must be finite and in the range -2..=2".to_owned(),
            ));
        }
        if let Some(value) = settings.frequency_penalty
            && (!value.is_finite() || !(-2.0..=2.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp frequency penalty must be finite and in the range -2..=2".to_owned(),
            ));
        }
        if settings.stop.as_ref().is_some_and(|values| {
            values.is_empty()
                || values
                    .iter()
                    .any(|value| value.is_empty() || value.contains('\0'))
        }) {
            return Err(EngineError::InvalidGenerationSettings(
                "llama.cpp stop strings must be non-empty and contain no NUL bytes".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_inference_request(
        &self,
        request: &InferenceRequest,
        backend_defaults: &EffectiveGenerationSettings,
        settings_schema: &SettingsSchema,
    ) -> Result<(), EngineError> {
        self.validate_generation_settings(&request.generation_settings, backend_defaults)?;
        let required = [
            (request.generation_settings.seed.is_some(), "seed"),
            (
                request.generation_settings.repeat_penalty.is_some(),
                "repeat_penalty",
            ),
            (
                request.generation_settings.presence_penalty.is_some(),
                "presence_penalty",
            ),
            (
                request.generation_settings.frequency_penalty.is_some(),
                "frequency_penalty",
            ),
            (request.generation_settings.stop.is_some(), "stop_strings"),
            (
                request.generation_settings.reasoning_effort.is_some(),
                "reasoning_effort",
            ),
            (
                matches!(
                    request.output_format.as_ref(),
                    Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
                ),
                "structured_output_schema",
            ),
        ];
        for (used, id) in required {
            if used {
                require_supported_setting(settings_schema, id)?;
            }
        }
        if let Some(effort) = request.generation_settings.reasoning_effort {
            let id = SettingId::new("reasoning_effort").expect("static setting ID");
            let definition = settings_schema.definition(&id).expect("common definition");
            let SettingKind::Choice { choices } = &definition.kind else {
                return Err(EngineError::InvalidGenerationSettings(
                    "exact llama.cpp reasoning-effort contract is malformed".to_owned(),
                ));
            };
            if !choices.iter().any(|choice| choice == effort.as_str()) {
                return Err(EngineError::InvalidGenerationSettings(format!(
                    "the exact llama-server does not advertise reasoning effort `{effort}`"
                )));
            }
        }
        Ok(())
    }

    async fn context_capacity(&self, endpoint: &str) -> Result<Option<u64>, EngineError> {
        let response = self
            .client
            .get(format!("{endpoint}/props"))
            .timeout(PROPS_TIMEOUT)
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
        let props: PropsResponse = serde_json::from_slice(&body).map_err(|error| {
            EngineError::Operation(format!(
                "invalid llama.cpp /props context contract: {error}"
            ))
        })?;
        Ok(Some(props.default_generation_settings.n_ctx))
    }

    async fn count_input_tokens(
        &self,
        endpoint: &str,
        request: &InferenceRequest,
    ) -> Result<Option<u64>, EngineError> {
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions/input_tokens"))
            .timeout(PROPS_TIMEOUT)
            .json(&self.backend_request(request, false))
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(EngineError::InvalidGenerationSettings(format!(
                "exact llama-server token counting is unavailable: {}",
                backend_http_error(status, &body)
            )));
        }
        let value: Value = serde_json::from_slice(&body).map_err(|error| {
            EngineError::Operation(format!("invalid llama.cpp input-token response: {error}"))
        })?;
        let count = value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                EngineError::Operation(
                    "llama.cpp input-token response omitted `input_tokens`".to_owned(),
                )
            })?;
        Ok(Some(count))
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

    fn runtime_compatibility(&self, runtime: &InstalledRuntime) -> CompatibilityDecision {
        let identity = &runtime.manifest.identity;
        if is_managed_llama_linux_cuda(identity) && identity.variant == "managed-portable-v1" {
            CompatibilityDecision::Unsupported {
                reason: "legacy managed llama.cpp source recipe `managed-portable-v1` has a known relocation defect from stale build-tree shared-library paths; replace it with a current managed recipe"
                    .to_owned(),
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
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> RuntimeCompatibility {
        let model_compatibility = llama_model_compatibility(self.compatibility(model));
        if matches!(model_compatibility, RuntimeCompatibility::Incompatible(_)) {
            return model_compatibility;
        }
        let help = self
            .capability_cache
            .try_read()
            .ok()
            .and_then(|cache| cache.get(&Self::capability_key(runtime)).cloned());
        settings.map_or(model_compatibility, |settings| {
            llama_configured_runtime_compatibility(settings, Some(model), help.as_deref())
        })
    }

    fn available_runtime_model_compatibility(
        &self,
        _runtime: &AvailableRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> RuntimeCompatibility {
        let model_compatibility = llama_model_compatibility(self.compatibility(model));
        if matches!(model_compatibility, RuntimeCompatibility::Incompatible(_)) {
            return model_compatibility;
        }
        settings.map_or(model_compatibility, |settings| {
            llama_configured_runtime_compatibility(settings, Some(model), None)
        })
    }

    fn runtime_model_preference(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> u16 {
        llama_runtime_preference(&runtime.manifest.identity.accelerator, host)
    }

    fn available_runtime_model_preference(
        &self,
        runtime: &AvailableRuntime,
        _model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> u16 {
        llama_runtime_preference(&runtime.identity.accelerator, host)
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

    fn setting_definitions(&self) -> Vec<SettingDefinition> {
        llama_setting_definitions()
    }

    fn model_setting_definitions(
        &self,
        model: &ModelArtifact,
    ) -> Result<Vec<SettingDefinition>, EngineError> {
        Ok(configurable_setting_definitions(
            llama_model_setting_definitions(Some(model)),
        ))
    }

    async fn runtime_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self.cached_runtime_help(runtime).await?;
        let mut definitions = self.setting_definitions();
        apply_llama_exact_help_contract(&mut definitions, &help);
        if let Some(definition) = definitions
            .iter_mut()
            .find(|definition| definition.id.as_str() == "llama.cpp.active_experts")
        {
            definition.supported = false;
            definition.unsupported_reason = Some(
                "active experts requires a bound model with inspected expert metadata".to_owned(),
            );
        }
        let definitions = configurable_setting_definitions(definitions);
        Ok(SettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions,
        })
    }

    async fn settings_schema(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self.cached_runtime_help(runtime).await?;
        let mut definitions = self.model_setting_definitions(model)?;
        apply_llama_exact_help_contract(&mut definitions, &help);
        let definitions = configurable_setting_definitions(definitions);
        Ok(SettingsSchema {
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
            .settings_schema
            .validate(&request.settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        validate_llama_bound_files(&request.settings, &request.model.primary).await?;
        let structured = translate_llama_settings_for_model(
            &request.settings,
            Some(&request.model.primary),
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
        let public_model_id = request.settings.model_profile_id.as_ref().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "resolved settings do not identify the loaded Model Profile".to_owned(),
            )
        })?;
        let arguments = vec![
            OsString::from("--model"),
            request.model.primary.path.as_os_str().to_owned(),
            OsString::from("--alias"),
            OsString::from(public_model_id.as_str()),
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
            normalized_settings: llama_normalized_generation_defaults(&request.settings),
            settings: request.settings,
            native_arguments: self.native_arguments.clone(),
            installation: (*installation).clone(),
            runtime: request.runtime,
            model: request.model,
            accelerator: request.accelerator,
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

    async fn startup_observation(
        &self,
        process: &ProcessDescriptor,
        _stderr_tail: &[String],
    ) -> Result<StartupObservation, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("llama.cpp process has no backend endpoint".to_owned())
        })?;
        let Some(context) = self.context_capacity(endpoint).await? else {
            return Ok(StartupObservation::Ready(BTreeMap::new()));
        };
        Ok(StartupObservation::Ready(BTreeMap::from([(
            "resolved_settings".to_owned(),
            json!({"context_length": context}),
        )])))
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
            tool_calls: Vec::new(),
            usage: response.usage.map(Into::into),
            finish_reason,
        })
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        activity: InferenceActivityReporter,
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
        Ok(llama_sse_stream(response.bytes_stream().boxed(), activity))
    }
}

fn llama_runtime_preference(accelerator: &str, host: &HostCapabilities) -> u16 {
    let has_nvidia = host
        .accelerators
        .iter()
        .any(|device| device.accelerator.eq_ignore_ascii_case("cuda"));
    match (has_nvidia, accelerator) {
        (true, "cuda") | (false, "cpu") => 0,
        (_, "vulkan") => 10,
        (true, "cpu") | (false, "cuda") => 20,
        _ => 100,
    }
}

fn require_supported_setting(schema: &SettingsSchema, id: &str) -> Result<(), EngineError> {
    let id = SettingId::new(id).expect("static setting ID");
    match schema.definition(&id) {
        Some(definition) if definition.supported => Ok(()),
        Some(definition) => Err(EngineError::InvalidGenerationSettings(
            definition
                .unsupported_reason
                .clone()
                .unwrap_or_else(|| format!("the exact llama-server does not support `{id}`")),
        )),
        None => Err(EngineError::InvalidGenerationSettings(format!(
            "the exact llama-server schema does not define `{id}`"
        ))),
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
    n_ctx: u64,
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
    activity: InferenceActivityReporter,
    finished: bool,
}

fn llama_sse_stream(
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    activity: InferenceActivityReporter,
) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
        activity,
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
        if let Some(progress) = value.get("prompt_progress")
            && let (Some(total), Some(current), Some(cache)) = (
                progress.get("total").and_then(Value::as_u64),
                progress.get("processed").and_then(Value::as_u64),
                progress.get("cache").and_then(Value::as_u64),
            )
            && total > 0
            && cache <= current
            && current <= total
        {
            (state.activity)(InferenceActivityUpdate::PromptProgress { current, total });
        }
        if let Some(tokens) = value
            .get("timings")
            .and_then(|timings| timings.get("predicted_n"))
            .and_then(Value::as_u64)
        {
            (state.activity)(InferenceActivityUpdate::GeneratedTokens(tokens));
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
        .filter_map(InferenceMessage::text_only)
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
            InferenceRole::Tool => "tool",
        },
        "content": message.text_only().unwrap_or_default(),
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

fn llama_setting_definitions() -> Vec<SettingDefinition> {
    const COMMON_SETTINGS: &[&str] = &[
        "context_length",
        "parallel_requests",
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "seed",
        "repeat_penalty",
        "presence_penalty",
        "frequency_penalty",
        "max_output_tokens",
        "stop_strings",
        "system_prompt",
        "reasoning",
        "reasoning_effort",
        "reasoning_budget",
        "reasoning_budget_message",
        "structured_output_schema",
        "context_overflow",
    ];
    let mut definitions = common_setting_definitions_for(ENGINE_ID, COMMON_SETTINGS);
    definitions.extend([
        llama_definition(
            "llama.cpp.threads",
            "CPU threads",
            "CPU threads used during token generation",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
        ),
        llama_definition(
            "llama.cpp.batch_size",
            "Batch size",
            "Logical maximum prompt-processing batch size",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
        ),
        llama_definition(
            "llama.cpp.micro_batch_size",
            "Micro-batch size",
            "Physical maximum prompt-processing batch size",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
        ),
        llama_definition(
            "llama.cpp.gpu_offload",
            "GPU offload",
            "Weight layers placed in VRAM: none, auto, all, or an exact count",
            SettingKind::GpuOffload,
        ),
        llama_definition(
            "llama.cpp.flash_attention",
            "Flash attention",
            "Explicit llama.cpp Flash Attention mode",
            SettingKind::Choice {
                choices: vec!["auto".to_owned(), "on".to_owned(), "off".to_owned()],
            },
        ),
        llama_definition(
            "llama.cpp.kv_cache_k",
            "K-cache type",
            "Key cache storage type",
            SettingKind::Choice {
                choices: llama_cache_types(),
            },
        ),
        llama_definition(
            "llama.cpp.kv_cache_v",
            "V-cache type",
            "Value cache storage type",
            SettingKind::Choice {
                choices: llama_cache_types(),
            },
        ),
        llama_definition(
            "llama.cpp.load_mode",
            "Model load mode",
            "Current model-loading policy covering mmap and keep-in-memory semantics without deprecated standalone switches",
            SettingKind::Choice {
                choices: llama_load_modes(),
            },
        ),
        llama_definition(
            "llama.cpp.rope_frequency_base",
            "RoPE frequency base",
            "RoPE base frequency used by NTK-aware scaling",
            SettingKind::Float { minimum: Some(f64::MIN_POSITIVE), maximum: None },
        ),
        llama_definition(
            "llama.cpp.rope_frequency_scale",
            "RoPE frequency scale",
            "RoPE frequency scaling factor; context expands by 1/value",
            SettingKind::Float { minimum: Some(f64::MIN_POSITIVE), maximum: None },
        ),
        llama_definition(
            "llama.cpp.unified_kv_cache",
            "Unified KV cache",
            "Use one KV buffer shared across server sequences",
            SettingKind::Toggle,
        ),
        llama_definition(
            "llama.cpp.kv_cache_gpu_offload",
            "KV cache GPU offload",
            "Place KV cache storage on an accelerator when enabled",
            SettingKind::Toggle,
        ),
        llama_definition(
            "llama.cpp.context_checkpoints",
            "Context checkpoints",
            "Maximum context checkpoints per server slot; omission preserves the exact runtime default",
            SettingKind::UnsignedInteger { minimum: Some(0), maximum: None },
        ),
        llama_definition(
            "llama.cpp.cpu_moe_layers",
            "MoE layers on CPU",
            "Keep expert weights for the first N model layers on CPU",
            SettingKind::UnsignedInteger { minimum: Some(0), maximum: None },
        ),
        llama_definition(
            "llama.cpp.cpu_moe_all",
            "All MoE weights on CPU",
            "Keep all Mixture-of-Experts weights on CPU",
            SettingKind::OneWayFlag,
        ),
        llama_definition(
            "llama.cpp.active_experts",
            "Number of active experts",
            "Override the architecture-specific GGUF expert_used_count metadata only when the selected model proves that key",
            SettingKind::UnsignedInteger { minimum: Some(1), maximum: None },
        ),
        llama_definition(
            "llama.cpp.chat_template",
            "Chat template",
            "Exact runtime-advertised built-in chat template name; omission keeps model metadata authoritative",
            SettingKind::Choice { choices: Vec::new() },
        ),
        llama_definition(
            "llama.cpp.chat_template_file",
            "Chat template file",
            "Bound local Jinja chat-template file; content identity is recorded with the profile/default",
            SettingKind::Path,
        ),
        llama_definition(
            "llama.cpp.chat_template_sha256",
            "Chat template SHA-256",
            "Recorded content identity for the selected local chat-template file",
            SettingKind::String,
        ),
        llama_definition(
            "llama.cpp.speculative_mode",
            "Speculative decoding",
            "Exact runtime-advertised speculative decoding mode; off emits the runtime's none mode",
            SettingKind::Choice { choices: Vec::new() },
        ),
        llama_definition(
            "llama.cpp.speculative_draft_model",
            "Speculative draft model",
            "Bound local GGUF draft artifact used by draft-model speculative modes",
            SettingKind::Path,
        ),
        llama_definition(
            "llama.cpp.speculative_draft_sha256",
            "Draft model SHA-256",
            "Recorded content identity for the selected speculative draft artifact",
            SettingKind::String,
        ),
    ]);
    definitions
}

fn llama_model_setting_definitions(model: Option<&ModelArtifact>) -> Vec<SettingDefinition> {
    let mut definitions = llama_setting_definitions();
    if model.is_some() {
        for id in [
            "context_length",
            "llama.cpp.rope_frequency_base",
            "llama.cpp.rope_frequency_scale",
            "llama.cpp.chat_template",
        ] {
            if let Some(definition) = definitions
                .iter_mut()
                .find(|definition| definition.id.as_str() == id)
            {
                definition.default_preview = Some(
                    SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                    .with_detail(
                        "The selected GGUF does not expose this inspected value, so the exact runtime remains authoritative",
                    ),
                );
            }
        }
    }
    if let Some(temperature) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "temperature")
    {
        temperature.kind = SettingKind::Float {
            minimum: Some(0.0),
            maximum: Some(2.0),
        };
    }
    if let Some(experts) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "llama.cpp.active_experts")
    {
        match model.and_then(|model| match &model.native_identity {
            Some(ArtifactNativeIdentity::Gguf(identity)) => Some(identity),
            _ => None,
        }) {
            Some(identity)
                if identity.expert_used_count.is_some() && identity.expert_count.is_some() =>
            {
                experts.kind = SettingKind::UnsignedInteger {
                    minimum: Some(1),
                    maximum: identity.expert_count,
                };
                experts.default_preview = Some(
                    SettingDefaultPreview::new(
                        identity
                            .expert_used_count
                            .expect("checked expert_used_count")
                            .to_string(),
                        SettingDefaultSource::Model,
                    )
                    .with_detail(format!(
                        "GGUF metadata {}.expert_used_count",
                        identity.architecture
                    )),
                );
            }
            _ => {
                experts.supported = false;
                experts.unsupported_reason = Some(
                    "the selected GGUF does not prove an architecture-specific expert_used_count override"
                        .to_owned(),
                );
            }
        }
    }
    if let Some(ArtifactNativeIdentity::Gguf(identity)) =
        model.and_then(|model| model.native_identity.as_ref())
    {
        for (id, value, metadata_key) in [
            (
                "context_length",
                identity.context_length.map(|value| value.to_string()),
                format!("{}.context_length", identity.architecture),
            ),
            (
                "llama.cpp.rope_frequency_base",
                identity
                    .rope_frequency_base
                    .as_deref()
                    .and_then(valid_positive_decimal),
                format!("{}.rope.freq_base", identity.architecture),
            ),
            (
                "llama.cpp.rope_frequency_scale",
                identity
                    .rope_scaling_factor
                    .as_deref()
                    .and_then(effective_rope_frequency_scale),
                identity
                    .rope_scaling_factor_key
                    .clone()
                    .unwrap_or_else(|| format!("{}.rope.scaling.factor", identity.architecture)),
            ),
        ] {
            if let Some(value) = value
                && let Some(definition) = definitions
                    .iter_mut()
                    .find(|definition| definition.id.as_str() == id)
            {
                let detail = if id == "llama.cpp.rope_frequency_scale" {
                    format!(
                        "Derived with llama.cpp semantics as the inverse of selected GGUF metadata `{metadata_key}`"
                    )
                } else {
                    format!("Selected GGUF metadata `{metadata_key}`")
                };
                definition.default_preview = Some(
                    SettingDefaultPreview::new(value, SettingDefaultSource::Model)
                        .with_detail(detail),
                );
            }
        }
        if let Some(digest) = identity.chat_template_sha256.as_deref()
            && let Some(definition) = definitions
                .iter_mut()
                .find(|definition| definition.id.as_str() == "llama.cpp.chat_template")
        {
            definition.default_preview = Some(
                SettingDefaultPreview::new(
                    format!("embedded · sha256:{}", &digest[..digest.len().min(12)]),
                    SettingDefaultSource::Model,
                )
                .with_detail(
                    "The selected GGUF contains tokenizer.chat_template; its exact content digest is shown",
                ),
            );
        }
    }
    definitions
}

fn valid_positive_decimal(raw: &str) -> Option<String> {
    raw.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|_| raw.to_owned())
}

fn effective_rope_frequency_scale(raw_scaling_factor: &str) -> Option<String> {
    let scaling_factor = raw_scaling_factor.parse::<f64>().ok()?;
    if !scaling_factor.is_finite() || scaling_factor < 0.0 {
        return None;
    }
    let frequency_scale = if scaling_factor == 0.0 {
        1.0
    } else {
        1.0 / scaling_factor
    };
    if !frequency_scale.is_finite() || frequency_scale <= 0.0 {
        return None;
    }
    Some(if frequency_scale.fract() == 0.0 {
        format!("{frequency_scale:.1}")
    } else {
        frequency_scale.to_string()
    })
}

fn llama_definition(
    id: &str,
    label: &str,
    description: &str,
    kind: SettingKind,
) -> SettingDefinition {
    SettingDefinition {
        id: SettingId::new(id).expect("static llama.cpp setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: SettingScope::Runtime {
            engine_id: ENGINE_ID.to_owned(),
        },
        category: if id.contains("unified_kv")
            || id.contains("kv_cache")
            || id.contains("flash_attention")
        {
            norted_core::SettingCategory::KvMemory
        } else if id.contains("speculative") {
            norted_core::SettingCategory::Speculation
        } else if id.contains("chat_template") {
            norted_core::SettingCategory::Prompt
        } else if id.contains("context_checkpoint") {
            norted_core::SettingCategory::Advanced
        } else if id.contains("cache") {
            norted_core::SettingCategory::Cache
        } else {
            norted_core::SettingCategory::Load
        },
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: match id {
            "llama.cpp.chat_template_file"
            | "llama.cpp.chat_template_sha256"
            | "llama.cpp.speculative_draft_sha256" => Some(SettingDefaultPreview::new(
                "None",
                SettingDefaultSource::Norted,
            )),
            _ => None,
        },
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
        "temperature" => &["--temp"],
        "top_p" => &["--top-p"],
        "top_k" => &["--top-k"],
        "min_p" => &["--min-p"],
        "seed" => &["--seed"],
        "repeat_penalty" => &["--repeat-penalty"],
        "presence_penalty" => &["--presence-penalty"],
        "frequency_penalty" => &["--frequency-penalty"],
        "max_output_tokens" => &["--predict"],
        "stop_strings" => &["--reverse-prompt"],
        "reasoning" => &["--reasoning"],
        "reasoning_effort" => &["--reasoning-effort"],
        "reasoning_budget" => &["--reasoning-budget"],
        "reasoning_budget_message" => &["--reasoning-budget-message"],
        "structured_output_schema" => &["--json-schema"],
        "context_overflow" => &["--ctx-size"],
        "llama.cpp.threads" => &["--threads"],
        "llama.cpp.batch_size" => &["--batch-size"],
        "llama.cpp.micro_batch_size" => &["--ubatch-size"],
        "llama.cpp.gpu_offload" => &["--n-gpu-layers"],
        "llama.cpp.flash_attention" => &["--flash-attn"],
        "llama.cpp.kv_cache_k" => &["--cache-type-k"],
        "llama.cpp.kv_cache_v" => &["--cache-type-v"],
        "llama.cpp.load_mode" => &["--load-mode"],
        "llama.cpp.rope_frequency_base" => &["--rope-freq-base"],
        "llama.cpp.rope_frequency_scale" => &["--rope-freq-scale"],
        "llama.cpp.unified_kv_cache" => &["--kv-unified", "--no-kv-unified"],
        "llama.cpp.kv_cache_gpu_offload" => &["--kv-offload", "--no-kv-offload"],
        "llama.cpp.context_checkpoints" => &["--ctx-checkpoints"],
        "llama.cpp.cpu_moe_layers" => &["--n-cpu-moe"],
        "llama.cpp.cpu_moe_all" => &["--cpu-moe"],
        "llama.cpp.active_experts" => &["--override-kv"],
        "llama.cpp.chat_template" => &["--chat-template"],
        "llama.cpp.chat_template_file" | "llama.cpp.chat_template_sha256" => {
            &["--jinja", "--chat-template-file"]
        }
        "llama.cpp.speculative_mode" => &["--spec-type"],
        "llama.cpp.speculative_draft_model" | "llama.cpp.speculative_draft_sha256" => {
            &["--spec-draft-model"]
        }
        _ => &[],
    }
}

fn llama_setting_has_execution_path(id: &str) -> bool {
    matches!(
        id,
        "context_length"
            | "parallel_requests"
            | "temperature"
            | "top_p"
            | "top_k"
            | "min_p"
            | "seed"
            | "repeat_penalty"
            | "presence_penalty"
            | "frequency_penalty"
            | "max_output_tokens"
            | "stop_strings"
            | "system_prompt"
            | "reasoning"
            | "reasoning_effort"
            | "reasoning_budget"
            | "reasoning_budget_message"
            | "structured_output_schema"
            | "context_overflow"
            | "llama.cpp.threads"
            | "llama.cpp.batch_size"
            | "llama.cpp.micro_batch_size"
            | "llama.cpp.gpu_offload"
            | "llama.cpp.flash_attention"
            | "llama.cpp.kv_cache_k"
            | "llama.cpp.kv_cache_v"
            | "llama.cpp.load_mode"
            | "llama.cpp.rope_frequency_base"
            | "llama.cpp.rope_frequency_scale"
            | "llama.cpp.unified_kv_cache"
            | "llama.cpp.kv_cache_gpu_offload"
            | "llama.cpp.context_checkpoints"
            | "llama.cpp.cpu_moe_layers"
            | "llama.cpp.cpu_moe_all"
            | "llama.cpp.active_experts"
            | "llama.cpp.chat_template"
            | "llama.cpp.chat_template_file"
            | "llama.cpp.chat_template_sha256"
            | "llama.cpp.speculative_mode"
            | "llama.cpp.speculative_draft_model"
            | "llama.cpp.speculative_draft_sha256"
    )
}

fn apply_llama_exact_help_contract(definitions: &mut [SettingDefinition], help: &str) {
    for definition in definitions.iter_mut() {
        if !llama_setting_has_execution_path(definition.id.as_str()) {
            definition.supported = false;
            definition.unsupported_reason = Some(
                "the Norted llama.cpp adapter has no executable path for this setting".to_owned(),
            );
            continue;
        }
        let requirements = llama_setting_contract(definition.id.as_str());
        let missing = requirements
            .iter()
            .copied()
            .filter(|required| !help_has_option(help, required))
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
            .map(|option| help_option_context(help, option))
            .unwrap_or_default();
        match definition.id.as_str() {
            "system_prompt" => {}
            "reasoning_effort" => {
                let choices = ["minimal", "low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .filter(|choice| text_has_value(&contract, choice))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise any supported reasoning-effort levels"
                            .to_owned(),
                    );
                } else {
                    definition.kind = SettingKind::Choice { choices };
                }
            }
            "llama.cpp.chat_template" => {
                let block = help_option_block(help, "--chat-template");
                let choices = advertised_values_with_prefixes(&block, &[])
                    .into_iter()
                    .filter(|value| !value.starts_with('-') && value != "env")
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise finite built-in chat-template choices"
                            .to_owned(),
                    );
                } else {
                    definition.kind = SettingKind::Choice { choices };
                }
            }
            "llama.cpp.speculative_mode" => {
                let block = help_option_block(help, "--spec-type");
                let mut choices =
                    advertised_values_with_prefixes(&block, &["none", "draft-", "ngram-"]);
                for choice in &mut choices {
                    if choice == "none" {
                        *choice = "off".to_owned();
                    }
                }
                choices.sort();
                choices.dedup();
                if choices.is_empty() {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise speculative decoding modes"
                            .to_owned(),
                    );
                } else {
                    definition.kind = SettingKind::Choice { choices };
                }
            }
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
                    definition.kind = SettingKind::Choice { choices };
                }
            }
            "llama.cpp.load_mode" => {
                let choices = advertised_llama_load_modes(help);
                if choices.is_empty() {
                    definition.supported = false;
                    definition.unsupported_reason = Some(
                        "the exact llama-server does not advertise any Norted-understood load modes"
                            .to_owned(),
                    );
                } else {
                    definition.kind = SettingKind::Choice { choices };
                }
            }
            _ => {}
        }
        if definition.supported
            && let Some(option) = requirements.first()
        {
            apply_llama_reported_default(definition, &help_option_block(help, option));
        }
    }
    finalize_llama_exact_schema(definitions);
}

fn finalize_llama_exact_schema(definitions: &mut [SettingDefinition]) {
    for definition in definitions
        .iter_mut()
        .filter(|definition| definition.supported && definition.default_preview.is_none())
    {
        definition.supported = false;
        definition.unsupported_reason = Some(
            "the exact llama-server advertises this control but does not expose a trustworthy omitted/default value"
                .to_owned(),
        );
    }
}

fn apply_llama_reported_default(definition: &mut SettingDefinition, contract: &str) {
    if definition.default_preview.as_ref().is_some_and(|preview| {
        matches!(
            preview.source,
            SettingDefaultSource::Norted | SettingDefaultSource::Model
        )
    }) {
        return;
    }
    let Some(reported) = llama_help_reported_default(contract) else {
        return;
    };
    let id = definition.id.as_str();
    let lower = reported.to_ascii_lowercase();
    let preview = match id {
        "parallel_requests" if lower == "-1" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail(
            "The exact runtime reports `-1 = auto` and finalizes its server slot count during startup",
        ),
        "llama.cpp.threads" if matches!(lower.as_str(), "-1" | "0" | "auto") => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(format!(
                "The exact runtime reports `{reported}` and resolves the worker count from the launch host"
            ))
        }
        "llama.cpp.gpu_offload" if matches!(lower.as_str(), "-1" | "auto") => {
            SettingDefaultPreview::new(
                "auto",
                SettingDefaultSource::Runtime,
            )
            .with_detail(format!(
                "The exact runtime reports `{reported}` and resolves offload from model and accelerator capacity"
            ))
        }
        "llama.cpp.flash_attention" if lower == "auto" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail("The exact runtime selects Flash Attention after inspecting model/backend support"),
        "llama.cpp.load_mode" if lower == "auto" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail(
            "The exact runtime reports `auto` and selects its model loading strategy during startup",
        ),
        "context_length" if matches!(lower.as_str(), "0" | "auto") => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime defers context selection to model metadata and its runtime fallback")
        }
        "llama.cpp.rope_frequency_base" | "llama.cpp.rope_frequency_scale"
            if matches!(lower.as_str(), "0" | "auto") =>
        {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime defers RoPE selection to model metadata and its runtime fallback")
        }
        "seed" if matches!(lower.as_str(), "-1" | "random") => {
            SettingDefaultPreview::new("random", SettingDefaultSource::Runtime)
        }
        "max_output_tokens" if lower == "-1" => {
            SettingDefaultPreview::new("unlimited", SettingDefaultSource::Runtime)
        }
        "reasoning" if lower == "auto" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail(
            "The exact runtime detects the reasoning mode from the selected chat template",
        ),
        "reasoning_effort" if lower == "default" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail("The exact runtime preserves the selected chat template's reasoning policy"),
        "llama.cpp.speculative_mode" if lower == "none" => {
            SettingDefaultPreview::new("off", SettingDefaultSource::Runtime)
        }
        "llama.cpp.chat_template" if matches!(lower.as_str(), "model" | "auto" | "none") => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime selects the model-provided template when available")
        }
        _ if lower == "default" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail("The exact runtime retains its internal policy for this setting"),
        _ if matches!(definition.kind, SettingKind::Toggle | SettingKind::OneWayFlag) => {
            let value = match lower.as_str() {
                "1" | "true" | "on" | "yes" | "enabled" => "enabled",
                "0" | "false" | "off" | "no" | "disabled" | "none" => "disabled",
                _ => reported.as_str(),
            };
            SettingDefaultPreview::new(value, SettingDefaultSource::Runtime)
        }
        _ => SettingDefaultPreview::new(reported, SettingDefaultSource::Runtime),
    };
    definition.default_preview = Some(if preview.detail.is_some() {
        preview
    } else {
        preview.with_detail("Reported by this exact llama-server help contract")
    });
}

fn llama_help_reported_default(contract: &str) -> Option<String> {
    let lower = contract.to_ascii_lowercase();
    let (index, marker_len) = ["default:", "default =", "default="]
        .into_iter()
        .filter_map(|marker| lower.find(marker).map(|index| (index, marker.len())))
        .min_by_key(|(index, _)| *index)?;
    let tail = contract[index + marker_len..].trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '(' | '[' | '{' | '`' | '\'' | '"')
    });
    let value = tail
        .split(|character: char| {
            character.is_whitespace()
                || matches!(character, ')' | ']' | '}' | ',' | ';' | '`' | '\'' | '"')
        })
        .next()?
        .trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn llama_configured_runtime_compatibility(
    settings: &norted_core::ResolvedSettings,
    model: Option<&ModelArtifact>,
    exact_help: Option<&str>,
) -> RuntimeCompatibility {
    let configured = settings
        .configured
        .keys()
        .filter(|id| id.applies_to_engine(ENGINE_ID))
        .collect::<Vec<_>>();
    if configured.is_empty() {
        return RuntimeCompatibility::Compatible;
    }
    if let Err(error) = validate_llama_semantic_settings(settings) {
        return RuntimeCompatibility::Incompatible(error.to_string());
    }
    if let Some(id) = configured
        .iter()
        .find(|id| !llama_setting_has_execution_path(id.as_str()))
    {
        return RuntimeCompatibility::Incompatible(format!(
            "llama.cpp setting `{id}` has no executable adapter path"
        ));
    }
    let Some(help) = exact_help else {
        return RuntimeCompatibility::NeedsAttention(
            "configured llama.cpp settings require exact executable help-contract validation"
                .to_owned(),
        );
    };
    let mut definitions = llama_model_setting_definitions(model);
    apply_llama_exact_help_contract(&mut definitions, help);
    let definitions = configurable_setting_definitions(definitions);
    let schema = SettingsSchema {
        engine_id: ENGINE_ID.to_owned(),
        runtime_id: None,
        definitions,
    };
    match schema.validate(settings) {
        Ok(())
            if matches!(
                settings.value("llama.cpp.speculative_mode"),
                Some(SettingValue::Choice(mode)) if mode == "draft-mtp"
            ) =>
        {
            RuntimeCompatibility::NeedsAttention(
                "draft-mtp uses MTP heads from the main model, but Norted's bounded GGUF identity cannot statically prove that the target exposes usable MTP heads; exact llama-server load remains authoritative"
                    .to_owned(),
            )
        }
        Ok(()) if settings.value("llama.cpp.speculative_draft_model").is_some() => {
            RuntimeCompatibility::NeedsAttention(
                "draft tokenizer identity is checked by Norted; the exact llama-server launch remains authoritative for draft architecture/tensor compatibility"
                    .to_owned(),
            )
        }
        Ok(()) => RuntimeCompatibility::Compatible,
        Err(error) => RuntimeCompatibility::Incompatible(error.to_string()),
    }
}

fn llama_normalized_generation_defaults(
    settings: &norted_core::ResolvedSettings,
) -> BTreeMap<String, Value> {
    ["temperature", "top_p", "top_k", "min_p"]
        .into_iter()
        .filter_map(|id| {
            let value = match settings.value(id)? {
                SettingValue::Float(value) => json!(value),
                SettingValue::UnsignedInteger(value) => json!(value),
                _ => return None,
            };
            Some((format!("configured_{id}"), value))
        })
        .collect()
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

fn advertised_values_with_prefixes(text: &str, prefixes: &[&str]) -> Vec<String> {
    if prefixes.is_empty()
        && let Some((_, values)) = text.split_once("list of built-in templates:")
    {
        let values = values.split("(env:").next().unwrap_or(values);
        return values
            .split(',')
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
            .map(str::to_owned)
            .collect();
    }
    text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                ',' | '|' | '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | ':' | ';' | '\'' | '"'
            )
    })
    .map(|value| value.trim_matches('.'))
    .filter(|value| {
        prefixes
            .iter()
            .any(|prefix| *value == *prefix || value.starts_with(prefix))
    })
    .filter(|value| {
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
    .map(str::to_owned)
    .collect()
}

#[derive(Debug)]
struct LlamaStructuredArguments {
    arguments: Vec<OsString>,
    environment_remove: Vec<OsString>,
}

fn validate_llama_semantic_settings(
    settings: &norted_core::ResolvedSettings,
) -> Result<(), EngineError> {
    if settings.value("llama.cpp.chat_template").is_some()
        && settings.value("llama.cpp.chat_template_file").is_some()
    {
        return Err(EngineError::InvalidConfiguration(
            "choose either a built-in llama.cpp chat template or a bound template file, not both"
                .to_owned(),
        ));
    }
    if settings.value("llama.cpp.cpu_moe_all").is_some()
        && settings.value("llama.cpp.cpu_moe_layers").is_some()
    {
        return Err(EngineError::InvalidConfiguration(
            "choose either all MoE weights on CPU or an exact CPU MoE layer count, not both"
                .to_owned(),
        ));
    }

    let mode = match settings.value("llama.cpp.speculative_mode") {
        Some(SettingValue::Choice(value)) => Some(value.as_str()),
        _ => None,
    };
    let has_draft = matches!(
        settings.value("llama.cpp.speculative_draft_model"),
        Some(SettingValue::Path(_))
    );
    if mode == Some("off") && has_draft {
        return Err(EngineError::InvalidConfiguration(
            "speculative decoding is off but a draft model is configured".to_owned(),
        ));
    }
    if mode.is_none() && has_draft {
        return Err(EngineError::InvalidConfiguration(
            "a speculative draft model requires an explicit speculative mode".to_owned(),
        ));
    }
    if mode.is_some_and(|mode| mode.starts_with("ngram-")) && has_draft {
        return Err(EngineError::InvalidConfiguration(
            "n-gram speculative modes do not use a draft-model artifact".to_owned(),
        ));
    }
    if mode == Some("draft-mtp") && has_draft {
        return Err(EngineError::InvalidConfiguration(
            "speculative mode `draft-mtp` uses MTP heads from the main model and cannot be combined with an external draft-model artifact"
                .to_owned(),
        ));
    }
    if mode.is_some_and(|mode| {
        matches!(
            mode,
            "draft-simple" | "draft-eagle3" | "draft-dflash" | "draft-dspark"
        )
    }) && !has_draft
    {
        return Err(EngineError::InvalidConfiguration(format!(
            "speculative mode `{}` requires a bound draft model",
            mode.expect("checked")
        )));
    }
    Ok(())
}

async fn validate_llama_bound_files(
    settings: &norted_core::ResolvedSettings,
    model: &ModelArtifact,
) -> Result<(), EngineError> {
    validate_llama_semantic_settings(settings)?;
    verify_bound_file(
        settings,
        "llama.cpp.chat_template_file",
        "llama.cpp.chat_template_sha256",
        4 * 1024 * 1024,
    )
    .await?;
    if let Some(SettingValue::Path(path)) = settings.value("llama.cpp.chat_template_file") {
        let bytes = tokio::fs::read(path).await.map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not read bound chat template {}: {error}",
                path.display()
            ))
        })?;
        if bytes.is_empty() || std::str::from_utf8(&bytes).is_err() {
            return Err(EngineError::InvalidConfiguration(
                "bound llama.cpp chat template must be non-empty UTF-8".to_owned(),
            ));
        }
    }
    verify_bound_file(
        settings,
        "llama.cpp.speculative_draft_model",
        "llama.cpp.speculative_draft_sha256",
        1024_u64 * 1024 * 1024 * 1024,
    )
    .await?;

    let draft_path = match settings.value("llama.cpp.speculative_draft_model") {
        Some(SettingValue::Path(path)) => Some(path),
        _ => None,
    };
    if let Some(path) = draft_path {
        let draft = norted_core::inspect_gguf_metadata(path).map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "speculative draft model {} is not a valid bounded GGUF: {error}",
                path.display()
            ))
        })?;
        let target = match &model.native_identity {
            Some(ArtifactNativeIdentity::Gguf(identity)) => identity,
            _ => {
                return Err(EngineError::InvalidConfiguration(
                    "the target model lacks bounded GGUF tokenizer identity needed to prove draft compatibility"
                        .to_owned(),
                ));
            }
        };
        match (
            target.tokenizer_metadata_sha256.as_deref(),
            draft.tokenizer_metadata_sha256.as_deref(),
        ) {
            (Some(target), Some(draft)) if target == draft => {}
            _ => {
                return Err(EngineError::InvalidConfiguration(
                    "the draft and target GGUF files do not prove identical tokenizer metadata"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(())
}

async fn verify_bound_file(
    settings: &norted_core::ResolvedSettings,
    path_setting: &str,
    sha_setting: &str,
    maximum_bytes: u64,
) -> Result<(), EngineError> {
    let Some(SettingValue::Path(path)) = settings.value(path_setting) else {
        if settings.value(sha_setting).is_some() {
            return Err(EngineError::InvalidConfiguration(format!(
                "`{sha_setting}` requires `{path_setting}`"
            )));
        }
        return Ok(());
    };
    let Some(SettingValue::String(expected)) = settings.value(sha_setting) else {
        return Err(EngineError::InvalidConfiguration(format!(
            "`{path_setting}` requires recorded `{sha_setting}` identity"
        )));
    };
    let id = SettingId::new(path_setting).expect("static setting ID");
    let observed =
        norted_core::bounded_setting_file_sha256(&id, path, Path::new("/"), maximum_bytes)
            .await
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
    if !observed.eq_ignore_ascii_case(expected) {
        return Err(EngineError::InvalidConfiguration(format!(
            "bound file `{path_setting}` no longer matches recorded SHA-256 {expected}"
        )));
    }
    Ok(())
}

fn translate_llama_settings_for_model(
    settings: &norted_core::ResolvedSettings,
    model: Option<&ModelArtifact>,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<LlamaStructuredArguments, EngineError> {
    let mut arguments = Vec::new();
    let mut environment_remove = Vec::new();
    for (id, resolved) in &settings.configured {
        let (aliases, environment_names) = llama_setting_collision_contract(id.as_str());
        if let Some(argument) = find_native_option(native_arguments, aliases) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured setting `{id}` conflicts with native llama.cpp argument `{argument}`"
            )));
        }
        if let Some(name) = configured_environment.keys().find(|name| {
            environment_names
                .iter()
                .any(|owned| name.eq_ignore_ascii_case(owned))
        }) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured setting `{id}` conflicts with configured llama.cpp environment variable `{name}`"
            )));
        }
        environment_remove.extend(environment_names.iter().map(OsString::from));
        match (id.as_str(), &resolved.value) {
            ("context_length", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ctx-size", *value);
            }
            ("parallel_requests", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--parallel", *value);
            }
            ("temperature", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--temp", *value);
            }
            ("top_p", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--top-p", *value);
            }
            ("top_k", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--top-k", *value);
            }
            ("min_p", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--min-p", *value);
            }
            ("seed", SettingValue::UnsignedIntegerOrChoice(value)) => {
                let value = match value {
                    UnsignedIntegerOrChoiceValue::UnsignedInteger(value) => value.to_string(),
                    UnsignedIntegerOrChoiceValue::Choice(value) if value == "random" => {
                        "-1".to_owned()
                    }
                    _ => {
                        return Err(EngineError::InvalidConfiguration(
                            "seed must be a 32-bit integer or `random`".to_owned(),
                        ));
                    }
                };
                push_value_argument(&mut arguments, "--seed", value);
            }
            ("repeat_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--repeat-penalty", *value);
            }
            ("presence_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--presence-penalty", *value);
            }
            ("frequency_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--frequency-penalty", *value);
            }
            ("max_output_tokens", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--predict", *value);
            }
            ("stop_strings", SettingValue::StringList(values)) => {
                for value in values {
                    push_value_argument(&mut arguments, "--reverse-prompt", value);
                }
            }
            ("reasoning", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--reasoning", value);
            }
            ("reasoning_effort", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--reasoning-effort", value);
            }
            ("reasoning_budget", SettingValue::Integer(value)) => {
                push_value_argument(&mut arguments, "--reasoning-budget", *value);
            }
            ("reasoning_budget_message", SettingValue::String(value)) => {
                push_value_argument(&mut arguments, "--reasoning-budget-message", value);
            }
            ("structured_output_schema", SettingValue::Json(_)) => {}
            ("system_prompt" | "context_overflow", _) => {}
            ("llama.cpp.threads", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--threads", *value);
            }
            ("llama.cpp.batch_size", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--batch-size", *value);
            }
            ("llama.cpp.micro_batch_size", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ubatch-size", *value);
            }
            ("llama.cpp.gpu_offload", SettingValue::GpuOffload(value)) => {
                arguments.push(OsString::from("--n-gpu-layers"));
                arguments.push(OsString::from(match value {
                    GpuOffload::None => "0".to_owned(),
                    GpuOffload::Auto => "auto".to_owned(),
                    GpuOffload::All => "all".to_owned(),
                    GpuOffload::Layers(value) => value.to_string(),
                }));
            }
            ("llama.cpp.flash_attention", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--flash-attn", value);
            }
            ("llama.cpp.kv_cache_k", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--cache-type-k", value);
            }
            ("llama.cpp.kv_cache_v", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--cache-type-v", value);
            }
            ("llama.cpp.load_mode", SettingValue::Choice(value)) => {
                arguments.push(OsString::from("--load-mode"));
                arguments.push(OsString::from(value));
            }
            ("llama.cpp.rope_frequency_base", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--rope-freq-base", *value);
            }
            ("llama.cpp.rope_frequency_scale", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--rope-freq-scale", *value);
            }
            ("llama.cpp.unified_kv_cache", SettingValue::Toggle(value)) => {
                arguments.push(OsString::from(if *value {
                    "--kv-unified"
                } else {
                    "--no-kv-unified"
                }));
            }
            ("llama.cpp.kv_cache_gpu_offload", SettingValue::Toggle(value)) => {
                arguments.push(OsString::from(if *value {
                    "--kv-offload"
                } else {
                    "--no-kv-offload"
                }));
            }
            ("llama.cpp.context_checkpoints", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ctx-checkpoints", *value);
            }
            ("llama.cpp.cpu_moe_layers", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--n-cpu-moe", *value);
            }
            ("llama.cpp.cpu_moe_all", SettingValue::FlagEnabled) => {
                arguments.push(OsString::from("--cpu-moe"));
            }
            ("llama.cpp.active_experts", SettingValue::UnsignedInteger(value)) => {
                let architecture = match model.and_then(|model| match &model.native_identity {
                    Some(ArtifactNativeIdentity::Gguf(identity))
                        if identity.expert_used_count.is_some() =>
                    {
                        Some(&identity.architecture)
                    }
                    _ => None,
                }) {
                    Some(architecture) => architecture,
                    _ => {
                        return Err(EngineError::InvalidConfiguration(
                            "active-expert override lacks proven GGUF architecture metadata"
                                .to_owned(),
                        ));
                    }
                };
                push_value_argument(
                    &mut arguments,
                    "--override-kv",
                    format!("{architecture}.expert_used_count=int:{value}"),
                );
            }
            ("llama.cpp.chat_template", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--chat-template", value);
            }
            ("llama.cpp.chat_template_file", SettingValue::Path(value)) => {
                arguments.push(OsString::from("--jinja"));
                arguments.push(OsString::from("--chat-template-file"));
                arguments.push(value.as_os_str().to_owned());
            }
            ("llama.cpp.chat_template_sha256", SettingValue::String(_)) => {}
            ("llama.cpp.speculative_mode", SettingValue::Choice(value)) => {
                push_value_argument(
                    &mut arguments,
                    "--spec-type",
                    if value == "off" { "none" } else { value },
                );
            }
            ("llama.cpp.speculative_draft_model", SettingValue::Path(value)) => {
                arguments.push(OsString::from("--spec-draft-model"));
                arguments.push(value.as_os_str().to_owned());
            }
            ("llama.cpp.speculative_draft_sha256", SettingValue::String(_)) => {}
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has an invalid value for llama.cpp"
                )));
            }
        }
    }
    Ok(LlamaStructuredArguments {
        arguments,
        environment_remove,
    })
}

#[cfg(test)]
fn translate_llama_settings(
    settings: &norted_core::ResolvedSettings,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<LlamaStructuredArguments, EngineError> {
    translate_llama_settings_for_model(settings, None, native_arguments, configured_environment)
}

fn llama_setting_collision_contract(
    id: &str,
) -> (&'static [&'static str], &'static [&'static str]) {
    match id {
        "context_length" => (&["-c", "--ctx-size"], &["LLAMA_ARG_CTX_SIZE"]),
        "parallel_requests" => (&["-np", "--parallel"], &["LLAMA_ARG_N_PARALLEL"]),
        "temperature" => (&["--temp"], &["LLAMA_ARG_TEMP"]),
        "top_p" => (&["--top-p"], &["LLAMA_ARG_TOP_P"]),
        "top_k" => (&["--top-k"], &["LLAMA_ARG_TOP_K"]),
        "min_p" => (&["--min-p"], &["LLAMA_ARG_MIN_P"]),
        "seed" => (&["-s", "--seed"], &[]),
        "repeat_penalty" => (&["--repeat-penalty"], &[]),
        "presence_penalty" => (&["--presence-penalty"], &[]),
        "frequency_penalty" => (&["--frequency-penalty"], &[]),
        "max_output_tokens" => (
            &["-n", "--predict", "--n-predict"],
            &["LLAMA_ARG_N_PREDICT"],
        ),
        "stop_strings" => (&["-r", "--reverse-prompt"], &[]),
        "system_prompt" | "context_overflow" => (&[], &[]),
        "reasoning" => (&["-rea", "--reasoning"], &["LLAMA_ARG_REASONING"]),
        "reasoning_effort" => (&["--reasoning-effort"], &["LLAMA_ARG_REASONING_EFFORT"]),
        "reasoning_budget" => (&["--reasoning-budget"], &["LLAMA_ARG_THINK_BUDGET"]),
        "reasoning_budget_message" => (
            &["--reasoning-budget-message"],
            &["LLAMA_ARG_THINK_BUDGET_MESSAGE"],
        ),
        "structured_output_schema" => (
            &[
                "-j",
                "--json-schema",
                "-jf",
                "--json-schema-file",
                "--grammar",
                "--grammar-file",
            ],
            &[],
        ),
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
        "llama.cpp.rope_frequency_base" => (&["--rope-freq-base"], &["LLAMA_ARG_ROPE_FREQ_BASE"]),
        "llama.cpp.rope_frequency_scale" => {
            (&["--rope-freq-scale"], &["LLAMA_ARG_ROPE_FREQ_SCALE"])
        }
        "llama.cpp.unified_kv_cache" => (
            &["-kvu", "--kv-unified", "-no-kvu", "--no-kv-unified"],
            &["LLAMA_ARG_KV_UNIFIED"],
        ),
        "llama.cpp.kv_cache_gpu_offload" => (
            &["-kvo", "--kv-offload", "-nkvo", "--no-kv-offload"],
            &["LLAMA_ARG_KV_OFFLOAD"],
        ),
        "llama.cpp.context_checkpoints" => (
            &["-ctxcp", "--ctx-checkpoints", "--swa-checkpoints"],
            &["LLAMA_ARG_CTX_CHECKPOINTS"],
        ),
        "llama.cpp.cpu_moe_layers" => (&["-ncmoe", "--n-cpu-moe"], &["LLAMA_ARG_N_CPU_MOE"]),
        "llama.cpp.cpu_moe_all" => (&["-cmoe", "--cpu-moe"], &["LLAMA_ARG_CPU_MOE"]),
        "llama.cpp.active_experts" => (&["--override-kv"], &[]),
        "llama.cpp.chat_template" => (
            &["--chat-template", "--chat-template-file"],
            &["LLAMA_ARG_CHAT_TEMPLATE", "LLAMA_ARG_CHAT_TEMPLATE_FILE"],
        ),
        "llama.cpp.chat_template_file" | "llama.cpp.chat_template_sha256" => (
            &[
                "--jinja",
                "--no-jinja",
                "--chat-template",
                "--chat-template-file",
            ],
            &[
                "LLAMA_ARG_JINJA",
                "LLAMA_ARG_CHAT_TEMPLATE",
                "LLAMA_ARG_CHAT_TEMPLATE_FILE",
            ],
        ),
        "llama.cpp.speculative_mode" => (&["--spec-type"], &["LLAMA_ARG_SPEC_TYPE"]),
        "llama.cpp.speculative_draft_model" | "llama.cpp.speculative_draft_sha256" => (
            &["-md", "--model-draft", "--spec-draft-model"],
            &["LLAMA_ARG_SPEC_DRAFT_MODEL"],
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
mod recipe_update_tests {
    use norted_core::{
        RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeAcquisitionMethod, RuntimeIdentity,
        RuntimeManifest, RuntimePackageIdentity, RuntimeProbeObservation, RuntimeRequirements,
    };

    use super::*;

    fn identity(variant: &str, accelerator: &str) -> RuntimeIdentity {
        RuntimeIdentity {
            engine_id: ENGINE_ID.to_owned(),
            package_family: "llama-cpp-managed-source".to_owned(),
            version: "b12345".to_owned(),
            upstream_revision: Some("a".repeat(40)),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: accelerator.to_owned(),
            variant: variant.to_owned(),
            package: RuntimePackageIdentity {
                provider_id: LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID.to_owned(),
                repository: Some("ggml-org/llama.cpp".to_owned()),
                release_tag: Some("b12345".to_owned()),
                asset_id: None,
                asset_name: None,
                additional_assets: Vec::new(),
            },
        }
    }

    fn installed(identity: RuntimeIdentity) -> InstalledRuntime {
        let runtime_id = RuntimeId::from_identity(&identity);
        InstalledRuntime {
            manifest: RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
                runtime_id,
                identity,
                supported_formats: vec![ArtifactFormat::Gguf],
                supported_native_identities: Vec::new(),
                requirements: RuntimeRequirements::default(),
                acquisition_method: RuntimeAcquisitionMethod::SourceBuild,
                source_url: None,
                downloaded_archive_sha256: None,
                additional_downloaded_archive_sha256: Vec::new(),
                source_build: None,
                entrypoint: "llama-server".into(),
                entrypoint_sha256: "a".repeat(64),
                installed_at_unix: Some(1),
                probe: RuntimeProbeObservation {
                    compatible: true,
                    observed_engine_id: ENGINE_ID.to_owned(),
                    observed_version: Some("b12345".to_owned()),
                    observed_revision: None,
                    detail: "fixture".to_owned(),
                    observed_at_unix: 1,
                },
            },
            installation_root: PathBuf::new(),
        }
    }

    #[test]
    fn managed_cuda_recipe_generations_have_explicit_update_identity() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        for (variant, generation) in [
            ("managed-portable-v1", 1),
            ("managed-portable-v2", 2),
            ("managed-portable-v3", 3),
            ("managed-portable-v4", 4),
        ] {
            assert_eq!(
                adapter.runtime_variant_update_identity(&identity(variant, "cuda")),
                RuntimeVariantUpdateIdentity {
                    functional_variant: MANAGED_CUDA12_FUNCTIONAL_VARIANT.to_owned(),
                    source_recipe_generation: Some(generation),
                }
            );
        }
        for (variant, generation) in [
            ("managed-portable-cuda13-v1", 1),
            ("managed-portable-cuda13-v2", 2),
        ] {
            assert_eq!(
                adapter.runtime_variant_update_identity(&identity(variant, "cuda")),
                RuntimeVariantUpdateIdentity {
                    functional_variant: MANAGED_CUDA13_FUNCTIONAL_VARIANT.to_owned(),
                    source_recipe_generation: Some(generation),
                }
            );
        }
    }

    #[test]
    fn cuda13_is_an_intentional_parallel_functional_variant() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        let cuda12 =
            adapter.runtime_variant_update_identity(&identity("managed-portable-v4", "cuda"));
        let cuda13 = adapter
            .runtime_variant_update_identity(&identity("managed-portable-cuda13-v2", "cuda"));

        assert_eq!(cuda12.functional_variant, MANAGED_CUDA12_FUNCTIONAL_VARIANT);
        assert_eq!(cuda12.source_recipe_generation, Some(4));
        assert_eq!(cuda13.functional_variant, MANAGED_CUDA13_FUNCTIONAL_VARIANT);
        assert_eq!(cuda13.source_recipe_generation, Some(2));
        assert_ne!(cuda12.functional_variant, cuda13.functional_variant);
    }

    #[test]
    fn known_bad_v1_is_not_servable_but_retains_the_current_update_line() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        let v1_identity = identity("managed-portable-v1", "cuda");
        let current_identity = identity("managed-portable-v4", "cuda");
        let CompatibilityDecision::Unsupported { reason } =
            adapter.runtime_compatibility(&installed(v1_identity.clone()))
        else {
            panic!("managed V1 must fail closed");
        };
        assert!(reason.contains("known relocation defect"));

        let v1_update = adapter.runtime_variant_update_identity(&v1_identity);
        let current_update = adapter.runtime_variant_update_identity(&current_identity);
        assert_eq!(
            v1_update.functional_variant,
            current_update.functional_variant
        );
        assert!(current_update.source_recipe_generation > v1_update.source_recipe_generation);

        let mut external = v1_identity;
        external.package.provider_id = "external-runtime".to_owned();
        assert_eq!(
            adapter.runtime_compatibility(&installed(external)),
            CompatibilityDecision::Supported
        );
        assert_eq!(
            adapter.runtime_compatibility(&installed(identity("managed-portable-v1", "vulkan"))),
            CompatibilityDecision::Supported
        );
        assert_eq!(
            adapter.runtime_compatibility(&installed(identity("managed-portable-v1", "cpu"))),
            CompatibilityDecision::Supported
        );
        let mut windows_cuda = identity("managed-portable-v1", "cuda");
        windows_cuda.platform = "windows".to_owned();
        assert_eq!(
            adapter.runtime_compatibility(&installed(windows_cuda)),
            CompatibilityDecision::Supported
        );
        let mut official = identity("managed-portable-v1", "cuda");
        official.package_family = "llama-cpp-official-release".to_owned();
        official.package.provider_id = LLAMA_CPP_RUNTIME_PROVIDER_ID.to_owned();
        assert_eq!(
            adapter.runtime_compatibility(&installed(official)),
            CompatibilityDecision::Supported
        );
        assert_eq!(
            adapter.runtime_compatibility(&installed(identity("unrelated-v1", "cuda"))),
            CompatibilityDecision::Supported
        );
    }

    #[test]
    fn unknown_generations_and_other_accelerators_do_not_cross_update() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        for runtime in [
            identity("managed-portable-v5", "cuda"),
            identity("managed-portable-v3", "vulkan"),
        ] {
            assert_eq!(
                adapter.runtime_variant_update_identity(&runtime),
                RuntimeVariantUpdateIdentity::exact(&runtime)
            );
        }
    }
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
    use norted_core::ModelProfileId;

    use super::*;

    fn request(generation_settings: GenerationSettingsPatch) -> InferenceRequest {
        InferenceRequest {
            model_profile_id: ModelProfileId::new("model").expect("profile ID"),
            messages: vec![InferenceMessage::text(InferenceRole::User, "hello")],
            generation_settings,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            output_format: None,
            max_output_tokens: Some(123),
            stream: false,
        }
    }

    #[test]
    fn request_time_sampler_fields_override_process_defaults_only_when_explicit() {
        let adapter = LlamaCppAdapter::from_config(None, Path::new("."));
        let omitted = adapter.backend_request(&request(GenerationSettingsPatch::default()), false);
        assert!(omitted.get("temperature").is_none());
        assert!(omitted.get("top_p").is_none());
        assert_eq!(omitted["max_completion_tokens"], 123);

        let explicit = adapter.backend_request(
            &request(GenerationSettingsPatch {
                temperature: Some(0.25),
                top_p: Some(0.8),
                ..Default::default()
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
                        ..Default::default()
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
                ..Default::default()
            },
            GenerationSettingsPatch {
                temperature: Some(2.1),
                ..Default::default()
            },
            GenerationSettingsPatch {
                top_p: Some(1.1),
                ..Default::default()
            },
            GenerationSettingsPatch {
                temperature: Some(f64::NAN),
                ..Default::default()
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
        assert!(matches!(
            adapter.validate_generation_settings(
                &GenerationSettingsPatch {
                    reasoning_effort: Some(norted_engine::ReasoningEffort::High),
                    ..Default::default()
                },
                &EffectiveGenerationSettings {
                    temperature: 0.8,
                    top_p: 0.95,
                },
            ),
            Ok(())
        ));
    }
}

#[cfg(test)]
mod settings_tests {
    use norted_core::{
        ModelProfileId, ResolvedSetting, ResolvedSettings, SettingSource, SettingsPatch,
        SettingsState,
    };

    use super::*;

    fn resolved(values: &[(&str, SettingValue)]) -> ResolvedSettings {
        let mut configured = BTreeMap::new();
        for (id, value) in values {
            configured.insert(
                SettingId::new(*id).expect("setting ID"),
                ResolvedSetting {
                    value: value.clone(),
                    source: SettingSource::Invocation,
                },
            );
        }
        ResolvedSettings {
            engine_id: ENGINE_ID.to_owned(),
            model_profile_id: None,
            configured,
            effective: BTreeMap::new(),
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
        let translated = translate_llama_settings(&resolved(&[]), &[], &BTreeMap::new())
            .expect("empty translation");
        assert!(translated.arguments.is_empty());
        assert!(translated.environment_remove.is_empty());
    }

    #[test]
    fn common_and_kv_settings_translate_independently() {
        let translated = translate_llama_settings(
            &resolved(&[
                ("context_length", SettingValue::UnsignedInteger(131_072)),
                ("parallel_requests", SettingValue::UnsignedInteger(3)),
                (
                    "llama.cpp.kv_cache_k",
                    SettingValue::Choice("q8_0".to_owned()),
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
    fn configured_generation_defaults_translate_to_exact_llama_controls() {
        let settings = resolved(&[
            ("temperature", SettingValue::Float(0.7)),
            ("top_p", SettingValue::Float(0.9)),
            ("top_k", SettingValue::UnsignedInteger(40)),
            ("min_p", SettingValue::Float(0.05)),
        ]);
        let translated = translate_llama_settings(&settings, &[], &BTreeMap::new())
            .expect("configured generation defaults");
        assert_eq!(
            strings(translated.arguments),
            [
                "--min-p", "0.05", "--temp", "0.7", "--top-k", "40", "--top-p", "0.9",
            ]
        );
        assert_eq!(
            llama_normalized_generation_defaults(&settings),
            BTreeMap::from([
                ("configured_min_p".to_owned(), json!(0.05)),
                ("configured_temperature".to_owned(), json!(0.7)),
                ("configured_top_k".to_owned(), json!(40)),
                ("configured_top_p".to_owned(), json!(0.9)),
            ])
        );
    }

    #[test]
    fn common_temperature_resolves_from_a_llama_model_profile_and_translates() {
        let profile_id = ModelProfileId::new("llama-quality").expect("profile ID");
        let temperature_id = SettingId::new("temperature").expect("temperature ID");
        let mut overrides = SettingsPatch::default();
        overrides.insert(temperature_id.clone(), SettingValue::Float(0.7));
        let resolved = SettingsState::default()
            .resolve(
                &profile_id,
                ENGINE_ID,
                &overrides,
                &SettingsPatch::default(),
                Path::new("/"),
            )
            .expect("profile setting resolution");
        assert!(matches!(
            resolved.configured[&temperature_id].source,
            SettingSource::ModelProfile { ref model_profile_id } if model_profile_id == &profile_id
        ));
        assert_eq!(
            strings(
                translate_llama_settings(&resolved, &[], &BTreeMap::new())
                    .expect("profile temperature translation")
                    .arguments
            ),
            ["--temp", "0.7"]
        );
    }

    #[test]
    fn omitted_generation_defaults_preserve_upstream_behavior() {
        let settings = resolved(&[]);
        assert!(
            translate_llama_settings(&settings, &[], &BTreeMap::new())
                .expect("omitted generation settings")
                .arguments
                .is_empty()
        );
        assert!(llama_normalized_generation_defaults(&settings).is_empty());
    }

    #[test]
    fn exact_generation_and_reasoning_controls_are_help_gated() {
        let configured = resolved(&[("temperature", SettingValue::Float(0.7))]);
        assert!(matches!(
            llama_configured_runtime_compatibility(&configured, None, None),
            RuntimeCompatibility::NeedsAttention(_)
        ));
        assert!(matches!(
            llama_configured_runtime_compatibility(
                &configured,
                None,
                Some("  --temp N  temperature")
            ),
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(matches!(
            llama_configured_runtime_compatibility(&configured, None, Some("  --top-p N  top p")),
            RuntimeCompatibility::Incompatible(_)
        ));

        let all_generation = resolved(&[
            ("temperature", SettingValue::Float(0.7)),
            ("top_p", SettingValue::Float(0.9)),
            ("top_k", SettingValue::UnsignedInteger(40)),
            ("min_p", SettingValue::Float(0.05)),
        ]);
        let generation_help =
            "  --temp N  temperature\n  --top-p N  top p\n  --top-k N  top k\n  --min-p N  min p";
        assert!(matches!(
            llama_configured_runtime_compatibility(&all_generation, None, Some(generation_help)),
            RuntimeCompatibility::Incompatible(_)
        ));
        let mut definitions = llama_model_setting_definitions(None);
        apply_llama_exact_help_contract(&mut definitions, "  --temp N  temperature");
        for id in ["top_p", "top_k", "min_p"] {
            assert!(
                !definitions
                    .iter()
                    .find(|definition| definition.id.as_str() == id)
                    .expect("generation definition")
                    .supported,
                "{id} must be unsupported without an exact advertised control"
            );
        }

        let reasoning = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "reasoning_effort")
            .expect("reasoning definition");
        assert!(!reasoning.supported);
        assert!(
            reasoning
                .unsupported_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("does not advertise"))
        );
    }

    #[test]
    fn every_supported_exact_schema_definition_has_an_execution_path() {
        for definition in llama_model_setting_definitions(None) {
            if definition.supported {
                assert!(
                    llama_setting_has_execution_path(definition.id.as_str()),
                    "supported llama.cpp setting {} has no execution path",
                    definition.id
                );
                assert!(
                    definition.id.as_str() == "system_prompt"
                        || !llama_setting_contract(definition.id.as_str()).is_empty(),
                    "supported llama.cpp setting {} has no exact-runtime contract",
                    definition.id
                );
            }
        }
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

        let translated = translate_llama_settings(
            &resolved(&[(
                "llama.cpp.load_mode",
                SettingValue::Choice("mmap+mlock".to_owned()),
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
            SettingValue::Choice("mmap".to_owned()),
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
                translate_llama_settings(&settings, &[argument.to_owned()], &BTreeMap::new())
                    .expect_err("native load-mode collision");
            assert!(error.to_string().contains(argument), "{error}");
        }
    }

    #[test]
    fn structured_load_mode_owns_equivalent_environment_only_when_active() {
        let settings = resolved(&[(
            "llama.cpp.load_mode",
            SettingValue::Choice("dio".to_owned()),
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
            let error = translate_llama_settings(
                &settings,
                &[],
                &BTreeMap::from([(name.to_owned(), "1".to_owned())]),
            )
            .expect_err("configured environment collision");
            assert!(error.to_string().contains(name), "{error}");
        }

        let translated =
            translate_llama_settings(&settings, &[], &BTreeMap::new()).expect("translation");
        assert_eq!(
            translated.environment_remove,
            equivalent.map(OsString::from)
        );

        let absent = translate_llama_settings(
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
        let settings = resolved(&[("context_length", SettingValue::UnsignedInteger(8192))]);
        for native in [
            vec!["--ctx-size=4096".to_owned()],
            vec!["-c".to_owned(), "4096".to_owned()],
        ] {
            let error = translate_llama_settings(&settings, &native, &BTreeMap::new())
                .expect_err("native collision");
            assert!(error.to_string().contains("conflicts"));
        }
    }

    #[test]
    fn malformed_values_are_rejected_by_the_definition() {
        let definition = llama_setting_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "context_length")
            .expect("context definition");
        assert!(definition.parse("0").is_err());
        assert!(definition.parse("many").is_err());
    }
}
