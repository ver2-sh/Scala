//! llama.cpp-specific launch, probe, health, and inference translation.

mod catalog;
mod chat;
mod prefill;
mod source_catalog;

pub use catalog::{LLAMA_CPP_RUNTIME_PROVIDER_ID, LlamaCppRuntimeCatalogProvider};
pub use source_catalog::{
    LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID, LlamaCppSourceRuntimeCatalogProvider,
};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use scala_core::{
    AcceleratorBinding, AcceleratorDevice, AcquisitionMethod, ArtifactFormat,
    ArtifactNativeIdentity, AvailableRuntime, EngineConfig, EngineInstallation, EngineRevision,
    GpuOffload, HostCapabilities, InstalledRuntime, ModelArtifact, RuntimeAcquisitionMethod,
    RuntimeCompatibility, RuntimeId, RuntimeIdentity, RuntimeProbeObservation,
    SettingDefaultPreview, SettingDefaultSource, SettingDefinition, SettingId, SettingKind,
    SettingScope, SettingValue, SettingsSchema, UnsignedIntegerOrChoiceValue,
};
use scala_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, InferenceActivityReporter,
    InferenceActivityUpdate, InferenceEvent, InferenceFinishReason, InferenceMessage,
    InferenceOutput, InferenceRequest, InferenceRole, InferenceStream, InferenceToolCall,
    InferenceToolChoice, InferenceUsage, InstallationState, LaunchRequest, LaunchSpec,
    LoadProgressReporter, NativeOption, OutputFormat, PreparedModelInput, ProcessDescriptor,
    RuntimeVariantUpdateIdentity, StartupObservation, UpdateState, capture_command,
    common_setting_definitions_for, compatibility_for, isolated_cuda_environment_for_binding,
    prepare_norted_package_input, prepare_norted_package_input_with_progress,
    revalidate_norted_package_before_launch, revalidate_norted_package_before_launch_with_progress,
    visible_nvidia_device_set,
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

// Reserve every environment alias owned by either Scala's private-backend
// contract or an engine-qualified setting. Unrelated process environment is
// still inherited.
const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "CUDA_VISIBLE_DEVICES",
    "LLAMA_ARG_MODEL",
    "LLAMA_ARG_LOG_JSONL",
    "LLAMA_ARG_MODEL_URL",
    "LLAMA_ARG_DOCKER_REPO",
    "LLAMA_ARG_HF_REPO",
    "LLAMA_ARG_HF_FILE",
    "HF_TOKEN",
    "LLAMA_ARG_ALIAS",
    "LLAMA_ARG_TAGS",
    "LLAMA_ARG_DEVICE",
    "LLAMA_ARG_RPC",
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
    "LLAMA_ARG_STATIC_PATH",
    "LLAMA_ARG_CORS_ORIGINS",
    "LLAMA_ARG_CORS_METHODS",
    "LLAMA_ARG_CORS_HEADERS",
    "LLAMA_ARG_CORS_CREDENTIALS",
    "LLAMA_ARG_UI_CONFIG",
    "LLAMA_ARG_UI_CONFIG_FILE",
    "LLAMA_ARG_UI_MCP_PROXY",
    "LLAMA_ARG_UI",
    "LLAMA_ARG_POOLING",
    "LLAMA_ARG_DEFRAG_THOLD",
    "LLAMA_ARG_MMPROJ",
    "LLAMA_ARG_MMPROJ_URL",
    "LLAMA_ARG_MMPROJ_AUTO",
    "LLAMA_ARG_MMPROJ_OFFLOAD",
    "MTMD_BACKEND_DEVICE",
    "LLAMA_ARG_IMAGE_MIN_TOKENS",
    "LLAMA_ARG_IMAGE_MAX_TOKENS",
    "LLAMA_ARG_MTMD_BATCH_MAX_TOKENS",
    "LLAMA_ARG_VIDEO_FPS",
    "LLAMA_ARG_VIDEO_TIMESTAMP_INTERVAL",
    "LLAMA_ARG_VIDEO_FFMPEG_DIR",
    "LLAMA_ARG_SPEC_DRAFT_HF_REPO",
    "LLAMA_ARG_SPEC_SYNTH_LEN",
    "LLAMA_ARG_SPEC_SYNTH_RATES",
    "LLAMA_ARG_DRAFT_MAX",
    "LLAMA_ARG_DRAFT_MIN",
    "LLAMA_SERVER_ROUTER_PORT",
    "LLAMA_SERVER_CHILD_MODE",
    "LLAMA_ARG_AGENT",
    "LLAMA_ARG_TOOLS",
    "LLAMA_ARG_TOOLS_RUNTIME",
    "LLAMA_ARG_MCP_CONFIG",
    "LLAMA_ARG_MCP_SERVERS_CONFIG",
    "LLAMA_ARG_MCP_SERVERS_JSON",
];

fn llama_model_compatibility(artifact: CompatibilityDecision) -> RuntimeCompatibility {
    match artifact {
        CompatibilityDecision::Supported => RuntimeCompatibility::Compatible,
        CompatibilityDecision::Unsupported { reason } => RuntimeCompatibility::Incompatible(reason),
    }
}

const MANAGED_NATIVE_ARGUMENTS: &[&str] = &[
    "-h",
    "--help",
    "--usage",
    // Preserve textual stderr used by startup progress and diagnostic parsing.
    "--log-jsonl",
    "--no-log-jsonl",
    "--version",
    "-cl",
    "--cache-list",
    "--completion-bash",
    "--server-base",
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
    "-hft",
    "--hf-token",
    "-a",
    "--alias",
    "--tags",
    "-dev",
    "--device",
    "--list-devices",
    "--rpc",
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
    "--path",
    "--cors-origins",
    "--cors-methods",
    "--cors-headers",
    "--cors-credentials",
    "--no-cors-credentials",
    "--ui-config",
    "--webui-config",
    "--ui-config-file",
    "--webui-config-file",
    "--ui-mcp-proxy",
    "--webui-mcp-proxy",
    "--no-ui-mcp-proxy",
    "--no-webui-mcp-proxy",
    "--ui",
    "--webui",
    "--no-ui",
    "--no-webui",
    "--pooling",
    "--embd-normalize",
    "-dt",
    "--defrag-thold",
    "--override-kv",
    "-mm",
    "--mmproj",
    "-mmu",
    "--mmproj-url",
    "--mmproj-auto",
    "--no-mmproj",
    "--no-mmproj-auto",
    "--mmproj-offload",
    "--no-mmproj-offload",
    "-mmdev",
    "--mmproj-device",
    "--image",
    "--audio",
    "--video",
    "--image-min-tokens",
    "--image-max-tokens",
    "--mtmd-batch-max-tokens",
    "--video-fps",
    "--video-timestamp-interval",
    "--video-ffmpeg-dir",
    "--media-path",
    "--agent",
    "-ag",
    "-no-ag",
    "--no-agent",
    "--tools",
    "--tools-runtime",
    "--tools-file",
    "--mcp-config",
    "--mcp-config-file",
    "--mcp-servers-config",
    "--mcp-servers-json",
    "--lookup-cache-static",
    "--lookup-cache-dynamic",
    "-lcs",
    "-lcd",
    "--spec-synth-len",
    "--spec-synth-rates",
    "--spec-draft-hf",
    "-hfd",
    "-hfrd",
    "--hf-repo-draft",
    "--spec-draft-device",
    "-devd",
    "--device-draft",
    "--sampler-seq",
    "--sampling-seq",
    "-l",
    "--logit-bias",
    "-v",
    "--verbose",
    "--log-verbose",
    "-sp",
    "--special",
    "--spm-infill",
    "--draft",
    "--draft-n",
    "--draft-max",
    "--draft-min",
    "--draft-n-min",
    "--spec-ngram-size-n",
    "--spec-ngram-size-m",
    "--spec-ngram-min-hits",
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
    "--spec-default",
];

pub struct LlamaCppAdapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    chat_proofs: std::sync::RwLock<BTreeMap<String, chat::LaunchProof>>,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
    /// Replaced before every launch attempt; activated only for its matching process.
    prompt_timing: tokio::sync::RwLock<BTreeMap<String, PromptTimingLaunch>>,
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
                    "environment variable `{name}` conflicts with the Scala-managed llama.cpp backend contract"
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
                    "native argument `{argument}` conflicts with the Scala-managed llama.cpp backend contract"
                ));
            }
            if configuration_error.is_none() && !native_arguments.is_empty() {
                configuration_error = Some(
                    "llama.cpp native arguments are disabled; use typed `llama.cpp.*` settings"
                        .to_owned(),
                );
            }
        }

        Self {
            enabled,
            binary_path,
            native_arguments,
            environment,
            configuration_error,
            client: reqwest::Client::new(),
            chat_proofs: std::sync::RwLock::new(BTreeMap::new()),
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
            prompt_timing: tokio::sync::RwLock::new(BTreeMap::new()),
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
        chat::request_fields(&mut body, request);
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
                OutputFormat::JsonObject => {
                    json!({ "type": "json_object", "schema": {"type": "object"} })
                }
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

    async fn startup_properties(
        &self,
        endpoint: &str,
    ) -> Result<LlamaStartupProperties, EngineError> {
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
        Ok(LlamaStartupProperties {
            context_length: props.default_generation_settings.n_ctx,
            parallel_requests: props.total_slots.filter(|slots| *slots > 0),
        })
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
            api: vec![
                ApiCapability::ChatCompletions,
                ApiCapability::Completions,
                ApiCapability::Embeddings,
            ],
            features: vec![
                EngineFeature::TextGeneration,
                EngineFeature::ToolCalling,
                EngineFeature::StructuredOutput,
            ],
        }
    }

    fn serving_features(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: Option<&scala_core::ResolvedSettings>,
    ) -> Vec<EngineFeature> {
        if pooled_embedding_model(model) {
            Vec::new()
        } else {
            self.chat_serving_features(runtime, model, settings)
        }
    }

    fn supports_model_capability(&self, model: &ModelArtifact, capability: ApiCapability) -> bool {
        let pooled = pooled_embedding_model(model);
        match capability {
            ApiCapability::Embeddings => pooled,
            ApiCapability::Completions
            | ApiCapability::ChatCompletions
            | ApiCapability::Responses => !pooled,
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
        chat::validate_request(request)?;
        let required = [
            (request.generation_settings.seed.is_some(), "llama.cpp.seed"),
            (
                request.generation_settings.repeat_penalty.is_some(),
                "llama.cpp.repeat_penalty",
            ),
            (
                request.generation_settings.presence_penalty.is_some(),
                "llama.cpp.presence_penalty",
            ),
            (
                request.generation_settings.frequency_penalty.is_some(),
                "llama.cpp.frequency_penalty",
            ),
            (
                request.generation_settings.stop.is_some(),
                "llama.cpp.stop_strings",
            ),
            (
                request.generation_settings.reasoning_effort.is_some(),
                "llama.cpp.reasoning_effort",
            ),
            (
                matches!(
                    request.output_format.as_ref(),
                    Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
                ),
                "llama.cpp.structured_output_schema",
            ),
        ];
        for (used, id) in required {
            if used {
                require_supported_setting(settings_schema, id)?;
            }
        }
        if let Some(effort) = request.generation_settings.reasoning_effort {
            let id = SettingId::new("llama.cpp.reasoning_effort").expect("static setting ID");
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
        Ok(Some(
            self.startup_properties(endpoint).await?.context_length,
        ))
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
        host: &HostCapabilities,
        _settings: Option<&scala_core::ResolvedSettings>,
    ) -> RuntimeCompatibility {
        let model_compatibility = llama_model_compatibility(self.compatibility(model));
        if matches!(model_compatibility, RuntimeCompatibility::Incompatible(_)) {
            return model_compatibility;
        }
        combine_llama_compatibility(
            model_compatibility,
            llama_device_evaluation(
                &runtime.manifest.identity.accelerator,
                &runtime.manifest.identity.platform,
                &runtime.manifest.identity.architecture,
                &runtime.manifest.requirements,
                host,
                None,
            )
            .compatibility,
        )
    }

    fn available_runtime_model_compatibility(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&scala_core::ResolvedSettings>,
    ) -> RuntimeCompatibility {
        let model_compatibility = llama_model_compatibility(self.compatibility(model));
        if matches!(model_compatibility, RuntimeCompatibility::Incompatible(_)) {
            return model_compatibility;
        }
        combine_llama_compatibility(
            model_compatibility,
            llama_device_evaluation(
                &runtime.identity.accelerator,
                &runtime.identity.platform,
                &runtime.identity.architecture,
                &runtime.requirements,
                host,
                None,
            )
            .compatibility,
        )
    }

    fn validate_configuration(
        &self,
        runtime: &InstalledRuntime,
        model: Option<&ModelArtifact>,
        host: &HostCapabilities,
        settings: &scala_core::ResolvedSettings,
    ) -> Result<(), EngineError> {
        let help = self
            .capability_cache
            .try_read()
            .ok()
            .and_then(|cache| cache.get(&Self::capability_key(runtime)).cloned());
        if let RuntimeCompatibility::Incompatible(reason) =
            llama_configured_runtime_compatibility(settings, model, help.as_deref())
        {
            return Err(EngineError::InvalidConfiguration(reason));
        }
        if let RuntimeCompatibility::Incompatible(reason) = llama_device_evaluation(
            &runtime.manifest.identity.accelerator,
            &runtime.manifest.identity.platform,
            &runtime.manifest.identity.architecture,
            &runtime.manifest.requirements,
            host,
            Some(settings),
        )
        .compatibility
        {
            return Err(EngineError::InvalidConfiguration(reason));
        }
        Ok(())
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

    fn runtime_model_accelerator_binding(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        host: &HostCapabilities,
        settings: Option<&scala_core::ResolvedSettings>,
    ) -> Option<AcceleratorBinding> {
        llama_device_evaluation(
            &runtime.manifest.identity.accelerator,
            &runtime.manifest.identity.platform,
            &runtime.manifest.identity.architecture,
            &runtime.manifest.requirements,
            host,
            settings,
        )
        .binding
    }

    fn normalize_settings(
        &self,
        settings: &mut scala_core::ResolvedSettings,
    ) -> Result<(), EngineError> {
        if settings.engine_id != ENGINE_ID {
            return Err(EngineError::InvalidConfiguration(format!(
                "resolved settings belong to `{}`, not `{ENGINE_ID}`",
                settings.engine_id
            )));
        }
        normalize_llama_semantic_alternatives(settings);
        Ok(())
    }

    fn native_options(&self) -> Vec<NativeOption> {
        Vec::new()
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
        Ok(llama_model_setting_definitions(Some(model)))
    }

    async fn runtime_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _host: &HostCapabilities,
        _settings: Option<&scala_core::ResolvedSettings>,
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
        _settings: Option<&scala_core::ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self.cached_runtime_help(runtime).await?;
        let mut definitions = self.model_setting_definitions(model)?;
        apply_llama_exact_help_contract(&mut definitions, &help);
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
        validate_llama_launch_help(&help)?;
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

    async fn build_launch_spec(
        &self,
        mut request: LaunchRequest,
    ) -> Result<LaunchSpec, EngineError> {
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
        let bound_files = bind_llama_inference_files(&mut request.settings).await?;
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
        let mut arguments = vec![
            OsString::from("--model"),
            request.model.primary.path.as_os_str().to_owned(),
            OsString::from("--alias"),
            OsString::from(public_model_id.as_str()),
            OsString::from("--host"),
            OsString::from(request.backend_address.ip().to_string()),
            OsString::from("--port"),
            OsString::from(request.backend_address.port().to_string()),
        ];
        if pooled_embedding_model(&request.model.primary) {
            let help = self.cached_runtime_help(&request.runtime).await?;
            if !help_has_option(&help, "--embedding") {
                return Err(EngineError::Unsupported(
                    "runtime does not advertise embedding mode".to_owned(),
                ));
            }
            arguments.push(OsString::from("--embedding"));
        }
        let mut environment = self.environment.clone();
        if manifest.identity.accelerator == "cuda" {
            let binding = request.accelerator_binding.as_ref().ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "llama.cpp CUDA launch has no exact NVIDIA GPU binding selected by compatibility evaluation"
                        .to_owned(),
                )
            })?;
            validate_llama_accelerator_settings(&request.settings, binding)?;
            let help = self.cached_runtime_help(&request.runtime).await?;
            if !help_has_option(&help, "--device") {
                return Err(EngineError::InvalidConfiguration(
                    "the exact CUDA llama-server does not advertise Scala's required --device isolation contract"
                        .to_owned(),
                ));
            }
            environment = isolated_cuda_environment_for_binding(&environment, binding, "llama.cpp")
                .map_err(EngineError::InvalidConfiguration)?;
            let visible_devices = (0..binding.devices.len())
                .map(|index| format!("CUDA{index}"))
                .collect::<Vec<_>>()
                .join(",");
            arguments.extend([OsString::from("--device"), OsString::from(visible_devices)]);
        }
        arguments.extend(structured.arguments);
        arguments.extend(self.native_arguments.iter().map(OsString::from));
        let mut environment_remove = managed_environment_removals();
        environment_remove.extend(structured.environment_remove);
        let mut normalized_settings = llama_normalized_generation_defaults(&request.settings);
        normalized_settings.extend(bound_files);
        Ok(LaunchSpec {
            executable: binary_path,
            arguments,
            environment,
            environment_remove,
            inherits_parent_environment: true,
            working_directory: None,
            temporary_files: Vec::new(),
            endpoint: Some(http_endpoint(request.backend_address)),
            normalized_settings,
            settings: request.settings,
            native_arguments: self.native_arguments.clone(),
            installation: (*installation).clone(),
            runtime: request.runtime,
            model: request.model,
            accelerator_binding: request.accelerator_binding,
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
        self.prepare_prompt_timing(spec).await;
        revalidate_norted_package_before_launch(&spec.model).await
    }

    async fn prepare_launch_attempt_with_progress(
        &self,
        spec: &LaunchSpec,
        progress: LoadProgressReporter,
    ) -> Result<(), EngineError> {
        self.prepare_prompt_timing(spec).await;
        revalidate_norted_package_before_launch_with_progress(&spec.model, &progress).await
    }

    async fn confirm_request_stopped(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<bool, EngineError> {
        let endpoint = process
            .endpoint
            .as_deref()
            .ok_or_else(|| EngineError::Operation("missing endpoint".into()))?;
        // Dropping the SSE body closes its HTTP connection. llama-server's
        // response reader cancels its tasks on destruction. /slots is handled
        // by the inference queue, so observe actual slot release, not HTTP health.
        // Disabled/unknown slots contracts fail closed; never change settings.
        let response = self
            .client
            .get(format!("{endpoint}/slots"))
            .send()
            .await
            .map_err(map_transport_error)?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let slots: serde_json::Value = response.json().await.map_err(map_transport_error)?;
        Ok(slots.as_array().is_some_and(|slots| {
            !slots.is_empty()
                && slots
                    .iter()
                    .all(|slot| slot["is_processing"].as_bool() == Some(false))
        }))
    }

    async fn clear_launch_state(&self, endpoint: Option<&str>) {
        if let Some(endpoint) = endpoint {
            self.chat_proofs
                .write()
                .expect("chat proof lock")
                .remove(endpoint);
            self.prompt_timing.write().await.remove(endpoint);
        }
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
        let native_prefill_verified = {
            let mut launches = self.prompt_timing.write().await;
            launches.get_mut(endpoint).is_some_and(|launch| {
                launch.ready = launch.reviewed
                    && launch.runtime_id == process.runtime_id
                    && launch.executable_sha256 == process.runtime_executable_sha256;
                launch.ready
            })
        };
        self.observe_chat(process).await;
        let properties = self.startup_properties(endpoint).await?;
        let mut resolved_settings = serde_json::Map::from_iter([(
            "llama.cpp.context_length".to_owned(),
            json!(properties.context_length),
        )]);
        if let Some(parallel_requests) = properties.parallel_requests {
            resolved_settings.insert(
                "llama.cpp.parallel_requests".to_owned(),
                json!(parallel_requests),
            );
        }
        Ok(StartupObservation::Ready(BTreeMap::from([
            ("chat_contract".to_owned(), self.chat_observation(endpoint)),
            (
                "resolved_settings".to_owned(),
                Value::Object(resolved_settings),
            ),
            (
                "native_prefill_contract".to_owned(),
                json!(native_prefill_verified.then_some(prefill::CONTRACT)),
            ),
        ])))
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

    async fn embed(
        &self,
        endpoint: &str,
        request: scala_engine::EmbeddingRequest,
    ) -> Result<scala_engine::EmbeddingOutput, EngineError> {
        let response = self.client.post(format!("{endpoint}/v1/embeddings"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&json!({"model": request.model_profile_id.as_str(), "input": request.input, "encoding_format": "float"}))
            .send().await.map_err(map_transport_error)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| EngineError::BackendUnavailable(e.to_string()))?;
        if !status.is_success() {
            return Err(backend_http_error(status, &bytes));
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Usage {
            prompt_tokens: u64,
            total_tokens: u64,
        }
        #[derive(Deserialize)]
        struct Embeddings {
            data: Vec<Item>,
            usage: Option<Usage>,
        }
        let response: Embeddings = serde_json::from_slice(&bytes)
            .map_err(|e| EngineError::Operation(format!("invalid embedding response: {e}")))?;
        let invalid = || EngineError::Operation("invalid embedding indexes or vectors".to_owned());
        if response.data.len() != request.input.len() {
            return Err(invalid());
        }
        let mut vectors = vec![None; request.input.len()];
        let mut dimension = None;
        for item in response.data {
            if item.index >= vectors.len()
                || vectors[item.index].is_some()
                || item.embedding.is_empty()
                || item.embedding.iter().any(|v| !v.is_finite())
                || dimension.is_some_and(|d| d != item.embedding.len())
            {
                return Err(invalid());
            }
            dimension = Some(item.embedding.len());
            vectors[item.index] = Some(item.embedding);
        }
        let (prompt_tokens, total_tokens) = match response.usage {
            Some(usage) if usage.prompt_tokens == usage.total_tokens => {
                (Some(usage.prompt_tokens), Some(usage.total_tokens))
            }
            _ => (None, None),
        };
        Ok(scala_engine::EmbeddingOutput {
            vectors: vectors
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or_else(invalid)?,
            prompt_tokens,
            total_tokens,
        })
    }

    async fn complete(
        &self,
        endpoint: &str,
        request: scala_engine::CompletionRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let body = self.raw_completion_body(endpoint, &request, false).await?;
        self.send_completion(endpoint, "/v1/completions", body)
            .await
    }
    async fn complete_stream(
        &self,
        endpoint: &str,
        request: scala_engine::CompletionRequest,
        activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let body = self.raw_completion_body(endpoint, &request, true).await?;
        self.send_completion_stream(endpoint, "/v1/completions", body, activity)
            .await
    }
    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        self.validate_chat_endpoint(endpoint, &request)?;
        self.send_completion(
            endpoint,
            "/v1/chat/completions",
            self.backend_request(&request, false),
        )
        .await
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        self.validate_chat_endpoint(endpoint, &request)?;
        self.send_completion_stream(
            endpoint,
            "/v1/chat/completions",
            self.backend_request(&request, true),
            activity,
        )
        .await
    }
}

fn setting_source_rank(source: &scala_core::SettingSource) -> u8 {
    match source {
        scala_core::SettingSource::RuntimeDefault => 0,
        scala_core::SettingSource::SettingsOverride => 1,
        scala_core::SettingSource::ModelProfile { .. } => 2,
        scala_core::SettingSource::Invocation => 3,
    }
}

fn remove_if_lower(
    settings: &mut scala_core::ResolvedSettings,
    winner: &str,
    alternatives: &[&str],
) {
    let Some(winner_rank) = settings.configured.iter().find_map(|(id, value)| {
        (id.as_str() == winner).then(|| setting_source_rank(&value.source))
    }) else {
        return;
    };
    settings.configured.retain(|id, value| {
        !alternatives.contains(&id.as_str()) || setting_source_rank(&value.source) >= winner_rank
    });
}

fn normalize_llama_semantic_alternatives(settings: &mut scala_core::ResolvedSettings) {
    remove_if_lower(
        settings,
        "llama.cpp.chat_template",
        &[
            "llama.cpp.chat_template_file",
            "llama.cpp.chat_template_sha256",
        ],
    );
    remove_if_lower(
        settings,
        "llama.cpp.chat_template_file",
        &["llama.cpp.chat_template", "llama.cpp.chat_template_sha256"],
    );
    remove_if_lower(
        settings,
        "llama.cpp.cpu_moe_all",
        &["llama.cpp.cpu_moe_layers"],
    );
    remove_if_lower(
        settings,
        "llama.cpp.cpu_moe_layers",
        &["llama.cpp.cpu_moe_all"],
    );
    let mode_disables_bound_draft = settings.configured.iter().any(|(id, setting)| {
        id.as_str() == "llama.cpp.speculative_mode"
            && matches!(
                &setting.value,
                SettingValue::Choice(mode)
                    if mode == "off" || mode == "draft-mtp" || mode.starts_with("ngram-")
            )
    });
    if mode_disables_bound_draft {
        remove_if_lower(
            settings,
            "llama.cpp.speculative_mode",
            &[
                "llama.cpp.speculative_draft_model",
                "llama.cpp.speculative_draft_sha256",
            ],
        );
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

#[derive(Debug)]
struct LlamaDeviceEvaluation {
    compatibility: RuntimeCompatibility,
    binding: Option<AcceleratorBinding>,
}

fn llama_device_evaluation(
    accelerator: &str,
    platform: &str,
    architecture: &str,
    requirements: &scala_core::RuntimeRequirements,
    host: &HostCapabilities,
    settings: Option<&scala_core::ResolvedSettings>,
) -> LlamaDeviceEvaluation {
    if accelerator != "cuda" {
        if settings.is_some_and(|settings| settings.value("llama.cpp.devices").is_some()) {
            return LlamaDeviceEvaluation {
                compatibility: RuntimeCompatibility::Incompatible(
                    "`llama.cpp.devices` requires a CUDA llama.cpp runtime".to_owned(),
                ),
                binding: None,
            };
        }
        return LlamaDeviceEvaluation {
            compatibility: RuntimeCompatibility::Compatible,
            binding: None,
        };
    }
    let devices = match visible_nvidia_device_set(host, "llama.cpp") {
        Ok(devices) => devices,
        Err(compatibility) => {
            return LlamaDeviceEvaluation {
                compatibility,
                binding: None,
            };
        }
    };
    let mut evaluated = devices
        .into_iter()
        .map(|device| {
            let device_host = HostCapabilities {
                platform: host.platform.clone(),
                architecture: host.architecture.clone(),
                accelerators: vec![device.clone()],
                nvidia_gpu_absence_confirmed: false,
                cuda_visible_devices: None,
                observations: Vec::new(),
            };
            (
                device.clone(),
                compatibility_for(
                    platform,
                    architecture,
                    accelerator,
                    requirements,
                    &device_host,
                ),
            )
        })
        .collect::<Vec<_>>();
    if let Some(configured) = settings.and_then(llama_configured_device_ids) {
        let mut selected = Vec::with_capacity(configured.len());
        for configured_uuid in configured {
            if !scala_engine::is_exact_nvidia_gpu_uuid(configured_uuid) {
                return LlamaDeviceEvaluation {
                    compatibility: RuntimeCompatibility::Incompatible(format!(
                        "`llama.cpp.devices` entry `{configured_uuid}` is not an exact NVIDIA GPU UUID"
                    )),
                    binding: None,
                };
            }
            let matching = evaluated
                .iter()
                .filter(|(device, _)| {
                    device
                        .stable_id
                        .as_deref()
                        .is_some_and(|uuid| uuid.eq_ignore_ascii_case(configured_uuid))
                })
                .collect::<Vec<_>>();
            let [(device, compatibility)] = matching.as_slice() else {
                return LlamaDeviceEvaluation {
                    compatibility: RuntimeCompatibility::Incompatible(format!(
                        "`llama.cpp.devices` entry `{configured_uuid}` does not resolve exactly to one currently visible NVIDIA GPU"
                    )),
                    binding: None,
                };
            };
            if selected.iter().any(|selected: &AcceleratorDevice| {
                selected.stable_id.as_deref().is_some_and(|uuid| {
                    device
                        .stable_id
                        .as_deref()
                        .is_some_and(|candidate| uuid.eq_ignore_ascii_case(candidate))
                })
            }) {
                return LlamaDeviceEvaluation {
                    compatibility: RuntimeCompatibility::Incompatible(format!(
                        "`llama.cpp.devices` selects NVIDIA GPU `{configured_uuid}` more than once"
                    )),
                    binding: None,
                };
            }
            if let RuntimeCompatibility::Incompatible(reason) = compatibility {
                return LlamaDeviceEvaluation {
                    compatibility: RuntimeCompatibility::Incompatible(format!(
                        "`llama.cpp.devices` GPU `{configured_uuid}` is incompatible: {reason}"
                    )),
                    binding: None,
                };
            }
            selected.push(device.clone());
        }
        let binding = AcceleratorBinding { devices: selected };
        if let Some(settings) = settings
            && let Err(error) = validate_llama_accelerator_settings(settings, &binding)
        {
            return LlamaDeviceEvaluation {
                compatibility: RuntimeCompatibility::Incompatible(error.to_string()),
                binding: None,
            };
        }
        let compatibility = evaluated
            .iter()
            .filter(|(device, _)| binding.devices.contains(device))
            .map(|(_, compatibility)| compatibility.clone())
            .fold(
                RuntimeCompatibility::Recommended,
                combine_llama_compatibility,
            );
        return LlamaDeviceEvaluation {
            compatibility,
            binding: Some(binding),
        };
    }

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
        Some((accelerator, compatibility)) => LlamaDeviceEvaluation {
            compatibility,
            binding: Some(AcceleratorBinding::single(accelerator)),
        },
        None => LlamaDeviceEvaluation {
            compatibility: RuntimeCompatibility::NeedsAttention(
                "llama.cpp CUDA requires a stable NVIDIA GPU UUID, but none was observed"
                    .to_owned(),
            ),
            binding: None,
        },
    }
}

fn llama_configured_device_ids(settings: &scala_core::ResolvedSettings) -> Option<&[String]> {
    match settings.value("llama.cpp.devices") {
        Some(SettingValue::StringList(devices)) => Some(devices),
        _ => None,
    }
}

fn combine_llama_compatibility(
    left: RuntimeCompatibility,
    right: RuntimeCompatibility,
) -> RuntimeCompatibility {
    if matches!(left, RuntimeCompatibility::Incompatible(_)) {
        left
    } else if matches!(right, RuntimeCompatibility::Incompatible(_)) {
        right
    } else if matches!(left, RuntimeCompatibility::NeedsAttention(_)) {
        left
    } else if matches!(right, RuntimeCompatibility::NeedsAttention(_)) {
        right
    } else if matches!(left, RuntimeCompatibility::Compatible)
        || matches!(right, RuntimeCompatibility::Compatible)
    {
        RuntimeCompatibility::Compatible
    } else {
        RuntimeCompatibility::Recommended
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
    total_slots: Option<u64>,
}

struct LlamaStartupProperties {
    context_length: u64,
    parallel_requests: Option<u64>,
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
    timings: Option<Value>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
    text: Option<String>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<chat::WireToolCall>,
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
            prompt_processing_tokens: None,
            prompt_processing_ms: None,
            reasoning_output_tokens: None,
        }
    }
}

/// b10665 tools/server/server-common.cpp and server-task.cpp define these
/// counters separately: processed prompt, reused prompt, native prefill time.
fn apply_prompt_timings(usage: &mut InferenceUsage, timings: &Value) {
    usage.prompt_processing_tokens = timings.get("prompt_n").and_then(Value::as_u64);
    usage.prompt_processing_ms = timings.get("prompt_ms").and_then(Value::as_f64);
    let cache = timings.get("cache_n").and_then(Value::as_u64);
    if usage
        .cached_input_tokens
        .zip(cache)
        .is_some_and(|(a, b)| a != b)
    {
        // Conflicting native populations cannot establish a valid rate.
        usage.prompt_processing_ms = None;
    } else if usage.cached_input_tokens.is_none() {
        usage.cached_input_tokens = cache;
    }
}

struct PromptTimingLaunch {
    runtime_id: RuntimeId,
    executable_sha256: String,
    reviewed: bool,
    ready: bool,
}

impl LlamaCppAdapter {
    async fn prepare_prompt_timing(&self, spec: &LaunchSpec) {
        let Some(endpoint) = spec.endpoint.as_ref() else {
            return;
        };
        self.prepare_chat(spec).await;
        // Clear old endpoint proof before verification, including failed/retried loads.
        self.prompt_timing.write().await.remove(endpoint);
        let reviewed = prefill::reviewed(&spec.runtime)
            && hash_file(&spec.executable).await.ok().as_deref()
                == Some(spec.runtime.manifest.entrypoint_sha256.as_str());
        self.prompt_timing.write().await.insert(
            endpoint.clone(),
            PromptTimingLaunch {
                runtime_id: spec.runtime.manifest.runtime_id.clone(),
                executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
                reviewed,
                ready: false,
            },
        );
    }

    async fn native_prefill_verified(&self, endpoint: &str) -> bool {
        self.prompt_timing
            .read()
            .await
            .get(endpoint)
            .is_some_and(|launch| launch.ready)
    }
}

struct SseState {
    tool_calls: chat::StreamCalls,
    native_prefill_verified: bool,
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: Option<InferenceFinishReason>,
    activity: InferenceActivityReporter,
    finished: bool,
}

fn llama_sse_stream(
    native_prefill_verified: bool,
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    activity: InferenceActivityReporter,
) -> InferenceStream {
    let state = SseState {
        tool_calls: chat::StreamCalls::default(),
        native_prefill_verified,
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
                    if let Err(error) = state.tool_calls.finish(&finish_reason) {
                        state.queued.push_back(Err(error));
                        state.finished = true;
                        return;
                    }
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
        // Reviewed b10665 server-common.cpp: prompt_n is n_prompt_processed,
        // cache_n is n_prompt_cached, prompt_ms is t_prompt_ms(). The final
        // OAI streaming usage frame carries these stats (server-task.cpp).
        // Accept only co-located terminal usage/timing, never progress snapshots.
        if state.native_prefill_verified
            && value.get("usage").is_some_and(|v| !v.is_null())
            && let Some(usage) = state.usage.as_mut()
            && let Some(timings) = value.get("timings")
        {
            apply_prompt_timings(usage, timings);
        }
        match state.tool_calls.deltas(&value) {
            Ok(events) => state.queued.extend(events.into_iter().map(Ok)),
            Err(error) => {
                state.queued.push_back(Err(error));
                state.finished = true;
                return;
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
            .and_then(|choice| {
                choice
                    .get("text")
                    .or_else(|| choice.get("delta").and_then(|delta| delta.get("content")))
            })
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
                matches!(
                    message.role,
                    InferenceRole::User | InferenceRole::Assistant | InferenceRole::Tool
                )
            })
            .map(message_json),
    );
    backend
}

fn message_json(message: &InferenceMessage) -> Value {
    let mut value = json!({
        "role": match message.role {
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
            InferenceRole::System | InferenceRole::Developer => "system",
            InferenceRole::Tool => "tool",
        },
        "content": message.text_only().unwrap_or_default(),
    });
    if !message.tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(chat::tool_call_json)
                .collect(),
        );
    }
    if let Some(id) = &message.tool_call_id {
        value["tool_call_id"] = json!(id);
    }
    value
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
        "llama.cpp.context_length",
        "llama.cpp.parallel_requests",
        "llama.cpp.temperature",
        "llama.cpp.top_p",
        "llama.cpp.top_k",
        "llama.cpp.min_p",
        "llama.cpp.seed",
        "llama.cpp.repeat_penalty",
        "llama.cpp.presence_penalty",
        "llama.cpp.frequency_penalty",
        "llama.cpp.max_output_tokens",
        "llama.cpp.stop_strings",
        "llama.cpp.system_prompt",
        "llama.cpp.reasoning",
        "llama.cpp.reasoning_effort",
        "llama.cpp.reasoning_budget",
        "llama.cpp.reasoning_budget_message",
        "llama.cpp.structured_output_schema",
        "llama.cpp.context_overflow",
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
    definitions.extend(llama_extended_setting_definitions());
    definitions
}

fn llama_extended_setting_definitions() -> Vec<SettingDefinition> {
    let uint = |minimum| SettingKind::UnsignedInteger {
        minimum: Some(minimum),
        maximum: None,
    };
    let integer = SettingKind::Integer {
        minimum: None,
        maximum: None,
    };
    let float = |minimum, maximum| SettingKind::Float { minimum, maximum };
    let choice = |values: &[&str]| SettingKind::Choice {
        choices: values.iter().map(|value| (*value).to_owned()).collect(),
    };
    let specs = [
        (
            "threads_batch",
            "Batch CPU threads",
            "CPU threads used for prompt and batch processing",
            uint(1),
            "same as llama.cpp.threads",
        ),
        (
            "cpu_mask",
            "CPU affinity mask",
            "Generation-thread CPU affinity hexadecimal mask",
            SettingKind::String,
            "empty",
        ),
        (
            "cpu_range",
            "CPU affinity range",
            "Generation-thread CPU affinity range",
            SettingKind::String,
            "None",
        ),
        (
            "cpu_strict",
            "Strict CPU placement",
            "Require strict generation-thread CPU placement",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "priority",
            "Thread priority",
            "Generation thread priority",
            choice(&["low", "normal", "medium", "high", "realtime"]),
            "normal",
        ),
        (
            "poll",
            "Thread polling",
            "Generation worker polling percentage",
            float(Some(0.0), Some(100.0)),
            "50",
        ),
        (
            "cpu_mask_batch",
            "Batch CPU affinity mask",
            "Batch-thread CPU affinity hexadecimal mask",
            SettingKind::String,
            "same as llama.cpp.cpu_mask",
        ),
        (
            "cpu_range_batch",
            "Batch CPU affinity range",
            "Batch-thread CPU affinity range",
            SettingKind::String,
            "None",
        ),
        (
            "cpu_strict_batch",
            "Strict batch CPU placement",
            "Require strict batch-thread CPU placement",
            SettingKind::Toggle,
            "same as llama.cpp.cpu_strict",
        ),
        (
            "priority_batch",
            "Batch thread priority",
            "Batch thread priority",
            choice(&["normal", "medium", "high", "realtime"]),
            "normal",
        ),
        (
            "poll_batch",
            "Batch thread polling",
            "Batch worker polling policy",
            SettingKind::Toggle,
            "same as llama.cpp.poll",
        ),
        (
            "keep_tokens",
            "Prompt tokens to keep",
            "Initial prompt tokens retained during context shifting",
            integer.clone(),
            "0",
        ),
        (
            "swa_full",
            "Full SWA cache",
            "Allocate a full-size sliding-window-attention cache",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "performance_timings",
            "Performance timings",
            "Enable internal libllama performance timings",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "escape_sequences",
            "Escape sequences",
            "Process escaped control sequences in prompt strings",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "rope_scaling",
            "RoPE scaling method",
            "RoPE scaling algorithm",
            choice(&["none", "linear", "yarn"]),
            "auto",
        ),
        (
            "rope_context_scale",
            "RoPE context scale",
            "Context expansion factor",
            float(Some(f64::MIN_POSITIVE), None),
            "auto",
        ),
        (
            "yarn_original_context",
            "YaRN original context",
            "Original training context for YaRN",
            uint(0),
            "model training context",
        ),
        (
            "yarn_extrapolation_factor",
            "YaRN extrapolation factor",
            "YaRN extrapolation/interpolation mix",
            float(None, None),
            "auto",
        ),
        (
            "yarn_attention_factor",
            "YaRN attention factor",
            "YaRN attention magnitude scale",
            float(None, None),
            "auto",
        ),
        (
            "yarn_beta_slow",
            "YaRN beta slow",
            "YaRN high correction dimension",
            float(None, None),
            "auto",
        ),
        (
            "yarn_beta_fast",
            "YaRN beta fast",
            "YaRN low correction dimension",
            float(None, None),
            "auto",
        ),
        (
            "weight_repacking",
            "Weight repacking",
            "Enable weight repacking",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "host_buffer",
            "Host buffer",
            "Allow host buffers for model tensors",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "lazy_mode",
            "Lazy tensor loading",
            "On-demand tensor row loading policy",
            choice(&["auto", "on", "off"]),
            "auto",
        ),
        (
            "numa",
            "NUMA policy",
            "NUMA execution placement",
            choice(&["distribute", "isolate", "numactl"]),
            "disabled",
        ),
        (
            "override_tensor",
            "Tensor placement overrides",
            "Tensor-pattern to buffer-type placement overrides",
            SettingKind::StringList,
            "None",
        ),
        (
            "cpu_ffn_layers",
            "Dense FFN layers on CPU",
            "Keep dense FFN weights for the first N layers on CPU",
            uint(0),
            "0",
        ),
        (
            "devices",
            "CUDA devices",
            "Ordered exact NVIDIA GPU UUIDs; unset uses Scala's automatic single-compatible-GPU policy",
            SettingKind::StringList,
            "auto",
        ),
        (
            "split_mode",
            "Multi-GPU split mode",
            "Model placement strategy across selected GPUs",
            choice(&["none", "layer", "row", "tensor"]),
            "layer",
        ),
        (
            "tensor_split",
            "Tensor split",
            "Per-device model offload proportions",
            SettingKind::StringList,
            "auto",
        ),
        (
            "main_gpu",
            "Main GPU",
            "Primary GPU index for single/row split operation",
            uint(0),
            "0",
        ),
        (
            "fit",
            "Fit to device memory",
            "Adjust otherwise-unset settings to fit device memory",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "fit_target_mib",
            "Fit target margins",
            "Per-device free-memory margins in MiB",
            SettingKind::StringList,
            "1024 MiB per device",
        ),
        (
            "fit_min_context",
            "Fit minimum context",
            "Minimum context allowed by automatic memory fitting",
            uint(1),
            "4096",
        ),
        (
            "check_tensors",
            "Check tensors",
            "Validate loaded model tensors for invalid values",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "operation_offload",
            "Operation offload",
            "Offload host tensor operations to an accelerator",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "lora_adapters",
            "LoRA adapters",
            "Local LoRA adapter paths",
            SettingKind::StringList,
            "None",
        ),
        (
            "lora_scaled",
            "Scaled LoRA adapters",
            "Local LoRA path and scale specifications",
            SettingKind::StringList,
            "None",
        ),
        (
            "control_vectors",
            "Control vectors",
            "Local control-vector paths",
            SettingKind::StringList,
            "None",
        ),
        (
            "control_vectors_scaled",
            "Scaled control vectors",
            "Local control-vector path and scale specifications",
            SettingKind::StringList,
            "None",
        ),
        (
            "control_vector_layer_range",
            "Control-vector layer range",
            "Inclusive start and end layers",
            SettingKind::String,
            "all layers",
        ),
        (
            "log_disabled",
            "Disable logging",
            "Disable llama.cpp process logging",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "log_file",
            "Log file",
            "Write llama.cpp process logs to a file",
            SettingKind::Path,
            "None",
        ),
        (
            "log_colors",
            "Log colors",
            "Colored log-output policy",
            choice(&["auto", "on", "off"]),
            "auto",
        ),
        (
            "log_verbosity",
            "Log verbosity",
            "llama.cpp log threshold",
            uint(0),
            "3",
        ),
        (
            "log_prefix",
            "Log prefix",
            "Prefix llama.cpp log messages",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "log_timestamps",
            "Log timestamps",
            "Timestamp llama.cpp log messages",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "offline",
            "Offline mode",
            "Prevent llama.cpp network access",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "samplers",
            "Sampler chain",
            "Ordered sampler names",
            SettingKind::StringList,
            "penalties;dry;top_n_sigma;top_k;typ_p;top_p;min_p;xtc;temperature",
        ),
        (
            "ignore_eos",
            "Ignore EOS",
            "Continue generation after EOS",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "top_n_sigma",
            "Top-n-sigma",
            "Top-n-sigma sampling threshold",
            float(None, None),
            "-1",
        ),
        (
            "xtc_probability",
            "XTC probability",
            "XTC sampling probability",
            float(Some(0.0), Some(1.0)),
            "0",
        ),
        (
            "xtc_threshold",
            "XTC threshold",
            "XTC sampling threshold",
            float(Some(0.0), Some(1.0)),
            "0.1",
        ),
        (
            "typical_p",
            "Typical P",
            "Locally typical sampling probability",
            float(Some(0.0), Some(1.0)),
            "1",
        ),
        (
            "repeat_last_n",
            "Repeat window",
            "Recent tokens considered by repetition penalties",
            uint(0),
            "64",
        ),
        (
            "dry_multiplier",
            "DRY multiplier",
            "DRY repetition penalty multiplier",
            float(Some(0.0), None),
            "0",
        ),
        (
            "dry_base",
            "DRY base",
            "DRY repetition penalty base",
            float(Some(0.0), None),
            "1.75",
        ),
        (
            "dry_allowed_length",
            "DRY allowed length",
            "Allowed repeated sequence length",
            uint(0),
            "2",
        ),
        (
            "dry_penalty_last_n",
            "DRY window",
            "Recent-token window for DRY",
            uint(0),
            "64",
        ),
        (
            "dry_sequence_breakers",
            "DRY sequence breakers",
            "DRY sequence breaker strings",
            SettingKind::StringList,
            "runtime built-ins",
        ),
        (
            "adaptive_target",
            "Adaptive target",
            "Adaptive-p target probability; negative disables",
            float(Some(-1.0), Some(1.0)),
            "-1",
        ),
        (
            "adaptive_decay",
            "Adaptive decay",
            "Adaptive-p target decay rate",
            float(Some(0.0), Some(0.99)),
            "0.9",
        ),
        (
            "dynatemp_range",
            "Dynamic temperature range",
            "Dynamic temperature range",
            float(Some(0.0), None),
            "0",
        ),
        (
            "dynatemp_exponent",
            "Dynamic temperature exponent",
            "Dynamic temperature exponent",
            float(Some(0.0), None),
            "48",
        ),
        (
            "mirostat",
            "Mirostat mode",
            "Mirostat sampling mode",
            uint(0),
            "0",
        ),
        (
            "mirostat_learning_rate",
            "Mirostat learning rate",
            "Mirostat eta",
            float(Some(0.0), None),
            "0.1",
        ),
        (
            "mirostat_entropy",
            "Mirostat entropy",
            "Mirostat target entropy",
            float(Some(0.0), None),
            "5",
        ),
        (
            "backend_sampling",
            "Backend sampling",
            "Use experimental backend sampling",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "draft_kv_cache_k",
            "Draft K-cache type",
            "Draft-model key cache storage type",
            choice(&[
                "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
            ]),
            "f16",
        ),
        (
            "draft_kv_cache_v",
            "Draft V-cache type",
            "Draft-model value cache storage type",
            choice(&[
                "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
            ]),
            "f16",
        ),
        (
            "draft_tokens_max",
            "Maximum draft tokens",
            "Maximum speculative draft length",
            uint(1),
            "3",
        ),
        (
            "draft_tokens_min",
            "Minimum draft tokens",
            "Minimum speculative draft length",
            uint(0),
            "0",
        ),
        (
            "draft_probability_min",
            "Minimum draft probability",
            "Greedy speculative acceptance probability",
            float(Some(0.0), Some(1.0)),
            "0",
        ),
        (
            "draft_probability_split",
            "Draft split probability",
            "Speculative candidate split probability",
            float(Some(0.0), Some(1.0)),
            "0.1",
        ),
        (
            "draft_threads",
            "Draft CPU threads",
            "Draft generation CPU threads",
            uint(1),
            "auto",
        ),
        (
            "draft_threads_batch",
            "Draft batch CPU threads",
            "Draft prompt-processing CPU threads",
            uint(1),
            "auto",
        ),
        (
            "draft_cpu_mask",
            "Draft CPU mask",
            "Draft generation CPU affinity mask",
            SettingKind::String,
            "auto",
        ),
        (
            "draft_cpu_range",
            "Draft CPU range",
            "Draft generation CPU affinity range",
            SettingKind::String,
            "None",
        ),
        (
            "draft_cpu_strict",
            "Strict draft CPU placement",
            "Require strict draft CPU placement",
            SettingKind::Toggle,
            "auto",
        ),
        (
            "draft_priority",
            "Draft priority",
            "Draft worker priority",
            choice(&["normal", "medium", "high", "realtime"]),
            "normal",
        ),
        (
            "draft_poll",
            "Draft polling",
            "Draft worker polling policy",
            SettingKind::Toggle,
            "auto",
        ),
        (
            "draft_cpu_mask_batch",
            "Draft batch CPU mask",
            "Draft batch-thread CPU affinity mask",
            SettingKind::String,
            "auto",
        ),
        (
            "draft_cpu_strict_batch",
            "Strict draft batch placement",
            "Require strict draft batch CPU placement",
            SettingKind::Toggle,
            "auto",
        ),
        (
            "draft_priority_batch",
            "Draft batch priority",
            "Draft batch-worker priority",
            choice(&["normal", "medium", "high", "realtime"]),
            "normal",
        ),
        (
            "draft_poll_batch",
            "Draft batch polling",
            "Draft batch-worker polling policy",
            SettingKind::Toggle,
            "auto",
        ),
        (
            "draft_override_tensor",
            "Draft tensor overrides",
            "Draft tensor-pattern buffer overrides",
            SettingKind::StringList,
            "None",
        ),
        (
            "draft_cpu_moe",
            "Draft MoE on CPU",
            "Keep every draft-model expert weight on CPU",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "draft_cpu_moe_layers",
            "Draft MoE CPU layers",
            "Keep the first N draft MoE layers on CPU",
            uint(0),
            "0",
        ),
        (
            "draft_gpu_offload",
            "Draft GPU offload",
            "Draft-model layers placed in VRAM",
            SettingKind::GpuOffload,
            "auto",
        ),
        (
            "draft_backend_sampling",
            "Draft backend sampling",
            "Offload draft sampling to the backend",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "ngram_mod_min",
            "N-gram modified minimum",
            "Minimum N-gram modified draft length",
            uint(1),
            "1",
        ),
        (
            "ngram_mod_max",
            "N-gram modified maximum",
            "Maximum N-gram modified draft length",
            uint(1),
            "64",
        ),
        (
            "ngram_mod_match",
            "N-gram modified match",
            "N-gram modified lookup length",
            uint(1),
            "24",
        ),
        (
            "ngram_simple_size_n",
            "N-gram simple lookup",
            "N-gram-simple lookup length",
            uint(1),
            "12",
        ),
        (
            "ngram_simple_size_m",
            "N-gram simple draft",
            "N-gram-simple draft length",
            uint(1),
            "48",
        ),
        (
            "ngram_simple_min_hits",
            "N-gram simple hits",
            "Minimum N-gram-simple hits",
            uint(1),
            "1",
        ),
        (
            "ngram_map_size_n",
            "N-gram map lookup",
            "N-gram-map lookup length",
            uint(1),
            "12",
        ),
        (
            "ngram_map_size_m",
            "N-gram map draft",
            "N-gram-map draft length",
            uint(1),
            "48",
        ),
        (
            "ngram_map_min_hits",
            "N-gram map hits",
            "Minimum N-gram-map hits",
            uint(1),
            "1",
        ),
        (
            "ngram_map4_size_n",
            "N-gram map4 lookup",
            "N-gram-map-k4v lookup length",
            uint(1),
            "12",
        ),
        (
            "ngram_map4_size_m",
            "N-gram map4 draft",
            "N-gram-map-k4v draft length",
            uint(1),
            "48",
        ),
        (
            "ngram_map4_min_hits",
            "N-gram map4 hits",
            "Minimum N-gram-map-k4v hits",
            uint(1),
            "1",
        ),
        (
            "kv_per_slot",
            "KV context per slot",
            "Unified-KV context limit for each slot",
            uint(1),
            "unset",
        ),
        (
            "checkpoint_min_step",
            "Checkpoint minimum step",
            "Minimum token spacing between context checkpoints",
            uint(1),
            "8192",
        ),
        (
            "cache_ram_mib",
            "Cache RAM",
            "Maximum server cache size in MiB",
            integer.clone(),
            "8192",
        ),
        (
            "cache_idle_slots",
            "Cache idle slots",
            "Allow idle slot KV data in the shared cache",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "context_shift",
            "Context shifting",
            "Shift context for unbounded generation",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "warmup",
            "Warmup",
            "Warm the model with an empty run",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "continuous_batching",
            "Continuous batching",
            "Enable continuous batching",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "timeout_seconds",
            "Server timeout",
            "Private backend read/write timeout in seconds",
            uint(1),
            "3600",
        ),
        (
            "sse_ping_interval",
            "SSE ping interval",
            "Backend SSE keepalive interval; -1 disables",
            integer.clone(),
            "30",
        ),
        (
            "http_threads",
            "HTTP threads",
            "Private backend HTTP worker threads",
            integer.clone(),
            "auto",
        ),
        (
            "prompt_cache",
            "Prompt cache",
            "Enable prompt caching",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "cache_reuse",
            "Cache reuse threshold",
            "Minimum reusable cache chunk size",
            uint(0),
            "0",
        ),
        (
            "metrics",
            "Metrics endpoint",
            "Enable the private backend metrics endpoint",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "slots_endpoint",
            "Slots endpoint",
            "Enable private backend slot monitoring",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "slot_save_path",
            "Slot save path",
            "Directory for persisted slot KV caches",
            SettingKind::Path,
            "None",
        ),
        (
            "jinja",
            "Jinja templates",
            "Enable the Jinja chat-template engine",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "reasoning_format",
            "Reasoning format",
            "Reasoning extraction format",
            choice(&["auto", "none", "deepseek", "deepseek-legacy"]),
            "auto",
        ),
        (
            "reasoning_preserve",
            "Preserve reasoning",
            "Preserve reasoning traces in conversation history",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "chat_template_kwargs",
            "Chat-template arguments",
            "Additional JSON object passed to the chat template",
            SettingKind::JsonObject,
            "{}",
        ),
        (
            "skip_chat_parsing",
            "Skip chat parsing",
            "Return reasoning/tool syntax as plain message content",
            SettingKind::Toggle,
            "disabled",
        ),
        (
            "prefill_assistant",
            "Prefill assistant",
            "Prefill a trailing assistant message",
            SettingKind::Toggle,
            "enabled",
        ),
        (
            "slot_prompt_similarity",
            "Slot prompt similarity",
            "Minimum prompt similarity for slot reuse",
            float(Some(0.0), Some(1.0)),
            "0.1",
        ),
        (
            "lora_init_without_apply",
            "Defer LoRA application",
            "Load LoRA adapters without initially applying them",
            SettingKind::OneWayFlag,
            "disabled",
        ),
        (
            "sleep_idle_seconds",
            "Sleep after idle",
            "Idle seconds before the backend sleeps; -1 disables",
            integer,
            "disabled",
        ),
        (
            "log_prompts_dir",
            "Prompt log directory",
            "Directory for diagnostic prompt logs",
            SettingKind::Path,
            "None",
        ),
    ];
    specs
        .into_iter()
        .map(
            |(suffix, label, description, kind, _audited_snapshot_default)| {
                llama_definition(&format!("{ENGINE_ID}.{suffix}"), label, description, kind)
            },
        )
        .collect()
}

fn llama_model_setting_definitions(model: Option<&ModelArtifact>) -> Vec<SettingDefinition> {
    let mut definitions = llama_setting_definitions();
    if let Some(temperature) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "llama.cpp.temperature")
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
                "llama.cpp.context_length",
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
        category: if id.contains("sampler")
            || id.contains("temperature")
            || id.contains("sigma")
            || id.contains("xtc")
            || id.contains("typical")
            || id.contains("repeat")
            || id.contains("dry_")
            || id.contains("mirostat")
            || id.contains("dynatemp")
            || id.contains("ignore_eos")
        {
            scala_core::SettingCategory::Generation
        } else if id.contains("unified_kv")
            || id.contains("kv_cache")
            || id.contains("flash_attention")
            || id.contains("cache_ram")
            || id.contains("kv_per_slot")
        {
            scala_core::SettingCategory::KvMemory
        } else if id.contains("speculative") || id.contains("draft_") || id.contains("ngram_") {
            scala_core::SettingCategory::Speculation
        } else if id.contains("chat_template")
            || id.contains("reasoning")
            || id.contains("jinja")
            || id.contains("prefill_assistant")
            || id.contains("skip_chat")
        {
            scala_core::SettingCategory::Prompt
        } else if id.contains("context_checkpoint")
            || id.contains("checkpoint_")
            || id.contains("log_")
            || id.contains("metrics")
            || id.contains("endpoint")
            || id.contains("timeout")
            || id.contains("http_")
            || id.contains("sse_")
        {
            scala_core::SettingCategory::Advanced
        } else if id.contains("cache") {
            scala_core::SettingCategory::Cache
        } else {
            scala_core::SettingCategory::Load
        },
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: match id {
            "llama.cpp.devices" => Some(
                SettingDefaultPreview::new("auto", SettingDefaultSource::Scala).with_detail(
                    "CUDA selects one compatible visible NVIDIA GPU by exact UUID; other runtimes retain ordinary automatic device behavior",
                ),
            ),
            "llama.cpp.chat_template_file"
            | "llama.cpp.chat_template_sha256"
            | "llama.cpp.speculative_draft_model"
            | "llama.cpp.speculative_draft_sha256"
            | "llama.cpp.lora_adapters"
            | "llama.cpp.lora_scaled"
            | "llama.cpp.control_vectors"
            | "llama.cpp.control_vectors_scaled" => Some(SettingDefaultPreview::new(
                "None",
                SettingDefaultSource::Scala,
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

#[derive(Clone, Copy)]
enum LlamaDirectMode {
    Value,
    Toggle {
        enabled: Option<&'static str>,
        disabled: Option<&'static str>,
    },
    Flag,
}

#[derive(Clone, Copy)]
struct LlamaDirectSetting {
    required: &'static str,
    aliases: &'static [&'static str],
    environment: &'static [&'static str],
    mode: LlamaDirectMode,
    list_separator: &'static str,
}

fn llama_direct_setting(id: &str) -> Option<LlamaDirectSetting> {
    macro_rules! value {
        ($option:literal, $env:literal) => {
            Some(LlamaDirectSetting {
                required: $option,
                aliases: &[$option],
                environment: &[$env],
                mode: LlamaDirectMode::Value,
                list_separator: ",",
            })
        };
        ($option:literal) => {
            Some(LlamaDirectSetting {
                required: $option,
                aliases: &[$option],
                environment: &[],
                mode: LlamaDirectMode::Value,
                list_separator: ",",
            })
        };
    }
    macro_rules! toggle {
        ($required:literal, $on:expr, $off:expr, $env:literal) => {
            Some(LlamaDirectSetting {
                required: $required,
                aliases: &[$required],
                environment: &[$env],
                mode: LlamaDirectMode::Toggle {
                    enabled: $on,
                    disabled: $off,
                },
                list_separator: ",",
            })
        };
        ($required:literal, $on:expr, $off:expr) => {
            Some(LlamaDirectSetting {
                required: $required,
                aliases: &[$required],
                environment: &[],
                mode: LlamaDirectMode::Toggle {
                    enabled: $on,
                    disabled: $off,
                },
                list_separator: ",",
            })
        };
    }
    macro_rules! flag {
        ($option:literal, $env:literal) => {
            Some(LlamaDirectSetting {
                required: $option,
                aliases: &[$option],
                environment: &[$env],
                mode: LlamaDirectMode::Flag,
                list_separator: ",",
            })
        };
        ($option:literal) => {
            Some(LlamaDirectSetting {
                required: $option,
                aliases: &[$option],
                environment: &[],
                mode: LlamaDirectMode::Flag,
                list_separator: ",",
            })
        };
    }
    match id {
        "llama.cpp.threads_batch" => value!("--threads-batch"),
        "llama.cpp.cpu_mask" => value!("--cpu-mask"),
        "llama.cpp.cpu_range" => value!("--cpu-range"),
        "llama.cpp.cpu_strict" => value!("--cpu-strict"),
        "llama.cpp.priority" => value!("--prio"),
        "llama.cpp.poll" => value!("--poll"),
        "llama.cpp.cpu_mask_batch" => value!("--cpu-mask-batch"),
        "llama.cpp.cpu_range_batch" => value!("--cpu-range-batch"),
        "llama.cpp.cpu_strict_batch" => value!("--cpu-strict-batch"),
        "llama.cpp.priority_batch" => value!("--prio-batch"),
        "llama.cpp.poll_batch" => value!("--poll-batch"),
        "llama.cpp.keep_tokens" => value!("--keep"),
        "llama.cpp.swa_full" => flag!("--swa-full", "LLAMA_ARG_SWA_FULL"),
        "llama.cpp.performance_timings" => toggle!(
            "--perf",
            Some("--perf"),
            Some("--no-perf"),
            "LLAMA_ARG_PERF"
        ),
        "llama.cpp.escape_sequences" => toggle!("--escape", Some("--escape"), Some("--no-escape")),
        "llama.cpp.rope_scaling" => value!("--rope-scaling", "LLAMA_ARG_ROPE_SCALING_TYPE"),
        "llama.cpp.rope_context_scale" => value!("--rope-scale", "LLAMA_ARG_ROPE_SCALE"),
        "llama.cpp.yarn_original_context" => value!("--yarn-orig-ctx", "LLAMA_ARG_YARN_ORIG_CTX"),
        "llama.cpp.yarn_extrapolation_factor" => {
            value!("--yarn-ext-factor", "LLAMA_ARG_YARN_EXT_FACTOR")
        }
        "llama.cpp.yarn_attention_factor" => {
            value!("--yarn-attn-factor", "LLAMA_ARG_YARN_ATTN_FACTOR")
        }
        "llama.cpp.yarn_beta_slow" => value!("--yarn-beta-slow", "LLAMA_ARG_YARN_BETA_SLOW"),
        "llama.cpp.yarn_beta_fast" => value!("--yarn-beta-fast", "LLAMA_ARG_YARN_BETA_FAST"),
        "llama.cpp.weight_repacking" => toggle!(
            "--repack",
            Some("--repack"),
            Some("--no-repack"),
            "LLAMA_ARG_REPACK"
        ),
        "llama.cpp.host_buffer" => {
            toggle!("--no-host", None, Some("--no-host"), "LLAMA_ARG_NO_HOST")
        }
        "llama.cpp.lazy_mode" => value!("--lazy-mode", "LLAMA_ARG_LAZY_MODE"),
        "llama.cpp.numa" => value!("--numa", "LLAMA_ARG_NUMA"),
        "llama.cpp.override_tensor" => value!("--override-tensor", "LLAMA_ARG_OVERRIDE_TENSOR"),
        "llama.cpp.cpu_ffn_layers" => value!("--n-cpu-ffn", "LLAMA_ARG_N_CPU_FFN"),
        "llama.cpp.split_mode" => value!("--split-mode", "LLAMA_ARG_SPLIT_MODE"),
        "llama.cpp.tensor_split" => value!("--tensor-split", "LLAMA_ARG_TENSOR_SPLIT"),
        "llama.cpp.main_gpu" => value!("--main-gpu", "LLAMA_ARG_MAIN_GPU"),
        "llama.cpp.fit" => value!("--fit", "LLAMA_ARG_FIT"),
        "llama.cpp.fit_target_mib" => value!("--fit-target", "LLAMA_ARG_FIT_TARGET"),
        "llama.cpp.fit_min_context" => value!("--fit-ctx", "LLAMA_ARG_FIT_CTX"),
        "llama.cpp.check_tensors" => flag!("--check-tensors"),
        "llama.cpp.operation_offload" => toggle!(
            "--op-offload",
            Some("--op-offload"),
            Some("--no-op-offload")
        ),
        "llama.cpp.lora_adapters" => value!("--lora"),
        "llama.cpp.lora_scaled" => value!("--lora-scaled"),
        "llama.cpp.control_vectors" => value!("--control-vector"),
        "llama.cpp.control_vectors_scaled" => value!("--control-vector-scaled"),
        "llama.cpp.control_vector_layer_range" => value!("--control-vector-layer-range"),
        "llama.cpp.log_disabled" => flag!("--log-disable"),
        "llama.cpp.log_file" => value!("--log-file", "LLAMA_ARG_LOG_FILE"),
        "llama.cpp.log_colors" => value!("--log-colors", "LLAMA_ARG_LOG_COLORS"),
        "llama.cpp.log_verbosity" => value!("--log-verbosity", "LLAMA_ARG_LOG_VERBOSITY"),
        "llama.cpp.log_prefix" => toggle!(
            "--log-prefix",
            Some("--log-prefix"),
            Some("--no-log-prefix"),
            "LLAMA_ARG_LOG_PREFIX"
        ),
        "llama.cpp.log_timestamps" => toggle!(
            "--log-timestamps",
            Some("--log-timestamps"),
            Some("--no-log-timestamps"),
            "LLAMA_ARG_LOG_TIMESTAMPS"
        ),
        "llama.cpp.offline" => flag!("--offline", "LLAMA_ARG_OFFLINE"),
        "llama.cpp.samplers" => {
            let mut setting = value!("--samplers")?;
            setting.list_separator = ";";
            Some(setting)
        }
        "llama.cpp.ignore_eos" => flag!("--ignore-eos"),
        "llama.cpp.top_n_sigma" => value!("--top-nsigma"),
        "llama.cpp.xtc_probability" => value!("--xtc-probability"),
        "llama.cpp.xtc_threshold" => value!("--xtc-threshold"),
        "llama.cpp.typical_p" => value!("--typical"),
        "llama.cpp.repeat_last_n" => value!("--repeat-last-n"),
        "llama.cpp.dry_multiplier" => value!("--dry-multiplier"),
        "llama.cpp.dry_base" => value!("--dry-base"),
        "llama.cpp.dry_allowed_length" => value!("--dry-allowed-length"),
        "llama.cpp.dry_penalty_last_n" => value!("--dry-penalty-last-n"),
        "llama.cpp.dry_sequence_breakers" => value!("--dry-sequence-breaker"),
        "llama.cpp.adaptive_target" => value!("--adaptive-target"),
        "llama.cpp.adaptive_decay" => value!("--adaptive-decay"),
        "llama.cpp.dynatemp_range" => value!("--dynatemp-range"),
        "llama.cpp.dynatemp_exponent" => value!("--dynatemp-exp"),
        "llama.cpp.mirostat" => value!("--mirostat"),
        "llama.cpp.mirostat_learning_rate" => value!("--mirostat-lr"),
        "llama.cpp.mirostat_entropy" => value!("--mirostat-ent"),
        "llama.cpp.backend_sampling" => {
            flag!("--backend-sampling", "LLAMA_ARG_BACKEND_SAMPLING")
        }
        "llama.cpp.draft_kv_cache_k" => {
            value!("--cache-type-k-draft", "LLAMA_ARG_SPEC_DRAFT_CACHE_TYPE_K")
        }
        "llama.cpp.draft_kv_cache_v" => {
            value!("--cache-type-v-draft", "LLAMA_ARG_SPEC_DRAFT_CACHE_TYPE_V")
        }
        "llama.cpp.draft_tokens_max" => {
            value!("--spec-draft-n-max", "LLAMA_ARG_SPEC_DRAFT_N_MAX")
        }
        "llama.cpp.draft_tokens_min" => {
            value!("--spec-draft-n-min", "LLAMA_ARG_SPEC_DRAFT_N_MIN")
        }
        "llama.cpp.draft_probability_min" => {
            value!("--spec-draft-p-min", "LLAMA_ARG_SPEC_DRAFT_P_MIN")
        }
        "llama.cpp.draft_probability_split" => {
            value!("--spec-draft-p-split", "LLAMA_ARG_SPEC_DRAFT_P_SPLIT")
        }
        "llama.cpp.draft_threads" => value!("--spec-draft-threads"),
        "llama.cpp.draft_threads_batch" => value!("--spec-draft-threads-batch"),
        "llama.cpp.draft_cpu_mask" => value!("--spec-draft-cpu-mask"),
        "llama.cpp.draft_cpu_range" => value!("--spec-draft-cpu-range"),
        "llama.cpp.draft_cpu_strict" => value!("--spec-draft-cpu-strict"),
        "llama.cpp.draft_priority" => value!("--spec-draft-prio"),
        "llama.cpp.draft_poll" => value!("--spec-draft-poll"),
        "llama.cpp.draft_cpu_mask_batch" => value!("--spec-draft-cpu-mask-batch"),
        "llama.cpp.draft_cpu_strict_batch" => value!("--spec-draft-cpu-strict-batch"),
        "llama.cpp.draft_priority_batch" => value!("--spec-draft-prio-batch"),
        "llama.cpp.draft_poll_batch" => value!("--spec-draft-poll-batch"),
        "llama.cpp.draft_override_tensor" => value!("--spec-draft-override-tensor"),
        "llama.cpp.draft_cpu_moe" => flag!("--spec-draft-cpu-moe", "LLAMA_ARG_SPEC_DRAFT_CPU_MOE"),
        "llama.cpp.draft_cpu_moe_layers" => {
            value!("--spec-draft-n-cpu-moe", "LLAMA_ARG_SPEC_DRAFT_N_CPU_MOE")
        }
        "llama.cpp.draft_gpu_offload" => value!("--spec-draft-ngl", "LLAMA_ARG_N_GPU_LAYERS_DRAFT"),
        "llama.cpp.draft_backend_sampling" => toggle!(
            "--spec-draft-backend-sampling",
            Some("--spec-draft-backend-sampling"),
            Some("--no-spec-draft-backend-sampling"),
            "LLAMA_ARG_SPEC_DRAFT_BACKEND_SAMPLING"
        ),
        "llama.cpp.ngram_mod_min" => value!("--spec-ngram-mod-n-min"),
        "llama.cpp.ngram_mod_max" => value!("--spec-ngram-mod-n-max"),
        "llama.cpp.ngram_mod_match" => value!("--spec-ngram-mod-n-match"),
        "llama.cpp.ngram_simple_size_n" => value!("--spec-ngram-simple-size-n"),
        "llama.cpp.ngram_simple_size_m" => value!("--spec-ngram-simple-size-m"),
        "llama.cpp.ngram_simple_min_hits" => value!("--spec-ngram-simple-min-hits"),
        "llama.cpp.ngram_map_size_n" => value!("--spec-ngram-map-k-size-n"),
        "llama.cpp.ngram_map_size_m" => value!("--spec-ngram-map-k-size-m"),
        "llama.cpp.ngram_map_min_hits" => value!("--spec-ngram-map-k-min-hits"),
        "llama.cpp.ngram_map4_size_n" => value!("--spec-ngram-map-k4v-size-n"),
        "llama.cpp.ngram_map4_size_m" => value!("--spec-ngram-map-k4v-size-m"),
        "llama.cpp.ngram_map4_min_hits" => value!("--spec-ngram-map-k4v-min-hits"),
        "llama.cpp.kv_per_slot" => {
            value!("--kv-unified-per-slot", "LLAMA_ARG_KV_UNIFIED_PER_SLOT")
        }
        "llama.cpp.checkpoint_min_step" => value!(
            "--checkpoint-min-step",
            "LLAMA_ARG_CHECKPOINT_MIN_SPACING_NT"
        ),
        "llama.cpp.cache_ram_mib" => value!("--cache-ram", "LLAMA_ARG_CACHE_RAM"),
        "llama.cpp.cache_idle_slots" => toggle!(
            "--cache-idle-slots",
            Some("--cache-idle-slots"),
            Some("--no-cache-idle-slots"),
            "LLAMA_ARG_CACHE_IDLE_SLOTS"
        ),
        "llama.cpp.context_shift" => toggle!(
            "--context-shift",
            Some("--context-shift"),
            Some("--no-context-shift"),
            "LLAMA_ARG_CONTEXT_SHIFT"
        ),
        "llama.cpp.warmup" => toggle!("--warmup", Some("--warmup"), Some("--no-warmup")),
        "llama.cpp.continuous_batching" => toggle!(
            "--cont-batching",
            Some("--cont-batching"),
            Some("--no-cont-batching"),
            "LLAMA_ARG_CONT_BATCHING"
        ),
        "llama.cpp.timeout_seconds" => value!("--timeout", "LLAMA_ARG_TIMEOUT"),
        "llama.cpp.sse_ping_interval" => {
            value!("--sse-ping-interval", "LLAMA_ARG_SSE_PING_INTERVAL")
        }
        "llama.cpp.http_threads" => value!("--threads-http", "LLAMA_ARG_THREADS_HTTP"),
        "llama.cpp.prompt_cache" => toggle!(
            "--cache-prompt",
            Some("--cache-prompt"),
            Some("--no-cache-prompt"),
            "LLAMA_ARG_CACHE_PROMPT"
        ),
        "llama.cpp.cache_reuse" => value!("--cache-reuse", "LLAMA_ARG_CACHE_REUSE"),
        "llama.cpp.metrics" => flag!("--metrics", "LLAMA_ARG_ENDPOINT_METRICS"),
        "llama.cpp.slots_endpoint" => toggle!(
            "--slots",
            Some("--slots"),
            Some("--no-slots"),
            "LLAMA_ARG_ENDPOINT_SLOTS"
        ),
        "llama.cpp.slot_save_path" => value!("--slot-save-path"),
        "llama.cpp.jinja" => toggle!(
            "--jinja",
            Some("--jinja"),
            Some("--no-jinja"),
            "LLAMA_ARG_JINJA"
        ),
        "llama.cpp.reasoning_format" => value!("--reasoning-format", "LLAMA_ARG_THINK"),
        "llama.cpp.reasoning_preserve" => toggle!(
            "--reasoning-preserve",
            Some("--reasoning-preserve"),
            Some("--no-reasoning-preserve"),
            "LLAMA_ARG_REASONING_PRESERVE"
        ),
        "llama.cpp.chat_template_kwargs" => {
            value!("--chat-template-kwargs", "LLAMA_ARG_CHAT_TEMPLATE_KWARGS")
        }
        "llama.cpp.skip_chat_parsing" => toggle!(
            "--skip-chat-parsing",
            Some("--skip-chat-parsing"),
            Some("--no-skip-chat-parsing"),
            "LLAMA_ARG_SKIP_CHAT_PARSING"
        ),
        "llama.cpp.prefill_assistant" => toggle!(
            "--prefill-assistant",
            Some("--prefill-assistant"),
            Some("--no-prefill-assistant"),
            "LLAMA_ARG_PREFILL_ASSISTANT"
        ),
        "llama.cpp.slot_prompt_similarity" => value!("--slot-prompt-similarity"),
        "llama.cpp.lora_init_without_apply" => flag!("--lora-init-without-apply"),
        "llama.cpp.sleep_idle_seconds" => value!("--sleep-idle-seconds"),
        "llama.cpp.log_prompts_dir" => value!("--log-prompts-dir"),
        _ => None,
    }
}

fn llama_direct_aliases(id: &str, required: &'static str) -> Vec<&'static str> {
    let aliases: &'static [&'static str] = match id {
        "llama.cpp.threads_batch" => &["-tb", "--threads-batch"],
        "llama.cpp.cpu_mask" => &["-C", "--cpu-mask"],
        "llama.cpp.cpu_range" => &["-Cr", "--cpu-range"],
        "llama.cpp.cpu_mask_batch" => &["-Cb", "--cpu-mask-batch"],
        "llama.cpp.cpu_range_batch" => &["-Crb", "--cpu-range-batch"],
        "llama.cpp.performance_timings" => &["--perf", "--no-perf"],
        "llama.cpp.escape_sequences" => &["-e", "--escape", "--no-escape"],
        "llama.cpp.weight_repacking" => &["--repack", "-nr", "--no-repack"],
        "llama.cpp.lazy_mode" => &["-lzm", "--lazy-mode"],
        "llama.cpp.override_tensor" => &["-ot", "--override-tensor"],
        "llama.cpp.cpu_ffn_layers" => &["-ncffn", "--n-cpu-ffn"],
        "llama.cpp.split_mode" => &["-sm", "--split-mode"],
        "llama.cpp.tensor_split" => &["-ts", "--tensor-split"],
        "llama.cpp.main_gpu" => &["-mg", "--main-gpu"],
        "llama.cpp.fit" => &["-fit", "--fit"],
        "llama.cpp.fit_target_mib" => &["-fitt", "--fit-target"],
        "llama.cpp.fit_min_context" => &["-fitc", "--fit-ctx"],
        "llama.cpp.log_verbosity" => &["-lv", "--verbosity", "--log-verbosity"],
        "llama.cpp.top_n_sigma" => &["--top-nsigma", "--top-n-sigma"],
        "llama.cpp.typical_p" => &["--typical", "--typical-p"],
        "llama.cpp.backend_sampling" => &["-bs", "--backend-sampling"],
        "llama.cpp.draft_kv_cache_k" => &["--spec-draft-type-k", "-ctkd", "--cache-type-k-draft"],
        "llama.cpp.draft_kv_cache_v" => &["--spec-draft-type-v", "-ctvd", "--cache-type-v-draft"],
        "llama.cpp.draft_threads" => &["--spec-draft-threads", "-td", "--threads-draft"],
        "llama.cpp.draft_threads_batch" => &[
            "--spec-draft-threads-batch",
            "-tbd",
            "--threads-batch-draft",
        ],
        "llama.cpp.draft_cpu_mask" => &["--spec-draft-cpu-mask", "-Cd", "--cpu-mask-draft"],
        "llama.cpp.draft_cpu_range" => &["--spec-draft-cpu-range", "-Crd", "--cpu-range-draft"],
        "llama.cpp.draft_cpu_strict" => &["--spec-draft-cpu-strict", "--cpu-strict-draft"],
        "llama.cpp.draft_priority" => &["--spec-draft-prio", "--prio-draft"],
        "llama.cpp.draft_poll" => &["--spec-draft-poll", "--poll-draft"],
        "llama.cpp.draft_cpu_mask_batch" => &[
            "--spec-draft-cpu-mask-batch",
            "-Cbd",
            "--cpu-mask-batch-draft",
        ],
        "llama.cpp.draft_cpu_strict_batch" => {
            &["--spec-draft-cpu-strict-batch", "--cpu-strict-batch-draft"]
        }
        "llama.cpp.draft_priority_batch" => &["--spec-draft-prio-batch", "--prio-batch-draft"],
        "llama.cpp.draft_poll_batch" => &["--spec-draft-poll-batch", "--poll-batch-draft"],
        "llama.cpp.draft_override_tensor" => &[
            "--spec-draft-override-tensor",
            "-otd",
            "--override-tensor-draft",
        ],
        "llama.cpp.draft_cpu_moe" => &["--spec-draft-cpu-moe", "-cmoed", "--cpu-moe-draft"],
        "llama.cpp.draft_cpu_moe_layers" => &[
            "--spec-draft-n-cpu-moe",
            "--spec-draft-ncmoe",
            "-ncmoed",
            "--n-cpu-moe-draft",
        ],
        "llama.cpp.draft_gpu_offload" => &[
            "--spec-draft-ngl",
            "-ngld",
            "--gpu-layers-draft",
            "--n-gpu-layers-draft",
        ],
        "llama.cpp.draft_probability_split" => &["--spec-draft-p-split", "--draft-p-split"],
        "llama.cpp.draft_probability_min" => &["--spec-draft-p-min", "--draft-p-min"],
        "llama.cpp.context_checkpoints" => &["-ctxcp", "--ctx-checkpoints", "--swa-checkpoints"],
        "llama.cpp.checkpoint_min_step" => &["-cms", "--checkpoint-min-step"],
        "llama.cpp.cache_ram_mib" => &["-cram", "--cache-ram"],
        "llama.cpp.continuous_batching" => {
            &["-cb", "--cont-batching", "-nocb", "--no-cont-batching"]
        }
        "llama.cpp.timeout_seconds" => &["-to", "--timeout"],
        "llama.cpp.slot_prompt_similarity" => &["-sps", "--slot-prompt-similarity"],
        _ => return vec![required],
    };
    aliases.to_vec()
}

fn llama_setting_contract(id: &str) -> Vec<&'static str> {
    if let Some(setting) = llama_direct_setting(id) {
        return vec![setting.required];
    }
    match id {
        "llama.cpp.devices" => vec!["--device"],
        "llama.cpp.context_length" => vec!["--ctx-size"],
        "llama.cpp.parallel_requests" => vec!["--parallel"],
        "llama.cpp.temperature" => vec!["--temp"],
        "llama.cpp.top_p" => vec!["--top-p"],
        "llama.cpp.top_k" => vec!["--top-k"],
        "llama.cpp.min_p" => vec!["--min-p"],
        "llama.cpp.seed" => vec!["--seed"],
        "llama.cpp.repeat_penalty" => vec!["--repeat-penalty"],
        "llama.cpp.presence_penalty" => vec!["--presence-penalty"],
        "llama.cpp.frequency_penalty" => vec!["--frequency-penalty"],
        "llama.cpp.max_output_tokens" => vec!["--predict"],
        "llama.cpp.stop_strings" => vec!["--reverse-prompt"],
        "llama.cpp.reasoning" => vec!["--reasoning"],
        "llama.cpp.reasoning_effort" => vec!["--reasoning-effort"],
        "llama.cpp.reasoning_budget" => vec!["--reasoning-budget"],
        "llama.cpp.reasoning_budget_message" => vec!["--reasoning-budget-message"],
        "llama.cpp.structured_output_schema" => vec!["--json-schema"],
        "llama.cpp.context_overflow" => vec!["--ctx-size"],
        "llama.cpp.threads" => vec!["--threads"],
        "llama.cpp.batch_size" => vec!["--batch-size"],
        "llama.cpp.micro_batch_size" => vec!["--ubatch-size"],
        "llama.cpp.gpu_offload" => vec!["--n-gpu-layers"],
        "llama.cpp.flash_attention" => vec!["--flash-attn"],
        "llama.cpp.kv_cache_k" => vec!["--cache-type-k"],
        "llama.cpp.kv_cache_v" => vec!["--cache-type-v"],
        "llama.cpp.load_mode" => vec!["--load-mode"],
        "llama.cpp.rope_frequency_base" => vec!["--rope-freq-base"],
        "llama.cpp.rope_frequency_scale" => vec!["--rope-freq-scale"],
        "llama.cpp.unified_kv_cache" => vec!["--kv-unified", "--no-kv-unified"],
        "llama.cpp.kv_cache_gpu_offload" => vec!["--kv-offload", "--no-kv-offload"],
        "llama.cpp.context_checkpoints" => vec!["--ctx-checkpoints"],
        "llama.cpp.cpu_moe_layers" => vec!["--n-cpu-moe"],
        "llama.cpp.cpu_moe_all" => vec!["--cpu-moe"],
        "llama.cpp.active_experts" => vec!["--override-kv"],
        "llama.cpp.chat_template" => vec!["--chat-template"],
        "llama.cpp.chat_template_file" | "llama.cpp.chat_template_sha256" => {
            vec!["--jinja", "--chat-template-file"]
        }
        "llama.cpp.speculative_mode" => vec!["--spec-type"],
        "llama.cpp.speculative_draft_model" | "llama.cpp.speculative_draft_sha256" => {
            vec!["--spec-draft-model"]
        }
        _ => vec![],
    }
}

fn llama_setting_has_execution_path(id: &str) -> bool {
    llama_direct_setting(id).is_some()
        || matches!(
            id,
            "llama.cpp.devices"
                | "llama.cpp.context_length"
                | "llama.cpp.parallel_requests"
                | "llama.cpp.temperature"
                | "llama.cpp.top_p"
                | "llama.cpp.top_k"
                | "llama.cpp.min_p"
                | "llama.cpp.seed"
                | "llama.cpp.repeat_penalty"
                | "llama.cpp.presence_penalty"
                | "llama.cpp.frequency_penalty"
                | "llama.cpp.max_output_tokens"
                | "llama.cpp.stop_strings"
                | "llama.cpp.system_prompt"
                | "llama.cpp.reasoning"
                | "llama.cpp.reasoning_effort"
                | "llama.cpp.reasoning_budget"
                | "llama.cpp.reasoning_budget_message"
                | "llama.cpp.structured_output_schema"
                | "llama.cpp.context_overflow"
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
                "the Scala llama.cpp adapter has no executable path for this setting".to_owned(),
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
            "llama.cpp.system_prompt" => {}
            "llama.cpp.reasoning_effort" => {
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
                        "the exact llama-server does not advertise any Scala-understood cache types"
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
                        "the exact llama-server does not advertise any Scala-understood load modes"
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
    // An advertised, implemented control can remain supported without a known
    // default. Inheritance omits it; explicit overrides still validate normally.
}

fn apply_llama_reported_default(definition: &mut SettingDefinition, contract: &str) {
    if definition.default_preview.as_ref().is_some_and(|preview| {
        matches!(
            preview.source,
            SettingDefaultSource::Scala | SettingDefaultSource::Model
        )
    }) {
        return;
    }
    let id = definition.id.as_str();
    if id == "llama.cpp.chat_template"
        && contract
            .to_ascii_lowercase()
            .contains("template taken from model")
    {
        definition.default_preview = Some(
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime selects the template from model metadata"),
        );
        return;
    }
    let Some(reported) = llama_help_reported_default(contract) else {
        return;
    };
    let lower = reported.to_ascii_lowercase();
    let preview = match id {
        _ if lower == "same" => SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
            .with_detail("The exact runtime derives this value from its related primary worker setting"),
        _ if lower == "loaded" => SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
            .with_detail("The exact runtime loads this value from selected model metadata"),
        "llama.cpp.yarn_original_context" if lower == "0" => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime uses the model training context")
        }
        "llama.cpp.yarn_extrapolation_factor"
        | "llama.cpp.yarn_attention_factor"
        | "llama.cpp.yarn_beta_slow"
        | "llama.cpp.yarn_beta_fast"
            if lower == "-1.00" || lower == "-1" =>
        {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime derives this YaRN parameter")
        }
        "llama.cpp.http_threads" if lower == "-1" => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime selects its HTTP worker count")
        }
        "llama.cpp.parallel_requests" if lower == "-1" => SettingDefaultPreview::new(
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
        "llama.cpp.context_length" if matches!(lower.as_str(), "0" | "auto") => {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime defers context selection to model metadata and its runtime fallback")
        }
        "llama.cpp.rope_frequency_base" | "llama.cpp.rope_frequency_scale"
            if matches!(lower.as_str(), "0" | "auto") =>
        {
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
                .with_detail("The exact runtime defers RoPE selection to model metadata and its runtime fallback")
        }
        "llama.cpp.seed" if matches!(lower.as_str(), "-1" | "random") => {
            SettingDefaultPreview::new("random", SettingDefaultSource::Runtime)
        }
        "llama.cpp.max_output_tokens" if lower == "-1" => {
            SettingDefaultPreview::new("unlimited", SettingDefaultSource::Runtime)
        }
        "llama.cpp.reasoning" if lower == "auto" => SettingDefaultPreview::new(
            "auto",
            SettingDefaultSource::Runtime,
        )
        .with_detail(
            "The exact runtime detects the reasoning mode from the selected chat template",
        ),
        "llama.cpp.reasoning_effort" if lower == "default" => SettingDefaultPreview::new(
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
    if contract[index + marker_len..]
        .trim_start()
        .starts_with("\"\"")
    {
        return Some("\"\"".to_owned());
    }
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
    settings: &scala_core::ResolvedSettings,
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
                "draft-mtp uses MTP heads from the main model, but Scala's bounded GGUF identity cannot statically prove that the target exposes usable MTP heads; exact llama-server load remains authoritative"
                    .to_owned(),
            )
        }
        Ok(()) if settings.value("llama.cpp.speculative_draft_model").is_some() => {
            RuntimeCompatibility::NeedsAttention(
                "draft tokenizer identity is checked by Scala; the exact llama-server launch remains authoritative for draft architecture/tensor compatibility"
                    .to_owned(),
            )
        }
        Ok(()) => RuntimeCompatibility::Compatible,
        Err(error) => RuntimeCompatibility::Incompatible(error.to_string()),
    }
}

fn llama_normalized_generation_defaults(
    settings: &scala_core::ResolvedSettings,
) -> BTreeMap<String, Value> {
    [
        "llama.cpp.temperature",
        "llama.cpp.top_p",
        "llama.cpp.top_k",
        "llama.cpp.min_p",
    ]
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

// Extra upstream controls are deliberately ignored; only owned launch controls
// are required here. Configured settings have their own help contracts.
fn validate_llama_launch_help(help: &str) -> Result<(), EngineError> {
    for required in ["--model", "--alias", "--host", "--port"] {
        if !help_has_option(help, required) {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime entrypoint does not advertise required llama-server flag `{required}`"
            )));
        }
    }
    Ok(())
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
    settings: &scala_core::ResolvedSettings,
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

fn validate_llama_accelerator_settings(
    settings: &scala_core::ResolvedSettings,
    binding: &AcceleratorBinding,
) -> Result<(), EngineError> {
    let device_count = binding.devices.len();
    if device_count == 0 {
        return Err(EngineError::InvalidConfiguration(
            "llama.cpp accelerator binding contains no devices".to_owned(),
        ));
    }
    if let Some(SettingValue::UnsignedInteger(main_gpu)) = settings.value("llama.cpp.main_gpu")
        && usize::try_from(*main_gpu).map_or(true, |index| index >= device_count)
    {
        return Err(EngineError::InvalidConfiguration(format!(
            "`llama.cpp.main_gpu` index {main_gpu} is outside the selected {device_count}-device CUDA binding"
        )));
    }
    if let Some(SettingValue::StringList(values)) = settings.value("llama.cpp.tensor_split") {
        let values = llama_per_device_values(values, "llama.cpp.tensor_split")?;
        if values.len() > device_count {
            return Err(EngineError::InvalidConfiguration(format!(
                "`llama.cpp.tensor_split` supplies {} proportions for a {device_count}-device CUDA binding",
                values.len()
            )));
        }
        for value in values {
            let parsed = value.parse::<f32>().map_err(|_| {
                EngineError::InvalidConfiguration(format!(
                    "`llama.cpp.tensor_split` value `{value}` is not a numeric proportion"
                ))
            })?;
            if !parsed.is_finite() || parsed < 0.0 {
                return Err(EngineError::InvalidConfiguration(format!(
                    "`llama.cpp.tensor_split` value `{value}` must be a finite non-negative proportion"
                )));
            }
        }
    }
    if let Some(SettingValue::StringList(values)) = settings.value("llama.cpp.fit_target_mib") {
        let values = llama_per_device_values(values, "llama.cpp.fit_target_mib")?;
        if values.len() != 1 && values.len() > device_count {
            return Err(EngineError::InvalidConfiguration(format!(
                "`llama.cpp.fit_target_mib` supplies {} margins for a {device_count}-device CUDA binding; upstream accepts one broadcast value or at most one value per selected device",
                values.len()
            )));
        }
        for value in values {
            value.parse::<u64>().map_err(|_| {
                EngineError::InvalidConfiguration(format!(
                    "`llama.cpp.fit_target_mib` value `{value}` is not a non-negative integer MiB margin"
                ))
            })?;
        }
    }
    Ok(())
}

fn llama_per_device_values<'a>(
    values: &'a [String],
    setting: &str,
) -> Result<Vec<&'a str>, EngineError> {
    let values = values
        .iter()
        .flat_map(|value| value.split([',', '/']))
        .collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| value.is_empty()) {
        return Err(EngineError::InvalidConfiguration(format!(
            "`{setting}` contains an empty per-device value"
        )));
    }
    Ok(values)
}

async fn validate_llama_bound_files(
    settings: &scala_core::ResolvedSettings,
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
        let draft = scala_core::inspect_gguf_metadata(path).map_err(|error| {
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

async fn bind_llama_inference_files(
    settings: &mut scala_core::ResolvedSettings,
) -> Result<BTreeMap<String, Value>, EngineError> {
    const MAXIMUM_INFERENCE_INPUT_BYTES: u64 = 1024_u64 * 1024 * 1024 * 1024;

    let mut provenance = BTreeMap::new();
    for (setting_id, scaled) in [
        ("llama.cpp.lora_adapters", false),
        ("llama.cpp.lora_scaled", true),
        ("llama.cpp.control_vectors", false),
        ("llama.cpp.control_vectors_scaled", true),
    ] {
        let id = SettingId::new(setting_id).expect("static llama.cpp setting ID");
        let Some(SettingValue::StringList(configured)) = settings.value(setting_id).cloned() else {
            continue;
        };
        let mut launch_values = Vec::with_capacity(configured.len());
        let mut identities = Vec::with_capacity(configured.len());
        for configured_entry in configured {
            let (configured_path, scale) = if scaled {
                let (path, scale) = configured_entry.rsplit_once(':').ok_or_else(|| {
                    EngineError::InvalidConfiguration(format!(
                        "`{setting_id}` entry `{configured_entry}` must use PATH:SCALE"
                    ))
                })?;
                let parsed = scale.parse::<f64>().map_err(|_| {
                    EngineError::InvalidConfiguration(format!(
                        "`{setting_id}` entry `{configured_entry}` has an invalid scale"
                    ))
                })?;
                if !parsed.is_finite() {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "`{setting_id}` entry `{configured_entry}` has a non-finite scale"
                    )));
                }
                (path, Some((scale, parsed)))
            } else {
                (configured_entry.as_str(), None)
            };
            if configured_path.is_empty() {
                return Err(EngineError::InvalidConfiguration(format!(
                    "`{setting_id}` contains an empty file path"
                )));
            }
            let canonical = tokio::fs::canonicalize(configured_path)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "could not resolve `{setting_id}` file `{configured_path}`: {error}"
                    ))
                })?;
            let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "could not inspect `{setting_id}` file {}: {error}",
                    canonical.display()
                ))
            })?;
            if !metadata.is_file() || metadata.len() > MAXIMUM_INFERENCE_INPUT_BYTES {
                return Err(EngineError::InvalidConfiguration(format!(
                    "`{setting_id}` file {} must be regular and no larger than {MAXIMUM_INFERENCE_INPUT_BYTES} bytes",
                    canonical.display()
                )));
            }
            let digest = scala_core::bounded_setting_file_sha256(
                &id,
                &canonical,
                Path::new("/"),
                MAXIMUM_INFERENCE_INPUT_BYTES,
            )
            .await
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
            if let Some((scale_text, scale_value)) = scale {
                launch_values.push(format!("{}:{scale_text}", canonical.display()));
                identities.push(json!({
                    "path": canonical,
                    "sha256": digest,
                    "scale": scale_value,
                }));
            } else {
                launch_values.push(canonical.display().to_string());
                identities.push(json!({
                    "path": canonical,
                    "sha256": digest,
                }));
            }
        }
        let resolved = settings.configured.get_mut(&id).ok_or_else(|| {
            EngineError::InvalidConfiguration(format!(
                "resolved setting `{setting_id}` disappeared while binding files"
            ))
        })?;
        resolved.value = SettingValue::StringList(launch_values);
        provenance.insert(
            format!("bound_files.{setting_id}"),
            Value::Array(identities),
        );
    }
    Ok(provenance)
}

async fn verify_bound_file(
    settings: &scala_core::ResolvedSettings,
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
        scala_core::bounded_setting_file_sha256(&id, path, Path::new("/"), maximum_bytes)
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
    settings: &scala_core::ResolvedSettings,
    model: Option<&ModelArtifact>,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<LlamaStructuredArguments, EngineError> {
    let mut arguments = Vec::new();
    let mut environment_remove = Vec::new();
    for (id, resolved) in &settings.configured {
        if let Some(direct) = llama_direct_setting(id.as_str()) {
            let mut aliases = direct.aliases.to_vec();
            aliases.extend(llama_direct_aliases(id.as_str(), direct.required));
            if let LlamaDirectMode::Toggle { enabled, disabled } = direct.mode {
                aliases.extend(enabled);
                aliases.extend(disabled);
            }
            aliases.sort_unstable();
            aliases.dedup();
            if let Some(argument) = find_native_option(native_arguments, &aliases) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "structured setting `{id}` conflicts with native llama.cpp argument `{argument}`"
                )));
            }
            if let Some(name) = configured_environment.keys().find(|name| {
                direct
                    .environment
                    .iter()
                    .any(|owned| name.eq_ignore_ascii_case(owned))
            }) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "structured setting `{id}` conflicts with configured llama.cpp environment variable `{name}`"
                )));
            }
            environment_remove.extend(direct.environment.iter().map(OsString::from));
            push_llama_direct_argument(&mut arguments, id.as_str(), &resolved.value, direct)?;
            continue;
        }
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
            ("llama.cpp.devices", SettingValue::StringList(_)) => {}
            ("llama.cpp.context_length", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--ctx-size", *value);
            }
            ("llama.cpp.parallel_requests", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--parallel", *value);
            }
            ("llama.cpp.temperature", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--temp", *value);
            }
            ("llama.cpp.top_p", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--top-p", *value);
            }
            ("llama.cpp.top_k", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--top-k", *value);
            }
            ("llama.cpp.min_p", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--min-p", *value);
            }
            ("llama.cpp.seed", SettingValue::UnsignedIntegerOrChoice(value)) => {
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
            ("llama.cpp.repeat_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--repeat-penalty", *value);
            }
            ("llama.cpp.presence_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--presence-penalty", *value);
            }
            ("llama.cpp.frequency_penalty", SettingValue::Float(value)) => {
                push_value_argument(&mut arguments, "--frequency-penalty", *value);
            }
            ("llama.cpp.max_output_tokens", SettingValue::UnsignedInteger(value)) => {
                push_value_argument(&mut arguments, "--predict", *value);
            }
            ("llama.cpp.stop_strings", SettingValue::StringList(values)) => {
                for value in values {
                    push_value_argument(&mut arguments, "--reverse-prompt", value);
                }
            }
            ("llama.cpp.reasoning", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--reasoning", value);
            }
            ("llama.cpp.reasoning_effort", SettingValue::Choice(value)) => {
                push_value_argument(&mut arguments, "--reasoning-effort", value);
            }
            ("llama.cpp.reasoning_budget", SettingValue::Integer(value)) => {
                push_value_argument(&mut arguments, "--reasoning-budget", *value);
            }
            ("llama.cpp.reasoning_budget_message", SettingValue::String(value)) => {
                push_value_argument(&mut arguments, "--reasoning-budget-message", value);
            }
            ("llama.cpp.structured_output_schema", SettingValue::Json(_)) => {}
            ("llama.cpp.system_prompt" | "llama.cpp.context_overflow", _) => {}
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

fn push_llama_direct_argument(
    arguments: &mut Vec<OsString>,
    id: &str,
    value: &SettingValue,
    setting: LlamaDirectSetting,
) -> Result<(), EngineError> {
    match setting.mode {
        LlamaDirectMode::Flag => {
            if !matches!(value, SettingValue::FlagEnabled) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` must be enabled or unset"
                )));
            }
            arguments.push(OsString::from(setting.required));
            return Ok(());
        }
        LlamaDirectMode::Toggle { enabled, disabled } => {
            let SettingValue::Toggle(value) = value else {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` must be a toggle"
                )));
            };
            if let Some(option) = if *value { enabled } else { disabled } {
                arguments.push(OsString::from(option));
            }
            return Ok(());
        }
        LlamaDirectMode::Value => {}
    }
    let rendered = match value {
        SettingValue::Toggle(value) => {
            if id == "llama.cpp.fit" {
                if *value {
                    "on".to_owned()
                } else {
                    "off".to_owned()
                }
            } else if *value {
                "1".to_owned()
            } else {
                "0".to_owned()
            }
        }
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::UnsignedInteger(value) => value.to_string(),
        SettingValue::Float(value) => value.to_string(),
        SettingValue::String(value) | SettingValue::Choice(value) => match (id, value.as_str()) {
            ("llama.cpp.priority", "low") => "-1".to_owned(),
            (id, "normal") if id.contains("priority") => "0".to_owned(),
            (id, "medium") if id.contains("priority") => "1".to_owned(),
            (id, "high") if id.contains("priority") => "2".to_owned(),
            (id, "realtime") if id.contains("priority") => "3".to_owned(),
            _ => value.clone(),
        },
        SettingValue::Path(value) => value.display().to_string(),
        SettingValue::GpuOffload(value) => match value {
            GpuOffload::None => "0".to_owned(),
            GpuOffload::Auto => "auto".to_owned(),
            GpuOffload::All => "all".to_owned(),
            GpuOffload::Layers(value) => value.to_string(),
        },
        SettingValue::Json(value) => serde_json::to_string(value).map_err(|error| {
            EngineError::InvalidConfiguration(format!("could not serialize `{id}`: {error}"))
        })?,
        SettingValue::StringList(values) => values.join(setting.list_separator),
        _ => {
            return Err(EngineError::InvalidConfiguration(format!(
                "setting `{id}` has an invalid value for llama.cpp"
            )));
        }
    };
    arguments.push(OsString::from(setting.required));
    if id == "llama.cpp.control_vector_layer_range" {
        let values = rendered.split_whitespace().collect::<Vec<_>>();
        if values.len() != 2 {
            return Err(EngineError::InvalidConfiguration(
                "llama.cpp.control_vector_layer_range requires `START END`".to_owned(),
            ));
        }
        arguments.extend(values.into_iter().map(OsString::from));
    } else if id == "llama.cpp.dry_sequence_breakers" {
        arguments.pop();
        let SettingValue::StringList(values) = value else {
            unreachable!()
        };
        for value in values {
            push_value_argument(arguments, setting.required, value);
        }
    } else {
        arguments.push(OsString::from(rendered));
    }
    Ok(())
}

impl LlamaCppAdapter {
    async fn send_completion(
        &self,
        endpoint: &str,
        route: &str,
        body: Value,
    ) -> Result<InferenceOutput, EngineError> {
        let constraint = chat::OutputConstraint::from_body(&body)?;
        let native_prefill_verified = self.native_prefill_verified(endpoint).await;
        let response = self
            .client
            .post(format!("{endpoint}{route}"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
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
        if response.choices.len() > 1 {
            return Err(EngineError::Operation(
                "llama.cpp returned multiple completion choices".into(),
            ));
        }
        let choice = response.choices.into_iter().next().ok_or_else(|| {
            EngineError::Operation("llama.cpp response contained no completion choice".to_owned())
        })?;
        if route == "/v1/completions"
            && (choice.text.is_none() || choice.finish_reason.as_deref() == Some("tool_calls"))
        {
            return Err(EngineError::Operation(
                "raw completion returned an invalid choice".to_owned(),
            ));
        }

        let finish_reason = map_finish_reason(choice.finish_reason.as_deref())?;
        let (content, calls) = choice
            .message
            .map(|m| (m.content, m.tool_calls))
            .unwrap_or_default();
        let tool_calls = chat::parse_calls(calls)?;
        chat::validate_call_finish(&tool_calls, &finish_reason)?;
        let text = choice
            .text
            .or(content)
            .or_else(|| (!tool_calls.is_empty()).then(String::new))
            .ok_or_else(|| {
                EngineError::Operation(
                    "llama.cpp response contained no assistant text or tool calls".to_owned(),
                )
            })?;
        if let Some(constraint) = constraint {
            constraint.validate(&text, &tool_calls, &finish_reason)?;
        }
        Ok(InferenceOutput {
            text,
            tool_calls,
            usage: response.usage.map(|usage| {
                let mut usage = InferenceUsage::from(usage);
                if native_prefill_verified && let Some(timings) = &response.timings {
                    apply_prompt_timings(&mut usage, timings);
                }
                usage
            }),
            finish_reason,
        })
    }

    async fn send_completion_stream(
        &self,
        endpoint: &str,
        route: &str,
        body: Value,
        activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let constraint = chat::OutputConstraint::from_body(&body)?;
        let native_prefill_verified = self.native_prefill_verified(endpoint).await;
        let response = self
            .client
            .post(format!("{endpoint}{route}"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
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
        let stream = llama_sse_stream(
            native_prefill_verified,
            response.bytes_stream().boxed(),
            activity,
        );
        Ok(chat::validate_stream(stream, constraint))
    }
}

impl LlamaCppAdapter {
    async fn raw_completion_body(
        &self,
        endpoint: &str,
        request: &scala_engine::CompletionRequest,
        stream: bool,
    ) -> Result<Value, EngineError> {
        let settings = &request.generation_settings;
        let _ = endpoint;
        if settings.reasoning_enabled.is_some()
            || settings.reasoning_effort.is_some()
            || settings.reasoning_budget.is_some()
        {
            return Err(EngineError::InvalidGenerationSettings(
                "chat reasoning controls do not apply to raw prompts".to_owned(),
            ));
        }
        let mut body = json!({"model": request.model_profile_id.as_str(), "prompt": request.prompt, "stream": stream});
        for (key, value) in serde_json::to_value(settings)
            .map_err(|e| EngineError::Operation(e.to_string()))?
            .as_object()
            .expect("generation settings object")
        {
            if !value.is_null() {
                body[key] = value.clone();
            }
        }
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }
        Ok(body)
    }
}

fn pooled_embedding_model(model: &ModelArtifact) -> bool {
    matches!(model.native_identity.as_ref(), Some(scala_core::ArtifactNativeIdentity::Gguf(identity)) if matches!(identity.pooling_type, Some(1..=3)))
}

#[cfg(test)]
fn translate_llama_settings(
    settings: &scala_core::ResolvedSettings,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<LlamaStructuredArguments, EngineError> {
    translate_llama_settings_for_model(settings, None, native_arguments, configured_environment)
}

fn llama_setting_collision_contract(
    id: &str,
) -> (&'static [&'static str], &'static [&'static str]) {
    match id {
        "llama.cpp.devices" => (&["-dev", "--device"], &["LLAMA_ARG_DEVICE"]),
        "llama.cpp.context_length" => (&["-c", "--ctx-size"], &["LLAMA_ARG_CTX_SIZE"]),
        "llama.cpp.parallel_requests" => (&["-np", "--parallel"], &["LLAMA_ARG_N_PARALLEL"]),
        "llama.cpp.temperature" => (&["--temp", "--temperature"], &["LLAMA_ARG_TEMP"]),
        "llama.cpp.top_p" => (&["--top-p"], &["LLAMA_ARG_TOP_P"]),
        "llama.cpp.top_k" => (&["--top-k"], &["LLAMA_ARG_TOP_K"]),
        "llama.cpp.min_p" => (&["--min-p"], &["LLAMA_ARG_MIN_P"]),
        "llama.cpp.seed" => (&["-s", "--seed"], &[]),
        "llama.cpp.repeat_penalty" => (&["--repeat-penalty"], &[]),
        "llama.cpp.presence_penalty" => (&["--presence-penalty"], &[]),
        "llama.cpp.frequency_penalty" => (&["--frequency-penalty"], &[]),
        "llama.cpp.max_output_tokens" => (
            &["-n", "--predict", "--n-predict"],
            &["LLAMA_ARG_N_PREDICT"],
        ),
        "llama.cpp.stop_strings" => (&["-r", "--reverse-prompt"], &[]),
        "llama.cpp.system_prompt" | "llama.cpp.context_overflow" => (&[], &[]),
        "llama.cpp.reasoning" => (&["-rea", "--reasoning"], &["LLAMA_ARG_REASONING"]),
        "llama.cpp.reasoning_effort" => (&["--reasoning-effort"], &["LLAMA_ARG_REASONING_EFFORT"]),
        "llama.cpp.reasoning_budget" => (&["--reasoning-budget"], &["LLAMA_ARG_THINK_BUDGET"]),
        "llama.cpp.reasoning_budget_message" => (
            &["--reasoning-budget-message"],
            &["LLAMA_ARG_THINK_BUDGET_MESSAGE"],
        ),
        "llama.cpp.structured_output_schema" => (
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
    MANAGED_NATIVE_ARGUMENTS
        .iter()
        .copied()
        .chain(structured_llama_argument_aliases())
        .any(|managed| {
            argument == managed
                || argument
                    .strip_prefix(managed)
                    .is_some_and(|suffix| suffix.starts_with('='))
        })
}

fn structured_llama_argument_aliases() -> impl Iterator<Item = &'static str> {
    llama_setting_definitions()
        .into_iter()
        .flat_map(|definition| {
            if let Some(setting) = llama_direct_setting(definition.id.as_str()) {
                let mut aliases = setting.aliases.to_vec();
                aliases.extend(llama_direct_aliases(
                    definition.id.as_str(),
                    setting.required,
                ));
                if let LlamaDirectMode::Toggle { enabled, disabled } = setting.mode {
                    aliases.extend(enabled);
                    aliases.extend(disabled);
                }
                aliases
            } else {
                llama_setting_collision_contract(definition.id.as_str())
                    .0
                    .to_vec()
            }
        })
}

fn conflicts_with_managed_environment(name: &str) -> bool {
    is_llama_engine_environment(name)
        || MANAGED_ENVIRONMENT_VARIABLES
            .iter()
            .copied()
            .chain(structured_llama_environment_names())
            .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn managed_environment_removals() -> Vec<OsString> {
    let mut names = MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .copied()
        .chain(structured_llama_environment_names())
        .map(OsString::from)
        .collect::<Vec<_>>();
    names.extend(std::env::vars_os().filter_map(|(name, _)| {
        name.to_str()
            .is_some_and(is_llama_engine_environment)
            .then_some(name)
    }));
    names.sort_unstable();
    names.dedup();
    names
}

fn is_llama_engine_environment(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["LLAMA_", "MTMD_", "GGML_", "LLGUIDANCE_", "AIP_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn structured_llama_environment_names() -> impl Iterator<Item = &'static str> {
    llama_setting_definitions()
        .into_iter()
        .flat_map(|definition| {
            if let Some(setting) = llama_direct_setting(definition.id.as_str()) {
                setting.environment.to_vec()
            } else {
                llama_setting_collision_contract(definition.id.as_str())
                    .1
                    .to_vec()
            }
        })
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
        Some("tool_calls") => Ok(InferenceFinishReason::ToolCalls),
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
    use scala_core::{
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
    use scala_core::ModelProfileId;

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
                    reasoning_effort: Some(scala_engine::ReasoningEffort::High),
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
    use scala_core::{
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
                (
                    "llama.cpp.context_length",
                    SettingValue::UnsignedInteger(131_072),
                ),
                (
                    "llama.cpp.parallel_requests",
                    SettingValue::UnsignedInteger(3),
                ),
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
            ("llama.cpp.temperature", SettingValue::Float(0.7)),
            ("llama.cpp.top_p", SettingValue::Float(0.9)),
            ("llama.cpp.top_k", SettingValue::UnsignedInteger(40)),
            ("llama.cpp.min_p", SettingValue::Float(0.05)),
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
                ("configured_llama.cpp.min_p".to_owned(), json!(0.05)),
                ("configured_llama.cpp.temperature".to_owned(), json!(0.7)),
                ("configured_llama.cpp.top_k".to_owned(), json!(40)),
                ("configured_llama.cpp.top_p".to_owned(), json!(0.9)),
            ])
        );
    }

    #[test]
    fn common_temperature_resolves_from_a_llama_model_profile_and_translates() {
        let profile_id = ModelProfileId::new("llama-quality").expect("profile ID");
        let temperature_id = SettingId::new("llama.cpp.temperature").expect("temperature ID");
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
        let configured = resolved(&[("llama.cpp.temperature", SettingValue::Float(0.7))]);
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
            RuntimeCompatibility::Compatible
        ));
        assert!(matches!(
            llama_configured_runtime_compatibility(&configured, None, Some("  --top-p N  top p")),
            RuntimeCompatibility::Incompatible(_)
        ));

        let all_generation = resolved(&[
            ("llama.cpp.temperature", SettingValue::Float(0.7)),
            ("llama.cpp.top_p", SettingValue::Float(0.9)),
            ("llama.cpp.top_k", SettingValue::UnsignedInteger(40)),
            ("llama.cpp.min_p", SettingValue::Float(0.05)),
        ]);
        let generation_help =
            "  --temp N  temperature\n  --top-p N  top p\n  --top-k N  top k\n  --min-p N  min p";
        assert!(matches!(
            llama_configured_runtime_compatibility(&all_generation, None, Some(generation_help)),
            RuntimeCompatibility::Compatible
        ));
        let mut definitions = llama_model_setting_definitions(None);
        apply_llama_exact_help_contract(&mut definitions, "  --temp N  temperature");
        for id in ["llama.cpp.top_p", "llama.cpp.top_k", "llama.cpp.min_p"] {
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
            .find(|definition| definition.id.as_str() == "llama.cpp.reasoning_effort")
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
                    definition.id.as_str() == "llama.cpp.system_prompt"
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
    fn structured_load_mode_owns_equivalent_environment_and_raw_aliases_are_reserved() {
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

        assert!(conflicts_with_managed_argument("--load-mode=none"));
        assert!(conflicts_with_managed_environment("LLAMA_ARG_MMAP"));
    }

    #[test]
    fn structured_llama_setting_rejects_both_native_argument_forms() {
        let settings = resolved(&[(
            "llama.cpp.context_length",
            SettingValue::UnsignedInteger(8192),
        )]);
        for native in [
            vec!["--ctx-size=4096".to_owned()],
            vec!["-c".to_owned(), "4096".to_owned()],
        ] {
            let error = translate_llama_settings(&settings, &native, &BTreeMap::new())
                .expect_err("native collision");
            assert!(error.to_string().contains("conflicts"));
        }
        for managed in [
            "--device=CUDA1",
            "--rpc",
            "--override-kv",
            "--list-devices",
            "--mmproj",
            "--models-dir",
            "--grammar-file",
            "--logit-bias",
            "--spec-synth-len",
        ] {
            assert!(conflicts_with_managed_argument(managed), "{managed}");
        }
        for managed in [
            "LLAMA_ARG_DEVICE",
            "LLAMA_ARG_RPC",
            "LLAMA_ARG_MMPROJ",
            "LLAMA_ARG_SPEC_SYNTH_LEN",
            "LLAMA_ARG_CTX_SIZE",
        ] {
            assert!(conflicts_with_managed_environment(managed), "{managed}");
        }
        for unknown_engine_control in [
            "LLAMA_ARG_FUTURE_CONTROL",
            "LLAMA_SERVER_FUTURE_CONTROL",
            "MTMD_FUTURE_CONTROL",
            "GGML_FUTURE_CONTROL",
        ] {
            assert!(
                conflicts_with_managed_environment(unknown_engine_control),
                "{unknown_engine_control}"
            );
        }
        assert!(!conflicts_with_managed_environment(
            "ORDINARY_APPLICATION_VALUE"
        ));
        let help = "--model FILE\n--alias NAME\n--host HOST\n--port PORT\n";
        validate_llama_launch_help(help).expect("required launch controls");
        let extended_help = format!("{help}--brand-new-control VALUE\n");
        validate_llama_launch_help(&extended_help).expect("unknown controls are ignored");
        for required in ["--model", "--alias", "--host", "--port"] {
            let missing = extended_help.replace(required, &format!("{required}-unrelated"));
            assert!(validate_llama_launch_help(&missing).is_err(), "{required}");
        }
        let mut definitions = llama_model_setting_definitions(None);
        let known_ids = definitions
            .iter()
            .map(|definition| definition.id.clone())
            .collect::<Vec<_>>();
        apply_llama_exact_help_contract(&mut definitions, &extended_help);
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.id.clone())
                .collect::<Vec<_>>(),
            known_ids
        );

        let raw: EngineConfig = serde_json::from_value(json!({
            "enabled": true,
            "native": { "arguments": ["--brand-new-control"] }
        }))
        .expect("engine config");
        let adapter = LlamaCppAdapter::from_config(Some(&raw), Path::new("."));
        assert!(
            adapter
                .configuration_error
                .as_deref()
                .is_some_and(|error| error.contains("native arguments are disabled"))
        );
        assert!(adapter.native_options().is_empty());
    }

    #[test]
    fn ordered_llama_device_binding_validates_index_and_per_device_arity() {
        let first_uuid = "GPU-11111111-1111-1111-1111-111111111111";
        let second_uuid = "GPU-22222222-2222-2222-2222-222222222222";
        let device = |uuid: &str, vram_gib: u64| AcceleratorDevice {
            accelerator: "cuda".to_owned(),
            stable_id: Some(uuid.to_owned()),
            name: Some(format!("fixture-{vram_gib}")),
            vram_bytes: Some(vram_gib * 1024 * 1024 * 1024),
            driver_version: Some("580.1".to_owned()),
            compute_capability: Some(scala_core::ComputeCapability::new(8, 9)),
        };
        let host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![device(first_uuid, 24), device(second_uuid, 32)],
            nvidia_gpu_absence_confirmed: false,
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        let requirements = scala_core::RuntimeRequirements {
            requires_nvidia_gpu: true,
            ..Default::default()
        };
        let settings = resolved(&[
            (
                "llama.cpp.devices",
                SettingValue::StringList(vec![second_uuid.to_owned(), first_uuid.to_owned()]),
            ),
            ("llama.cpp.main_gpu", SettingValue::UnsignedInteger(1)),
            (
                "llama.cpp.tensor_split",
                SettingValue::StringList(vec!["3,1".to_owned()]),
            ),
            (
                "llama.cpp.fit_target_mib",
                SettingValue::StringList(vec!["1024".to_owned()]),
            ),
        ]);
        let evaluated = llama_device_evaluation(
            "cuda",
            "linux",
            "x86_64",
            &requirements,
            &host,
            Some(&settings),
        );
        let binding = evaluated.binding.expect("explicit binding");
        assert_eq!(binding.devices[0].stable_id.as_deref(), Some(second_uuid));
        assert_eq!(binding.devices[1].stable_id.as_deref(), Some(first_uuid));
        validate_llama_accelerator_settings(&settings, &binding).expect("valid arity");
        assert_eq!(
            combine_llama_compatibility(
                RuntimeCompatibility::Recommended,
                RuntimeCompatibility::Compatible,
            ),
            RuntimeCompatibility::Compatible
        );

        let bad_main = resolved(&[("llama.cpp.main_gpu", SettingValue::UnsignedInteger(2))]);
        assert!(validate_llama_accelerator_settings(&bad_main, &binding).is_err());
        let bad_split = resolved(&[(
            "llama.cpp.tensor_split",
            SettingValue::StringList(vec!["1,1,1".to_owned()]),
        )]);
        assert!(validate_llama_accelerator_settings(&bad_split, &binding).is_err());
        let duplicate = resolved(&[(
            "llama.cpp.devices",
            SettingValue::StringList(vec![first_uuid.to_owned(), first_uuid.to_owned()]),
        )]);
        assert!(matches!(
            llama_device_evaluation(
                "cuda",
                "linux",
                "x86_64",
                &requirements,
                &host,
                Some(&duplicate),
            )
            .compatibility,
            RuntimeCompatibility::Incompatible(_)
        ));
    }

    #[test]
    fn malformed_values_are_rejected_by_the_definition() {
        let definition = llama_setting_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "llama.cpp.context_length")
            .expect("context definition");
        assert!(definition.parse("0").is_err());
        assert!(definition.parse("many").is_err());
    }
}
