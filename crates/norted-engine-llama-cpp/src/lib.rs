//! llama.cpp-specific launch, probe, health, and inference translation.

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
    AcquisitionMethod, ArtifactFormat, EngineConfig, EngineInstallation, EngineRevision,
    ModelArtifact,
};
use norted_engine::{
    AcquisitionRequest, ApiCapability, CompatibilityDecision, EngineAdapter, EngineCapabilities,
    EngineError, EngineFeature, EngineIdentity, EngineProbe, InferenceEvent, InferenceFinishReason,
    InferenceMessage, InferenceOutput, InferenceRequest, InferenceRole, InferenceStream,
    InferenceUsage, InstallationState, LaunchRequest, LaunchSpec, NativeOption, OptionValueKind,
    ProcessDescriptor, UpdateState, capture_command,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const ENGINE_ID: &str = "llama.cpp";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/ggml-org/llama.cpp";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;

pub struct LlamaCppAdapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
}

impl LlamaCppAdapter {
    pub fn from_config(config: Option<&EngineConfig>, config_directory: &Path) -> Self {
        let mut enabled = false;
        let mut binary_path = None;
        let mut native_arguments = Vec::new();
        let mut environment = BTreeMap::new();
        let mut configuration_error = None;

        if let Some(config) = config {
            enabled = config.enabled;
            environment = config.env.clone();
            if let Some(name) = environment.keys().find(|name| {
                matches!(
                    name.to_ascii_uppercase().as_str(),
                    "LLAMA_ARG_MODEL" | "LLAMA_ARG_HOST" | "LLAMA_ARG_PORT"
                )
            }) {
                configuration_error = Some(format!(
                    "environment variable `{name}` conflicts with Norted-managed model or loopback binding"
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
                    "native argument `{argument}` conflicts with Norted-managed model or loopback binding"
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
        }
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
        for required in ["--model", "--host", "--port"] {
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
                    source_repository: Some(UPSTREAM_REPOSITORY.to_owned()),
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

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
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

    fn native_options(&self) -> Vec<NativeOption> {
        vec![NativeOption {
            name: "arguments".to_owned(),
            description: "Arguments appended exactly as configured after managed bind/model flags"
                .to_owned(),
            value_kind: OptionValueKind::String,
            repeatable: true,
        }]
    }

    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        Ok(self.probe_uncached().await)
    }

    async fn install(
        &self,
        _request: AcquisitionRequest,
    ) -> Result<EngineInstallation, EngineError> {
        Err(EngineError::Unsupported(
            "managed llama.cpp installation is intentionally deferred; configure an existing llama-server binary"
                .to_owned(),
        ))
    }

    async fn update(
        &self,
        _request: AcquisitionRequest,
    ) -> Result<EngineInstallation, EngineError> {
        Err(EngineError::Unsupported(
            "managed llama.cpp updates are intentionally deferred".to_owned(),
        ))
    }

    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "llama.cpp backend address must be loopback".to_owned(),
            ));
        }
        let probe = self.probe().await?;
        let installation = match probe.installation {
            InstallationState::Installed { installation } if probe.healthy => installation,
            InstallationState::NotInstalled => return Err(EngineError::NotInstalled),
            InstallationState::Invalid { reason } => {
                return Err(EngineError::InvalidConfiguration(reason));
            }
            InstallationState::Installed { .. } => {
                return Err(EngineError::Operation(probe.detail));
            }
        };
        let arguments = vec![
            OsString::from("--model"),
            request.model.path.as_os_str().to_owned(),
            OsString::from("--host"),
            OsString::from(request.backend_address.ip().to_string()),
            OsString::from("--port"),
            OsString::from(request.backend_address.port().to_string()),
        ]
        .into_iter()
        .chain(self.native_arguments.iter().map(OsString::from))
        .collect();
        Ok(LaunchSpec {
            executable: installation.binary_path.clone(),
            arguments,
            environment: self.environment.clone(),
            inherits_parent_environment: true,
            working_directory: None,
            endpoint: Some(http_endpoint(request.backend_address)),
            normalized_settings: BTreeMap::new(),
            native_arguments: self.native_arguments.clone(),
            installation: (*installation).clone(),
        })
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
        let finish_reason = map_finish_reason(choice.finish_reason.as_deref());
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
        }
    }
}

struct SseState {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: InferenceFinishReason,
    finished: bool,
}

fn llama_sse_stream(source: BoxStream<'static, Result<Bytes, reqwest::Error>>) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: InferenceFinishReason::Stop,
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
            state.queued.push_back(Ok(InferenceEvent::Completed {
                usage: state.usage.take(),
                finish_reason: state.finish_reason,
            }));
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
            .and_then(Value::as_str)
        {
            state.finish_reason = map_finish_reason(Some(finish_reason));
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

fn conflicts_with_managed_argument(argument: &str) -> bool {
    let argument = argument.to_ascii_lowercase();
    argument == "-m"
        || argument == "--model"
        || argument.starts_with("--model=")
        || argument == "--host"
        || argument.starts_with("--host=")
        || argument == "--port"
        || argument.starts_with("--port=")
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

fn map_finish_reason(reason: Option<&str>) -> InferenceFinishReason {
    if reason == Some("length") {
        InferenceFinishReason::MaxOutputTokens
    } else {
        InferenceFinishReason::Stop
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
