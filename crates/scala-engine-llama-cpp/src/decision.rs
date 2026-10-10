//! Official llama-server System One contract. Proofs live only for a loaded process.
use super::*;
use scala_core::{ModelProfileId, ResolvedSettings};
use scala_engine::{DecisionOutput, DecisionQuestion, DecisionRequest};

const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
const STATE_REQUIRED: &str = "\"state\" must be provided";

/// Native GGUF metadata admits a candidate for JIT loading, never Decision itself.
/// No architecture table: upstream owns recognition of the concrete decision type.
pub(super) fn candidate(model: &ModelArtifact) -> bool {
    model.format == ArtifactFormat::Gguf
        && matches!(&model.native_identity, Some(ArtifactNativeIdentity::Gguf(identity))
            if identity.decision_type.as_deref().is_some_and(|kind| !kind.is_empty() && kind != "none"))
}

fn tuple_key(
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    settings: &ResolvedSettings,
) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                LlamaCppAdapter::capability_key(runtime),
                &runtime.manifest.identity,
                &model.id,
                &model.path,
                &model.hash,
                model.size_bytes,
                &model.native_identity,
                &settings.engine_id,
                &settings.model_profile_id,
                &settings.configured,
            ))
            .expect("serializable decision tuple")
        )
    )
}

pub(super) struct LaunchProof {
    key: String,
    runtime_id: RuntimeId,
    executable_sha256: String,
    model_id: scala_core::ModelId,
    model_path: PathBuf,
    profile_id: Option<ModelProfileId>,
    candidate: bool,
    executable_verified: bool,
    ready: bool,
    build_info: Option<String>,
    model_metadata: Option<Value>,
}

async fn body(response: reqwest::Response) -> Result<(reqwest::StatusCode, Vec<u8>), EngineError> {
    let status = response.status();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_transport_error)?;
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err(EngineError::Operation(
                "llama.cpp System One response exceeded local size limit".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes))
}

/// Translate only the private native wire body; public requests stay unchanged.
fn native_request_body(request: &DecisionRequest) -> Result<Value, EngineError> {
    request
        .validate()
        .map_err(EngineError::InvalidDecisionRequest)?;
    if !request.images.is_empty() {
        return Err(EngineError::Unsupported("native decision images".into()));
    }
    for (name, question) in &request.questions {
        if let DecisionQuestion::Score { criteria, .. } = question
            && !(2..=10).contains(&criteria.len())
        {
            return Err(EngineError::InvalidDecisionRequest(format!(
                "llama.cpp score question `{name}` requires criteria with 2 to 10 levels (received {})",
                criteria.len()
            )));
        }
    }
    let mut questions = serde_json::to_value(&request.questions)
        .map_err(|error| EngineError::Operation(error.to_string()))?;
    for question in questions
        .as_object_mut()
        .expect("typed question map")
        .values_mut()
    {
        question
            .as_object_mut()
            .expect("typed question")
            .entry("instructions")
            .or_insert_with(|| json!(""));
    }
    Ok(json!({"state": request.state, "questions": questions}))
}

impl LlamaCppAdapter {
    pub(super) async fn prepare_decision(&self, spec: &LaunchSpec) {
        let Some(endpoint) = &spec.endpoint else {
            return;
        };
        let key = tuple_key(&spec.runtime, &spec.model.primary, &spec.settings);
        // Invalidate before every attempt, even one whose verification fails.
        self.decision_proofs
            .write()
            .expect("decision proof lock")
            .retain(|address, proof| address != endpoint && proof.key != key);
        let candidate = candidate(&spec.model.primary);
        let executable_verified = candidate
            && hash_file(&spec.executable).await.ok().as_deref()
                == Some(spec.runtime.manifest.entrypoint_sha256.as_str());
        self.decision_proofs
            .write()
            .expect("decision proof lock")
            .insert(
                endpoint.clone(),
                LaunchProof {
                    key,
                    runtime_id: spec.runtime.manifest.runtime_id.clone(),
                    executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
                    model_id: spec.model.primary.id.clone(),
                    model_path: spec.model.primary.path.clone(),
                    profile_id: spec.settings.model_profile_id.clone(),
                    candidate,
                    executable_verified,
                    ready: false,
                    build_info: None,
                    model_metadata: None,
                },
            );
    }

    pub(super) fn decision_candidate_endpoint(&self, endpoint: &str) -> bool {
        self.decision_proofs
            .read()
            .expect("decision proof lock")
            .get(endpoint)
            .is_some_and(|proof| proof.candidate)
    }

    pub(super) async fn observe_decision(&self, process: &ProcessDescriptor) {
        let Some(endpoint) = &process.endpoint else {
            return;
        };
        let expected = self
            .decision_proofs
            .read()
            .expect("decision proof lock")
            .get(endpoint)
            .filter(|proof| {
                proof.candidate
                    && proof.executable_verified
                    && proof.runtime_id == process.runtime_id
                    && proof.executable_sha256 == process.runtime_executable_sha256
                    && proof.model_id == process.model_id
            })
            .map(|proof| {
                (
                    proof.key.clone(),
                    proof.model_path.clone(),
                    proof.profile_id.clone(),
                )
            });
        let Some((key, model_path, Some(profile_id))) = expected else {
            return;
        };
        let result = async {
            let (status, bytes) = body(
                self.client
                    .get(format!("{endpoint}/props"))
                    .timeout(PROPS_TIMEOUT)
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            if !status.is_success() {
                return None;
            }
            let props: Value = serde_json::from_slice(&bytes).ok()?;
            let build_info = props.get("build_info")?.as_str()?.to_owned();
            if build_info.is_empty()
                || props.get("model_path")?.as_str()? != model_path.to_str()?
                || props.get("model_alias")?.as_str()? != profile_id.as_str()
            {
                return None;
            }
            let (status, bytes) = body(
                self.client
                    .get(format!("{endpoint}/v1/models"))
                    .timeout(PROPS_TIMEOUT)
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            if !status.is_success() {
                return None;
            }
            let models: Value = serde_json::from_slice(&bytes).ok()?;
            let entries = models.get("data")?.as_array()?;
            if entries.len() != 1 {
                return None;
            }
            let model = &entries[0];
            if model.get("id")?.as_str()? != profile_id.as_str() || !model.get("meta")?.is_object()
            {
                return None;
            }
            // Current upstream computes this from native decision.type metadata.
            // Earlier System One nightlies omit architecture; the native validator
            // below proves their classifier, without duplicating model-name tables.
            if let Some(architecture) = model.get("architecture") {
                let outputs = architecture.get("output_modalities")?.as_array()?;
                if !outputs
                    .iter()
                    .any(|value| value.as_str() == Some("decisions"))
                {
                    return None;
                }
            }
            // An empty request exits parse_questions before any task or inference.
            // Unknown route: 404; ordinary model: 501; native Decision: typed 400.
            let (status, bytes) = body(
                self.client
                    .post(format!("{endpoint}/v1/systemone"))
                    .timeout(PROPS_TIMEOUT)
                    .json(&json!({}))
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            let error: Value = serde_json::from_slice(&bytes).ok()?;
            if status != reqwest::StatusCode::BAD_REQUEST
                || error.pointer("/error/type")?.as_str()? != "invalid_request_error"
                || error.pointer("/error/message")?.as_str()? != STATE_REQUIRED
            {
                return None;
            }
            Some((build_info, model.clone()))
        }
        .await;
        let mut proofs = self.decision_proofs.write().expect("decision proof lock");
        if let Some(proof) = proofs.get_mut(endpoint).filter(|proof| {
            proof.key == key
                && proof.runtime_id == process.runtime_id
                && proof.executable_sha256 == process.runtime_executable_sha256
                && proof.model_id == process.model_id
        }) {
            proof.ready = result.is_some();
            if let Some((build_info, metadata)) = result {
                proof.build_info = Some(build_info);
                proof.model_metadata = Some(metadata);
            }
        }
    }

    pub(super) fn decision_supported(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: &ResolvedSettings,
    ) -> bool {
        if !candidate(model)
            || settings.engine_id != ENGINE_ID
            || runtime.manifest.identity.engine_id != ENGINE_ID
            || runtime.manifest.runtime_id != RuntimeId::from_identity(&runtime.manifest.identity)
            || !runtime.manifest.probe.compatible
            || runtime.manifest.probe.observed_engine_id != ENGINE_ID
        {
            return false;
        }
        let key = tuple_key(runtime, model, settings);
        self.decision_proofs
            .read()
            .expect("decision proof lock")
            .values()
            .any(|proof| proof.key == key && proof.executable_verified && proof.ready)
    }

    pub(super) fn decision_observation(&self, endpoint: &str) -> Value {
        self.decision_proofs
            .read()
            .expect("decision proof lock")
            .get(endpoint)
            .map_or(Value::Null, |proof| {
                json!({
                    "executable_verified": proof.executable_verified,
                    "native_systemone_verified": proof.ready,
                    "build_info": proof.build_info,
                    "model_metadata": proof.model_metadata,
                })
            })
    }

    pub(super) async fn native_decide(
        &self,
        endpoint: &str,
        request: DecisionRequest,
    ) -> Result<DecisionOutput, EngineError> {
        if !self
            .decision_proofs
            .read()
            .expect("decision proof lock")
            .get(endpoint)
            .is_some_and(|proof| {
                proof.executable_verified
                    && proof.ready
                    && proof.profile_id.as_ref() == Some(&request.model_profile_id)
            })
        {
            return Err(EngineError::Unsupported(
                "llama.cpp loaded runtime/model/settings have not proved native System One support"
                    .into(),
            ));
        }
        // The private server has already loaded the model. Never send a selector.
        let request_body = native_request_body(&request)?;
        let (status, bytes) = body(
            self.client
                .post(format!("{endpoint}/v1/systemone"))
                .json(&request_body)
                .send()
                .await
                .map_err(map_transport_error)?,
        )
        .await?;
        if !status.is_success() {
            return Err(backend_http_error(status, &bytes));
        }
        let wire: WireOutput = serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::Operation(format!("invalid llama.cpp System One response: {error}"))
        })?;
        let usage = wire
            .usage
            .map(|usage| {
                let total_tokens = usage
                    .input_tokens
                    .checked_add(usage.output_tokens)
                    .ok_or_else(|| {
                        EngineError::Operation("llama.cpp System One token usage overflow".into())
                    })?;
                Ok(InferenceUsage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens,
                    ..Default::default()
                })
            })
            .transpose()?;
        Ok(DecisionOutput {
            model: wire.model,
            answers: wire.answers,
            usage,
        })
    }
}

#[derive(Deserialize)]
struct WireOutput {
    model: Option<String>,
    answers: BTreeMap<String, scala_engine::DecisionAnswer>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Request, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use scala_core::ModelId;
    use scala_engine::{DecisionAnswer, DecisionContent, native_decision_supported};
    use std::sync::{Arc, Mutex};

    struct Backend {
        calls: Mutex<Vec<(String, String, Value)>>,
        props: Value,
        models: Value,
        native: bool,
        probe_response: Mutex<(StatusCode, Value)>,
        response: Mutex<(StatusCode, Value)>,
    }

    async fn handle(State(backend): State<Arc<Backend>>, request: Request) -> impl IntoResponse {
        let path = request.uri().path().to_owned();
        let method = request.method().to_string();
        let bytes = axum::body::to_bytes(request.into_body(), RESPONSE_LIMIT)
            .await
            .unwrap();
        let payload = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        backend
            .calls
            .lock()
            .unwrap()
            .push((method, path.clone(), payload.clone()));
        let (status, value) = match path.as_str() {
            "/props" => (StatusCode::OK, backend.props.clone()),
            "/v1/models" | "/models" => (StatusCode::OK, backend.models.clone()),
            "/v1/systemone" if !backend.native => (
                StatusCode::NOT_FOUND,
                json!({"error":{"message":"not found"}}),
            ),
            "/v1/systemone" if payload == json!({}) => {
                backend.probe_response.lock().unwrap().clone()
            }
            "/v1/systemone"
                if payload["questions"].as_object().is_some_and(|questions| {
                    questions
                        .values()
                        .any(|question| question["instructions"].is_null())
                }) =>
            {
                (
                    StatusCode::BAD_REQUEST,
                    json!({"error":{"code":400,"type":"invalid_request_error","message":"questions.safe: \"instructions\" must be provided"}}),
                )
            }
            "/v1/systemone" => backend.response.lock().unwrap().clone(),
            _ => (
                StatusCode::NOT_FOUND,
                json!({"error":{"message":"unexpected generation route"}}),
            ),
        };
        (status, Json(value))
    }

    struct Fixture {
        adapter: LlamaCppAdapter,
        spec: LaunchSpec,
        process: ProcessDescriptor,
        backend: Arc<Backend>,
        server: tokio::task::JoinHandle<()>,
        _directory: tempfile::TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    // Header/metadata only; no tensors, real executable, runtime installation or inference.
    fn gguf(path: &Path, decision_type: Option<&str>, other_architecture: bool) {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3_u32.to_le_bytes());
        bytes.extend(0_u64.to_le_bytes());
        bytes.extend((1_u64 + u64::from(decision_type.is_some())).to_le_bytes());
        for (key, value) in
            std::iter::once(("general.architecture", "bert")).chain(decision_type.map(|value| {
                (
                    if other_architecture {
                        "llama.decision.type"
                    } else {
                        "bert.decision.type"
                    },
                    value,
                )
            }))
        {
            bytes.extend((key.len() as u64).to_le_bytes());
            bytes.extend(key.as_bytes());
            bytes.extend(8_u32.to_le_bytes());
            bytes.extend((value.len() as u64).to_le_bytes());
            bytes.extend(value.as_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    async fn fixture(native: bool, kind: Option<&str>, outputs: Option<Value>) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Laya.gguf");
        gguf(&path, kind, false);
        let model = ModelArtifact {
            id: ModelId("synthetic".into()),
            display_name: "Laya".into(),
            path: path.clone(),
            format: ArtifactFormat::Gguf,
            size_bytes: std::fs::metadata(&path).unwrap().len(),
            created: 1,
            hash: Some("c".repeat(64)),
            architecture: Some("bert".into()),
            context_length: None,
            provenance: None,
            native_identity: Some(ArtifactNativeIdentity::Gguf(
                scala_core::inspect_gguf_metadata(&path).unwrap(),
            )),
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        };
        let executable = directory.path().join("llama-server");
        std::fs::write(&executable, b"synthetic executable identity; never run").unwrap();
        let sha = hash_file(&executable).await.unwrap();
        let identity: RuntimeIdentity = serde_json::from_value(json!({
            "engine_id":ENGINE_ID,"package_family":"llama-cpp","version":if native {"b11425"} else {"b10000"},
            "upstream_revision":null,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "accelerator":"cpu","variant":"cpu", "package":{"provider_id":LLAMA_CPP_RUNTIME_PROVIDER_ID,
                "repository":"ggml-org/llama.cpp","release_tag":if native {"b11425"} else {"b10000"},"asset_id":"1","asset_name":"fixture.zip"}
        })).unwrap();
        let runtime_id = RuntimeId::from_identity(&identity);
        let runtime: InstalledRuntime = serde_json::from_value(json!({
            "manifest":{"schema_version":scala_core::RUNTIME_MANIFEST_SCHEMA_VERSION,"runtime_id":runtime_id,
                "identity":identity,"supported_formats":["gguf"],"requirements":scala_core::RuntimeRequirements::default(),
                "acquisition_method":"official_release_asset","source_url":"https://synthetic.invalid/fixture.zip","downloaded_archive_sha256":"b".repeat(64),
                "entrypoint":"llama-server","entrypoint_sha256":sha,"installed_at_unix":1,
                "probe":{"compatible":true,"observed_engine_id":ENGINE_ID,"observed_version":null,"observed_revision":null,"detail":"synthetic","observed_at_unix":1}},
            "installation_root":directory.path()
        })).unwrap();
        let settings = ResolvedSettings {
            engine_id: ENGINE_ID.into(),
            model_profile_id: Some(ModelProfileId::new("decision-profile").unwrap()),
            ..Default::default()
        };
        let revision = EngineRevision {
            engine_id: ENGINE_ID.into(),
            version: Some(identity.version.clone()),
            revision: None,
        };
        let installation: EngineInstallation = serde_json::from_value(json!({
            "engine":revision,"source_repository":UPSTREAM_REPOSITORY,"acquisition_method":{"method":"official_binary"},
            "binary_path":executable,"binary_sha256":sha,"build":null,"platform":std::env::consts::OS,
            "architecture":std::env::consts::ARCH,"runtime_variant":"cpu","acquired_at_unix":1,"observed_at_unix":1
        })).unwrap();
        let mut listed = json!({"id":"decision-profile","object":"model","owned_by":"llamacpp","meta":{"n_ctx":2048}});
        if let Some(outputs) = outputs {
            listed["architecture"] =
                json!({"input_modalities":["text"],"output_modalities":outputs});
        }
        let backend = Arc::new(Backend {
            calls: Mutex::new(Vec::new()),
            native,
            probe_response: Mutex::new((
                StatusCode::BAD_REQUEST,
                json!({"error":{"code":400,"type":"invalid_request_error","message":STATE_REQUIRED}}),
            )),
            props: json!({"model_path":path,"model_alias":"decision-profile","build_info":"b11425-e117148",
                "default_generation_settings":{"n_ctx":2048,"params":{"temperature":0.8,"top_p":0.9}},"total_slots":1}),
            models: json!({"object":"list","data":[listed]}),
            response: Mutex::new((
                StatusCode::OK,
                json!({
                "model":"native-model", "answers":{
                    "route":{"type":"choice","choice":"accept","probabilities":{"accept":0.75,"reject":0.25},"confidence":0.6},
                    "urgency":{"type":"score","score":0.4,"legend":{"0":"low","1":{"level":"high"}},"probabilities":{"0":0.6,"1":0.4},"confidence":0.2},
                    "safe":{"type":"noul","noul":0.8}},"usage":{"input_tokens":42,"output_tokens":0}}),
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().fallback(handle).with_state(backend.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let process = ProcessDescriptor {
            supervisor_id: "synthetic".into(),
            process_id: 0,
            engine: revision,
            runtime_id: runtime_id.clone(),
            runtime_version: identity.version.clone(),
            runtime_variant: "cpu".into(),
            runtime_executable_sha256: sha,
            model_id: model.id.clone(),
            endpoint: Some(endpoint.clone()),
            launched_at_unix: 1,
        };
        let spec = LaunchSpec {
            executable,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            environment_remove: Vec::new(),
            inherits_parent_environment: false,
            supervise_process_tree: false,
            working_directory: None,
            temporary_files: Vec::new(),
            endpoint: Some(endpoint),
            normalized_settings: BTreeMap::new(),
            settings,
            native_arguments: Vec::new(),
            installation,
            runtime,
            model: PreparedModelInput {
                primary: model,
                auxiliary: Vec::new(),
                primary_file_identity: None,
            },
            accelerator_binding: None,
        };
        Fixture {
            adapter: LlamaCppAdapter::from_config(None, Path::new(".")),
            spec,
            process,
            backend,
            server,
            _directory: directory,
        }
    }

    impl Fixture {
        async fn qualify(&self) {
            self.adapter.prepare_decision(&self.spec).await;
            self.adapter
                .startup_observation(&self.process, &[])
                .await
                .unwrap();
        }
        fn supported(&self) -> bool {
            native_decision_supported(
                &self.adapter,
                &self.spec.runtime,
                &self.spec.model.primary,
                &self.spec.settings,
            )
        }
        fn request(&self) -> DecisionRequest {
            DecisionRequest { model_profile_id:self.spec.settings.model_profile_id.clone().unwrap(),
                images: Vec::new(),
                state:serde_json::from_value(json!({"message":"synthetic","nested":[1,2]})).unwrap(),
                questions:serde_json::from_value(json!({
                    "route":{"type":"choice","instructions":{"question":"route?"},"criteria":{"accept":"allowed","reject":null}},
                    "urgency":{"type":"score","instructions":["urgency?"],"criteria":["low",{"level":"high"}]},
                    "safe":{"type":"noul","instructions":"safe?","criteria":{"true":"safe","false":"unsafe"}}
                })).unwrap() }
        }
    }

    // Substitute only process mechanics and runtime probes. Real manager routing,
    // llama qualification, HTTP mapping and public discovery are exercised.
    struct ManagedAdapter {
        native: LlamaCppAdapter,
        template: LaunchSpec,
    }
    #[async_trait]
    impl EngineAdapter for ManagedAdapter {
        fn identity(&self) -> EngineIdentity {
            self.native.identity()
        }
        fn capabilities(&self) -> EngineCapabilities {
            self.native.capabilities()
        }
        fn native_options(&self) -> Vec<NativeOption> {
            Vec::new()
        }
        fn supports_model_capability(
            &self,
            model: &ModelArtifact,
            capability: ApiCapability,
        ) -> bool {
            self.native.supports_model_capability(model, capability)
        }
        fn supports_native_decision_candidate(
            &self,
            runtime: &InstalledRuntime,
            model: &ModelArtifact,
            settings: &ResolvedSettings,
        ) -> bool {
            self.native
                .supports_native_decision_candidate(runtime, model, settings)
        }
        fn supports_native_decision(
            &self,
            runtime: &InstalledRuntime,
            model: &ModelArtifact,
            settings: &ResolvedSettings,
        ) -> bool {
            self.native
                .supports_native_decision(runtime, model, settings)
        }
        fn serving_features(
            &self,
            runtime: &InstalledRuntime,
            model: &ModelArtifact,
            settings: Option<&ResolvedSettings>,
        ) -> Vec<EngineFeature> {
            self.native.serving_features(runtime, model, settings)
        }
        async fn probe(&self) -> Result<EngineProbe, EngineError> {
            Ok(EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: true,
                detail: "synthetic".into(),
            })
        }
        async fn probe_runtime(
            &self,
            runtime: &InstalledRuntime,
        ) -> Result<RuntimeProbeObservation, EngineError> {
            Ok(runtime.manifest.probe.clone())
        }
        async fn build_launch_spec(
            &self,
            request: LaunchRequest,
        ) -> Result<LaunchSpec, EngineError> {
            Ok(LaunchSpec {
                model: request.model,
                runtime: request.runtime,
                settings: request.settings,
                ..self.template.clone()
            })
        }
        async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
            self.native.prepare_decision(spec).await;
            Ok(())
        }
        async fn clear_launch_state(&self, endpoint: Option<&str>) {
            self.native.clear_launch_state(endpoint).await;
        }
        async fn health(&self, _: &ProcessDescriptor) -> Result<bool, EngineError> {
            Ok(true)
        }
        async fn startup_observation(
            &self,
            process: &ProcessDescriptor,
            _: &[String],
        ) -> Result<StartupObservation, EngineError> {
            self.native.observe_decision(process).await;
            Ok(StartupObservation::Ready(BTreeMap::new()))
        }
        async fn effective_generation_settings(
            &self,
            _: &ProcessDescriptor,
        ) -> Result<EffectiveGenerationSettings, EngineError> {
            Ok(EffectiveGenerationSettings {
                temperature: 0.0,
                top_p: 1.0,
            })
        }
        async fn decide(
            &self,
            endpoint: &str,
            request: DecisionRequest,
        ) -> Result<DecisionOutput, EngineError> {
            self.native.decide(endpoint, request).await
        }
        async fn infer(
            &self,
            _: &str,
            _: InferenceRequest,
        ) -> Result<InferenceOutput, EngineError> {
            panic!("Decision must never use generation")
        }
        async fn infer_stream(
            &self,
            _: &str,
            _: InferenceRequest,
            _: InferenceActivityReporter,
        ) -> Result<InferenceStream, EngineError> {
            panic!("Decision must never use streaming generation")
        }
    }

    struct SyntheticSupervisor {
        launches: std::sync::atomic::AtomicUsize,
        exit: tokio::sync::watch::Sender<Option<scala_engine::ProcessExit>>,
    }
    #[async_trait]
    impl scala_engine::ProcessSupervisor for SyntheticSupervisor {
        async fn spawn(
            &self,
            spec: LaunchSpec,
            engine: EngineRevision,
            model_id: ModelId,
        ) -> Result<ProcessDescriptor, EngineError> {
            self.launches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ProcessDescriptor {
                supervisor_id: "synthetic".into(),
                process_id: 0,
                engine,
                runtime_id: spec.runtime.manifest.runtime_id.clone(),
                runtime_version: spec.runtime.manifest.identity.version.clone(),
                runtime_variant: spec.runtime.manifest.identity.variant.clone(),
                runtime_executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
                model_id,
                endpoint: spec.endpoint,
                launched_at_unix: 1,
            })
        }
        async fn terminate(&self, _: &ProcessDescriptor) -> Result<(), EngineError> {
            Ok(())
        }
        async fn subscribe(
            &self,
            _: &ProcessDescriptor,
        ) -> Result<tokio::sync::watch::Receiver<Option<scala_engine::ProcessExit>>, EngineError>
        {
            Ok(self.exit.subscribe())
        }
        async fn stderr_tail(&self, _: &ProcessDescriptor) -> Result<Vec<String>, EngineError> {
            Ok(Vec::new())
        }
        async fn shutdown(&self) {}
    }

    async fn public_request(
        endpoint: &str,
        method: &str,
        path: &str,
        payload: Value,
    ) -> (StatusCode, Value) {
        let response = reqwest::Client::new()
            .request(method.parse().unwrap(), format!("{endpoint}{path}"))
            .json(&payload)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    #[tokio::test]
    async fn public_decision_candidate_cold_jit_unload_and_fresh_manager_lifecycle() {
        use scala_core::{AppPaths, ApplicationCore, EngineId, ModelProfilesStore};
        use scala_engine::{
            EngineRegistry, RuntimeManager, RuntimeManagerOptions, RuntimePackManager, RuntimeStore,
        };
        use std::sync::atomic::Ordering;
        for (native, kind, valid_proof) in [
            (true, Some("laya"), true),
            (false, Some("laya"), true),
            (true, None, true),
            (true, Some("laya"), false),
        ] {
            let fixture = fixture(native, kind, None).await;
            if !valid_proof {
                *fixture.backend.probe_response.lock().unwrap() = (
                    StatusCode::BAD_REQUEST,
                    json!({"error":{"type":"invalid_request_error","message":"unqualified candidate"}}),
                );
            }
            let root = fixture._directory.path().join("app");
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
            paths.ensure_required().unwrap();
            std::fs::write(
                &paths.config_file,
                format!(
                    "version = 1\n[server]\nhost = \"127.0.0.1\"\nport = 0\n[models]\npaths = [{}]\n",
                    serde_json::to_string(fixture._directory.path()).unwrap()
                ),
            )
            .unwrap();
            let core = ApplicationCore::load_from_paths(paths.clone())
                .await
                .unwrap();
            core.refresh_models().await.unwrap();
            let model = core.snapshot().await.models.into_iter().next().unwrap();
            let profile_id = fixture.spec.settings.model_profile_id.clone().unwrap();
            ModelProfilesStore::new(&paths)
                .update({
                    let profile_id = profile_id.clone();
                    move |state| {
                        state.create(
                            profile_id,
                            "Synthetic",
                            model.id,
                            EngineId::new(ENGINE_ID).unwrap(),
                        )?;
                        Ok(())
                    }
                })
                .await
                .unwrap();
            // A fake installed manifest and inert bytes; no runtime is installed or run.
            let store = RuntimeStore::new(&paths);
            let installation = store.installation_path(&fixture.spec.runtime.manifest);
            std::fs::create_dir_all(&installation).unwrap();
            std::fs::copy(&fixture.spec.executable, installation.join("llama-server")).unwrap();
            std::fs::write(
                installation.join("runtime.json"),
                serde_json::to_vec(&fixture.spec.runtime.manifest).unwrap(),
            )
            .unwrap();
            let supervisor = Arc::new(SyntheticSupervisor {
                launches: Default::default(),
                exit: tokio::sync::watch::channel(None).0,
            });
            // Restart means new manager AND new adapter, reading the same stored profile.
            for restart in 0..2 {
                let adapter = Arc::new(ManagedAdapter {
                    native: LlamaCppAdapter::from_config(None, Path::new(".")),
                    template: fixture.spec.clone(),
                });
                let mut registry = EngineRegistry::default();
                registry.register(adapter.clone()).unwrap();
                let packs = RuntimePackManager::new(&paths, registry.clone(), Vec::new()).unwrap();
                let manager = RuntimeManager::initialize(
                    core.clone(),
                    registry,
                    packs.clone(),
                    supervisor.clone(),
                    RuntimeManagerOptions::default(),
                )
                .await;
                let api = scala_api::ApiServer::bind(
                    core.clone(),
                    manager.clone(),
                    scala_api::PublicAuth::disabled(),
                )
                .await
                .unwrap();
                let endpoint = format!("http://{}", api.local_addr());
                let (stop, stopped) = tokio::sync::oneshot::channel();
                let server = tokio::spawn(api.run(async {
                    let _ = stopped.await;
                }));
                for unload in 0..2 {
                    let (_, listed) =
                        public_request(&endpoint, "GET", "/v1/models", Value::Null).await;
                    let capabilities = &listed["data"][0]["capabilities"];
                    assert_ne!(capabilities["decision"], true);
                    assert_eq!(
                        capabilities["decision_candidate"] == true,
                        kind.is_some(),
                        "{listed}"
                    );
                    assert!(manager.status().await.backends.is_empty());
                    assert!(adapter.native.decision_proofs.read().unwrap().is_empty());
                    let before = supervisor.launches.load(Ordering::SeqCst);
                    let request = fixture.request();
                    let (status, result) = public_request(&endpoint, "POST", "/v1/systemone", json!({"model":profile_id,"state":request.state,"questions":request.questions})).await;
                    if native && kind.is_some() && valid_proof {
                        assert_eq!(
                            status,
                            StatusCode::OK,
                            "restart {restart}, unload {unload}: {result}"
                        );
                        assert_eq!(supervisor.launches.load(Ordering::SeqCst), before + 1);
                        assert_eq!(
                            result["answers"],
                            fixture.backend.response.lock().unwrap().1["answers"]
                        );
                        let (_, listed) =
                            public_request(&endpoint, "GET", "/v1/models", Value::Null).await;
                        assert_eq!(listed["data"][0]["capabilities"]["decision"], true);
                        // Adapter validation reaches the public 400 envelope without HTTP.
                        let native_calls = fixture.backend.calls.lock().unwrap().len();
                        for levels in [1, 11] {
                            let (status, error) = public_request(&endpoint, "POST", "/v1/systemone", json!({
                                "model":profile_id,"state":{},"questions":{"score":{"type":"score","criteria":vec!["level"; levels]}}
                            })).await;
                            assert_eq!(status, StatusCode::BAD_REQUEST);
                            assert_eq!(error["error"]["code"], "invalid_value");
                            assert_eq!(error["error"]["param"], "questions");
                            assert!(
                                error["error"]["message"]
                                    .as_str()
                                    .unwrap()
                                    .contains("2 to 10 levels")
                            );
                        }
                        assert_eq!(fixture.backend.calls.lock().unwrap().len(), native_calls);
                        manager.unload(profile_id.clone()).await.unwrap();
                        assert!(adapter.native.decision_proofs.read().unwrap().is_empty());
                    } else {
                        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
                        assert_eq!(result["error"]["code"], "unsupported_capability");
                        assert_eq!(
                            supervisor.launches.load(Ordering::SeqCst),
                            before + usize::from(kind.is_some())
                        );
                        if kind.is_some() {
                            manager.unload(profile_id.clone()).await.unwrap();
                        }
                    }
                }
                stop.send(()).unwrap();
                server.await.unwrap().unwrap();
            }
            assert!(fixture.backend.calls.lock().unwrap().iter().all(
                |(_, path, payload)| matches!(
                    path.as_str(),
                    "/props" | "/v1/models" | "/v1/systemone"
                ) && (native
                    || path != "/v1/systemone"
                    || payload == &json!({}))
            ));
        }
    }

    #[tokio::test]
    async fn old_runtime_and_generous_version_strings_do_not_prove_decision() {
        let mut fixture = fixture(false, Some("laya"), Some(json!(["decisions"]))).await;
        fixture.spec.runtime.manifest.identity.version = "v0.6.0".into();
        fixture.spec.runtime.manifest.runtime_id =
            RuntimeId::from_identity(&fixture.spec.runtime.manifest.identity);
        fixture.process.runtime_id = fixture.spec.runtime.manifest.runtime_id.clone();
        fixture.qualify().await;
        assert!(!fixture.supported());
        assert!(
            fixture
                .adapter
                .serving_features(
                    &fixture.spec.runtime,
                    &fixture.spec.model.primary,
                    Some(&fixture.spec.settings)
                )
                .is_empty()
        );
        assert!(matches!(
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), fixture.request())
                .await,
            Err(EngineError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn normal_gguf_and_laya_filename_grant_nothing_and_keep_generation_behavior() {
        let fixture = fixture(true, None, Some(json!(["text"]))).await;
        // Do not call the existing generative chat probe in synthetic validation.
        fixture.adapter.prepare_decision(&fixture.spec).await;
        fixture.adapter.observe_decision(&fixture.process).await;
        assert!(!fixture.supported());
        assert!(
            !fixture
                .adapter
                .supports_model_capability(&fixture.spec.model.primary, ApiCapability::Decision)
        );
        for capability in [
            ApiCapability::ChatCompletions,
            ApiCapability::Completions,
            ApiCapability::Responses,
        ] {
            assert!(
                fixture
                    .adapter
                    .supports_model_capability(&fixture.spec.model.primary, capability)
            );
        }
        assert_eq!(
            fixture.adapter.serving_features(
                &fixture.spec.runtime,
                &fixture.spec.model.primary,
                Some(&fixture.spec.settings)
            ),
            vec![EngineFeature::TextGeneration]
        );
        assert!(fixture.backend.calls.lock().unwrap().is_empty());
        let mut model = fixture.spec.model.primary.clone();
        if let Some(ArtifactNativeIdentity::Gguf(identity)) = &mut model.native_identity {
            identity.pooling_type = Some(1);
        }
        assert!(
            fixture
                .adapter
                .supports_model_capability(&model, ApiCapability::Embeddings)
        );
        assert!(
            !fixture
                .adapter
                .supports_model_capability(&model, ApiCapability::ChatCompletions)
        );
        assert!(
            !fixture
                .adapter
                .supports_model_capability(&model, ApiCapability::Decision)
        );
        assert!(
            fixture
                .adapter
                .serving_features(&fixture.spec.runtime, &model, Some(&fixture.spec.settings))
                .is_empty()
        );
        model.native_identity = None;
        assert!(!candidate(&model));
    }

    #[tokio::test]
    async fn native_metadata_and_validator_qualify_only_the_exact_pair() {
        let fixture = fixture(
            true,
            Some("laya"),
            Some(json!(["decisions", "future-modality"])),
        )
        .await;
        assert!(!fixture.supported());
        fixture.qualify().await;
        assert!(fixture.supported());
        assert!(
            fixture
                .backend
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, path, _)| matches!(
                    path.as_str(),
                    "/props" | "/v1/models" | "/v1/systemone"
                ))
        );
        assert_eq!(
            fixture.adapter.serving_features(
                &fixture.spec.runtime,
                &fixture.spec.model.primary,
                Some(&fixture.spec.settings)
            ),
            vec![EngineFeature::Decision]
        );
        for capability in [
            ApiCapability::Embeddings,
            ApiCapability::ChatCompletions,
            ApiCapability::Completions,
            ApiCapability::Responses,
        ] {
            assert!(
                !fixture
                    .adapter
                    .supports_model_capability(&fixture.spec.model.primary, capability)
            );
        }
        assert!(
            fixture
                .adapter
                .serving_features(&fixture.spec.runtime, &fixture.spec.model.primary, None)
                .is_empty()
        );
        let mut settings = fixture.spec.settings.clone();
        settings.model_profile_id = Some(ModelProfileId::new("other").unwrap());
        assert!(!native_decision_supported(
            &fixture.adapter,
            &fixture.spec.runtime,
            &fixture.spec.model.primary,
            &settings
        ));
        settings = fixture.spec.settings.clone();
        settings.configured.insert(
            scala_core::SettingId::new("llama.cpp.context_length").unwrap(),
            scala_core::ResolvedSetting {
                value: scala_core::SettingValue::UnsignedInteger(4096),
                source: scala_core::SettingSource::Invocation,
            },
        );
        assert!(!native_decision_supported(
            &fixture.adapter,
            &fixture.spec.runtime,
            &fixture.spec.model.primary,
            &settings
        ));
        let mut model = fixture.spec.model.primary.clone();
        model.hash = Some("d".repeat(64));
        assert!(!native_decision_supported(
            &fixture.adapter,
            &fixture.spec.runtime,
            &model,
            &fixture.spec.settings
        ));
        let mut runtime = fixture.spec.runtime.clone();
        runtime.manifest.entrypoint_sha256 = "e".repeat(64);
        assert!(!native_decision_supported(
            &fixture.adapter,
            &runtime,
            &fixture.spec.model.primary,
            &fixture.spec.settings
        ));
        fixture
            .adapter
            .clear_launch_state(fixture.spec.endpoint.as_deref())
            .await;
        assert!(!fixture.supported());
    }

    #[tokio::test]
    async fn native_validation_qualifies_pre_modalities_nightlies_and_rejects_contradictions() {
        let fixture = fixture(true, Some("laya"), None).await;
        fixture.qualify().await;
        assert!(fixture.supported());
        let fixture = self::fixture(true, Some("laya"), Some(json!(["text"]))).await;
        fixture.qualify().await;
        assert!(!fixture.supported());
    }

    #[tokio::test]
    async fn unverified_executables_processes_paths_and_generic_errors_do_not_qualify() {
        let fixture = fixture(true, Some("laya"), None).await;
        *fixture.backend.probe_response.lock().unwrap() = (
            StatusCode::BAD_REQUEST,
            json!({"error":{"type":"invalid_request_error","message":"unknown route"}}),
        );
        fixture.qualify().await;
        assert!(!fixture.supported());
        let mut fixture = self::fixture(true, Some("laya"), None).await;
        fixture.spec.model.primary.path = PathBuf::from("other-model.gguf");
        fixture.qualify().await;
        assert!(!fixture.supported());
        let fixture = self::fixture(true, Some("laya"), None).await;
        fixture.adapter.prepare_decision(&fixture.spec).await;
        let mut process = fixture.process.clone();
        process.runtime_executable_sha256 = "f".repeat(64);
        fixture.adapter.observe_decision(&process).await;
        assert!(!fixture.supported());
        assert!(fixture.backend.calls.lock().unwrap().is_empty());
        std::fs::write(&fixture.spec.executable, b"different executable").unwrap();
        fixture.adapter.prepare_decision(&fixture.spec).await;
        fixture.adapter.observe_decision(&fixture.process).await;
        assert!(!fixture.supported());
        assert!(fixture.backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn omitted_instructions_are_empty_only_on_the_native_wire() {
        let fixture = fixture(true, Some("laya"), None).await;
        fixture.qualify().await;
        fixture.backend.calls.lock().unwrap().clear();
        let mut request = fixture.request();
        request.questions = serde_json::from_value(json!({
            "route":{"type":"choice","criteria":{"accept":null}},
            "urgency":{"type":"score","criteria":["low","high"]},
            "safe":{"type":"noul"}
        }))
        .unwrap();
        let original = request.questions.clone();
        let expected = native_request_body(&request).unwrap();
        assert_eq!(request.questions, original);
        for question in expected["questions"].as_object().unwrap().values() {
            assert_eq!(question["instructions"], "");
        }
        fixture
            .adapter
            .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
            .await
            .unwrap();
        assert_eq!(
            *fixture.backend.calls.lock().unwrap(),
            vec![("POST".into(), "/v1/systemone".into(), expected)]
        );
    }

    #[tokio::test]
    async fn native_decision_images_are_rejected_without_backend_dispatch() {
        let fixture = fixture(true, Some("laya"), None).await;
        fixture.qualify().await;
        fixture.backend.calls.lock().unwrap().clear();
        let mut request = fixture.request();
        request.images = vec!["data:image/png;base64,YQ==".into()];
        assert!(matches!(
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
                .await,
            Err(EngineError::Unsupported(_))
        ));
        assert!(fixture.backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn score_limits_are_local_and_valid_levels_stay_unchanged() {
        let fixture = fixture(true, Some("laya"), None).await;
        fixture.qualify().await;
        fixture.backend.calls.lock().unwrap().clear();
        for levels in [1, 11] {
            let mut request = fixture.request();
            request.questions = serde_json::from_value(json!({
                "score":{"type":"score","criteria":vec![json!("level"); levels]}
            }))
            .unwrap();
            // The shared contract allows these; only llama.cpp restricts them.
            request.validate().unwrap();
            assert!(
                matches!(fixture.adapter.decide(fixture.spec.endpoint.as_ref().unwrap(), request).await,
                Err(EngineError::InvalidDecisionRequest(message)) if message.contains("2 to 10 levels") && message.contains("score"))
            );
        }
        assert!(fixture.backend.calls.lock().unwrap().is_empty());
        for levels in 2..=10 {
            let mut request = fixture.request();
            request.questions = serde_json::from_value(json!({
                "score":{"type":"score","instructions":{"task":["unchanged",1]},
                    "criteria":(0..levels).map(|n| json!({"level":n})).collect::<Vec<_>>()}
            }))
            .unwrap();
            let expected = json!({"state":request.state,"questions":request.questions});
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
                .await
                .unwrap();
            assert_eq!(
                fixture.backend.calls.lock().unwrap().last().unwrap(),
                &("POST".into(), "/v1/systemone".into(), expected)
            );
        }
    }

    #[tokio::test]
    async fn decision_dispatch_preserves_native_wire_semantics_and_optional_observations() {
        let fixture = fixture(true, Some("laya"), Some(json!(["decisions"]))).await;
        fixture.qualify().await;
        fixture.backend.calls.lock().unwrap().clear();
        let request = fixture.request();
        let expected = json!({"state":request.state,"questions":request.questions});
        let output = fixture
            .adapter
            .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
            .await
            .unwrap();
        assert_eq!(
            *fixture.backend.calls.lock().unwrap(),
            vec![("POST".into(), "/v1/systemone".into(), expected)]
        );
        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(
            value["answers"],
            fixture.backend.response.lock().unwrap().1["answers"]
        );
        assert_eq!(output.model.as_deref(), Some("native-model"));
        let usage = output.usage.unwrap();
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.total_tokens),
            (42, 0, 42)
        );
        assert!(usage.prompt_processing_tokens.is_none());
        assert!(matches!(
            output.answers["safe"],
            DecisionAnswer::Noul {
                probabilities: None,
                confidence: None,
                ..
            }
        ));
        *fixture.backend.response.lock().unwrap() = (
            StatusCode::OK,
            json!({"answers":{"safe":{"type":"noul","noul":0.6}}}),
        );
        let mut request = fixture.request();
        request.questions.retain(|name, _| name == "safe");
        let output = fixture
            .adapter
            .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
            .await
            .unwrap();
        assert!(output.model.is_none());
        assert!(output.usage.is_none());
        assert_eq!(
            serde_json::to_value(output).unwrap(),
            json!({"answers":{"safe":{"type":"noul","noul":0.6}}})
        );
    }

    #[tokio::test]
    async fn native_errors_and_transport_failures_never_fall_back() {
        let fixture = fixture(true, Some("laya"), None).await;
        fixture.qualify().await;
        fixture.backend.calls.lock().unwrap().clear();
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            *fixture.backend.response.lock().unwrap() =
                (status, json!({"error":{"message":"native failure"}}));
            let mut request = fixture.request();
            request.state = DecisionContent::Array(vec![json!({"content":"kept structured"})]);
            assert!(
                matches!(fixture.adapter.decide(fixture.spec.endpoint.as_ref().unwrap(),request).await,Err(EngineError::Operation(message)) if message.contains("native failure"))
            );
        }
        *fixture.backend.response.lock().unwrap() =
            (StatusCode::OK, json!({"answers":{"safe":{"noul":0.5}}}));
        assert!(matches!(
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), fixture.request())
                .await,
            Err(EngineError::Operation(_))
        ));
        assert!(
            fixture
                .backend
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(method, path, _)| method == "POST" && path == "/v1/systemone")
        );
        fixture.server.abort();
        let mut request = fixture.request();
        request.model_profile_id = ModelProfileId::new("another-profile").unwrap();
        assert!(matches!(
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), request)
                .await,
            Err(EngineError::Unsupported(_))
        ));
        fixture.server.abort();
        tokio::task::yield_now().await;
        assert!(matches!(
            fixture
                .adapter
                .decide(fixture.spec.endpoint.as_ref().unwrap(), fixture.request())
                .await,
            Err(EngineError::BackendUnavailable(_))
        ));
    }

    #[test]
    fn gguf_decision_metadata_is_architecture_scoped_and_not_a_model_name_table() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Laya.gguf");
        gguf(&path, Some("future-upstream-classifier"), false);
        assert_eq!(
            scala_core::inspect_gguf_metadata(&path)
                .unwrap()
                .decision_type
                .as_deref(),
            Some("future-upstream-classifier")
        );
        gguf(&path, Some("laya"), true);
        assert!(
            scala_core::inspect_gguf_metadata(&path)
                .unwrap()
                .decision_type
                .is_none()
        );
        gguf(&path, None, false);
        assert!(
            scala_core::inspect_gguf_metadata(&path)
                .unwrap()
                .decision_type
                .is_none()
        );
    }
}
