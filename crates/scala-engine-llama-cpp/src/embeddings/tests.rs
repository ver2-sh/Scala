use super::*;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    response::IntoResponse,
};
use scala_core::{ModelId, ModelProfileId, ResolvedSettings};
use std::sync::{Arc, Mutex};

const IMAGE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";
const WAV: &[u8] = include_bytes!("../../../scala-engine/testdata/embedding.wav");
const AVI: &[u8] = include_bytes!("../../../scala-engine/testdata/embedding.avi");
fn base64(bytes: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(bytes)
}

struct Backend {
    props: Mutex<Value>,
    models: Mutex<Value>,
    probe: Mutex<(StatusCode, Value)>,
    response: Mutex<(StatusCode, Value)>,
    calls: Mutex<Vec<(String, Value)>>,
}
async fn handle(State(backend): State<Arc<Backend>>, request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_owned();
    let bytes = axum::body::to_bytes(request.into_body(), scala_api::MAX_INFERENCE_BODY_BYTES)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    backend
        .calls
        .lock()
        .unwrap()
        .push((path.clone(), value.clone()));
    let (status, value) = match path.as_str() {
        "/props" => (StatusCode::OK, backend.props.lock().unwrap().clone()),
        "/v1/models" => (StatusCode::OK, backend.models.lock().unwrap().clone()),
        "/v1/embeddings" if value == json!({"input":[]}) => backend.probe.lock().unwrap().clone(),
        "/v1/embeddings" => backend.response.lock().unwrap().clone(),
        _ => (StatusCode::NOT_FOUND, json!({"error":"unexpected route"})),
    };
    (status, Json(value))
}

struct Fixture {
    directory: tempfile::TempDir,
    spec: LaunchSpec,
    process: ProcessDescriptor,
    adapter: LlamaCppAdapter,
    backend: Arc<Backend>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn gguf(path: &Path, projector: bool) {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend((if projector { 3u64 } else { 2 }).to_le_bytes());
    let mut field = |key: &str, kind: u32, value: &[u8]| {
        bytes.extend((key.len() as u64).to_le_bytes());
        bytes.extend(key.as_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend(value);
    };
    let architecture = if projector { "clip" } else { "bert" };
    let mut value = (architecture.len() as u64).to_le_bytes().to_vec();
    value.extend(architecture.as_bytes());
    field("general.architecture", 8, &value);
    if projector {
        field("clip.has_vision_encoder", 7, &[1]);
        field("clip.has_audio_encoder", 7, &[1]);
    } else {
        field("bert.pooling_type", 4, &1u32.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}
async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("arbitrary.gguf");
    gguf(&path, false);
    let path = std::fs::canonicalize(path).unwrap();
    let model = ModelArtifact {
        id: ModelId("synthetic".into()),
        display_name: "arbitrary".into(),
        path: path.clone(),
        format: ArtifactFormat::Gguf,
        size_bytes: std::fs::metadata(&path).unwrap().len(),
        created: 1,
        hash: Some(hash_file(&path).await.unwrap()),
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
    std::fs::write(&executable, b"inert synthetic runtime; never run").unwrap();
    for name in ["ffmpeg", "ffprobe"] {
        let name = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        };
        let tool = directory.path().join(name);
        std::fs::write(&tool, b"inert media resource; never run").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let sha = hash_file(&executable).await.unwrap();
    let identity:RuntimeIdentity=serde_json::from_value(json!({"engine_id":ENGINE_ID,"package_family":"llama-cpp","version":"b11429",
        "upstream_revision":"d81235049384534c167caea52b85a694f6103d14","platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "accelerator":"cpu","variant":"cpu","package":{"provider_id":LLAMA_CPP_RUNTIME_PROVIDER_ID,"repository":"ggml-org/llama.cpp",
        "release_tag":"b11429","asset_id":"1","asset_name":"fixture.zip"}})).unwrap();
    let runtime_id = RuntimeId::from_identity(&identity);
    let runtime:InstalledRuntime=serde_json::from_value(json!({"installation_root":directory.path(),"manifest":{
        "schema_version":scala_core::RUNTIME_MANIFEST_SCHEMA_VERSION,"runtime_id":runtime_id,"identity":identity,
        "supported_formats":["gguf"],"requirements":scala_core::RuntimeRequirements::default(),"acquisition_method":"official_release_asset",
        "source_url":"https://synthetic.invalid/fixture.zip","downloaded_archive_sha256":"b".repeat(64),
        "entrypoint":"llama-server","entrypoint_sha256":sha,"installed_at_unix":1,"probe":{"compatible":true,"observed_engine_id":ENGINE_ID,
        "observed_version":"b11429","observed_revision":"d81235049","detail":"synthetic","observed_at_unix":1}}})).unwrap();
    let profile_id = ModelProfileId::new("embedding-profile").unwrap();
    let mut settings = ResolvedSettings {
        engine_id: ENGINE_ID.into(),
        model_profile_id: Some(profile_id.clone()),
        ..Default::default()
    };
    let projector = directory.path().join("projector.gguf");
    gguf(&projector, true);
    for (id, value) in [
        ("llama.cpp.projector", SettingValue::Path(projector.clone())),
        (
            "llama.cpp.projector_sha256",
            SettingValue::String(hash_file(&projector).await.unwrap()),
        ),
    ] {
        settings.configured.insert(
            SettingId::new(id).unwrap(),
            scala_core::ResolvedSetting {
                value,
                source: scala_core::SettingSource::Invocation,
            },
        );
    }
    let backend = Arc::new(Backend {
        props: Mutex::new(
            json!({"model_path":path,"model_alias":profile_id,"build_info":"b11429-d81235049","is_sleeping":false,
            "modalities":{"vision":true,"audio":true,"video":true},"total_slots":1,
            "default_generation_settings":{"n_ctx":2048,"params":{"temperature":0.8,"top_p":0.9}}}),
        ),
        models: Mutex::new(
            json!({"data":[{"id":profile_id,"architecture":{"input_modalities":["text","image","audio","video"]}}]}),
        ),
        probe: Mutex::new((
            StatusCode::BAD_REQUEST,
            json!({"error":{"type":"invalid_request_error","message":"\"input\" must not be empty"}}),
        )),
        response: Mutex::new((
            StatusCode::OK,
            json!({"data":[{"index":0,"embedding":[0.25,0.75]}],"usage":{"prompt_tokens":17,"total_tokens":17}}),
        )),
        calls: Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(handle).with_state(backend.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let engine = EngineRevision {
        engine_id: ENGINE_ID.into(),
        version: Some(identity.version.clone()),
        revision: identity.upstream_revision.clone(),
    };
    let installation:EngineInstallation=serde_json::from_value(json!({"engine":engine,"source_repository":UPSTREAM_REPOSITORY,
        "acquisition_method":{"method":"official_binary"},"binary_path":executable,"binary_sha256":sha,"build":null,
        "platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"runtime_variant":"cpu","acquired_at_unix":1,"observed_at_unix":1})).unwrap();
    let process = ProcessDescriptor {
        supervisor_id: "synthetic".into(),
        process_id: 0,
        engine,
        runtime_id,
        runtime_version: identity.version,
        runtime_variant: "cpu".into(),
        runtime_executable_sha256: sha,
        model_id: model.id.clone(),
        endpoint: Some(endpoint.clone()),
        launched_at_unix: 1,
    };
    let spec = LaunchSpec {
        executable,
        arguments: vec!["--embedding".into()],
        environment: BTreeMap::from([(
            "PATH".into(),
            directory.path().to_string_lossy().into_owned(),
        )]),
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
        directory,
        spec,
        process,
        adapter: LlamaCppAdapter::from_config(None, Path::new(".")),
        backend,
        server,
    }
}
impl Fixture {
    fn request(&self, input: Value) -> EmbeddingRequest {
        EmbeddingRequest {
            model_profile_id: self.spec.settings.model_profile_id.clone().unwrap(),
            input: serde_json::from_value(input).unwrap(),
        }
    }
    async fn qualify(&self) {
        self.adapter.prepare_embeddings(&self.spec).await;
        self.adapter.observe_embeddings(&self.process).await;
    }
    fn validate(&self, request: &EmbeddingRequest) -> Result<(), EngineError> {
        self.adapter.validate_embedding_request(
            &self.spec.runtime,
            &self.spec.model.primary,
            &self.spec.settings,
            self.spec.endpoint.as_deref().unwrap(),
            request,
        )
    }
}

#[tokio::test]
async fn embedding_all_native_parts_preserve_groups_and_order() {
    let f = fixture().await;
    f.qualify().await;
    let audio = json!({"type":"input_audio","input_audio":{"data":base64(WAV),"format":"wav"}});
    let video = json!({"type":"input_video","input_video":{"url":format!("data:video/x-msvideo;base64,{}",base64(AVI))}});
    for input in [
        json!({"content":[{"type":"image_url","image_url":{"url":IMAGE.split_once(',').unwrap().1}}]}),
        json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}},{"type":"text","text":"  exact image text\n"}]}),
        json!({"content":[{"type":"text","text":"A"},audio,{"type":"text","text":"B"}]}),
        json!({"content":[video,{"type":"text","text":"video"}]}),
        json!(["first",{"content":[{"type":"text","text":"before"},{"type":"image_url","image_url":{"url":IMAGE}},audio,{"type":"text","text":"after"}]},{"content":[{"type":"text","text":"third"}]}]),
    ] {
        let request = f.request(input.clone());
        f.validate(&request).unwrap();
        let n = request.input.len();
        let data = (0..n)
            .rev()
            .map(|index| json!({"index":index,"embedding":[index as f32,1.0]}))
            .collect::<Vec<_>>();
        *f.backend.response.lock().unwrap() = (
            StatusCode::OK,
            json!({"data":data,"usage":{"prompt_tokens":17,"total_tokens":17}}),
        );
        let output = f
            .adapter
            .embed(f.spec.endpoint.as_deref().unwrap(), request)
            .await
            .unwrap();
        assert_eq!(
            output.vectors,
            (0..n).map(|i| vec![i as f32, 1.0]).collect::<Vec<_>>()
        );
        assert_eq!(output.prompt_tokens, Some(17));
        assert_eq!(output.total_tokens, Some(17));
        assert_eq!(
            f.backend.calls.lock().unwrap().last().unwrap().1["input"],
            input
        );
    }
}

#[tokio::test]
async fn embedding_capability_is_exact_independent_and_process_local() {
    let mut f = fixture().await;
    let image = f.request(json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}}]}));
    assert!(f.validate(&image).is_err()); // candidate is not verified
    f.qualify().await;
    f.validate(&image).unwrap();
    for modality in ["vision", "audio", "video"] {
        f.backend.props.lock().unwrap()["modalities"][modality] = json!(false);
        f.adapter.observe_embeddings(&f.process).await;
        let part = match modality {
            "vision" => json!({"type":"image_url","image_url":{"url":IMAGE}}),
            "audio" => json!({"type":"input_audio","input_audio":{"data":base64(WAV)}}),
            _ => json!({"type":"input_video","input_video":{"data":base64(AVI)}}),
        };
        assert!(f.validate(&f.request(json!({"content":[part]}))).is_err());
        f.backend.props.lock().unwrap()["modalities"][modality] = json!(true);
    }
    for bad in [
        json!({"error":{"type":"invalid_request_error","message":"Pooling type 'none' is not OAI compatible. Please use a different pooling type"}}),
        json!({"error":{"type":"not_supported_error","message":"embedding mode is unavailable"}}),
    ] {
        *f.backend.probe.lock().unwrap() = (StatusCode::BAD_REQUEST, bad);
        f.qualify().await;
        assert!(f.validate(&image).is_err());
    }
    *f.backend.probe.lock().unwrap() = (
        StatusCode::BAD_REQUEST,
        json!({"error":{"type":"invalid_request_error","message":"\"input\" must not be empty"}}),
    );
    f.spec.settings.configured.clear();
    f.qualify().await;
    assert!(f.validate(&image).is_err());
    // Structured text needs the frozen shape/pooled endpoint, but no projector.
    f.validate(&f.request(json!({"content":[{"type":"text","text":"only text"}]})))
        .unwrap();
    f.spec.runtime.manifest.identity.upstream_revision = Some("a".repeat(40));
    f.qualify().await;
    assert!(f.validate(&image).is_err());
    for input in [json!("legacy"), json!(["one", "two"])] {
        f.validate(&f.request(input)).unwrap();
    }
    f.adapter
        .clear_launch_state(f.spec.endpoint.as_deref())
        .await;
    assert!(f.adapter.embedding_proofs.read().unwrap().is_empty());
}

#[tokio::test]
async fn embedding_projector_and_runtime_identity_changes_invalidate_proof() {
    let f = fixture().await;
    f.qualify().await;
    let request =
        f.request(json!({"content":[{"type":"input_video","input_video":{"data":base64(AVI)}}]}));
    f.validate(&request).unwrap();
    std::fs::write(
        f.directory.path().join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        }),
        b"changed",
    )
    .unwrap();
    assert!(f.validate(&request).is_err());
    f.qualify().await;
    let image = f.request(json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}}]}));
    f.validate(&image).unwrap();
    std::fs::write(
        f.directory.path().join("projector.gguf"),
        b"corrupt projector",
    )
    .unwrap();
    assert!(f.validate(&image).is_err());
    f.qualify().await;
    assert!(f.validate(&image).is_err());
}

#[tokio::test]
async fn embedding_vectors_and_native_usage_are_validated() {
    let f = fixture().await;
    let request = f.request(json!(["one", "two"]));
    for response in [
        json!({"data":[]}),
        json!({"data":[{"index":0,"embedding":[1.0]}]}),
        json!({"data":[{"index":0,"embedding":[1.0]},{"index":0,"embedding":[2.0]}]}),
        json!({"data":[{"index":0,"embedding":[1.0]},{"index":2,"embedding":[2.0]}]}),
        json!({"data":[{"index":0,"embedding":[]},{"index":1,"embedding":[]}]}),
        json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[2.0,3.0]}]}),
        json!({"data":[{"index":0,"embedding":["NaN"]},{"index":1,"embedding":[1.0]}]}),
        json!({"data":[{"index":0,"embedding":[1e100]},{"index":1,"embedding":[1.0]}]}),
        json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[2.0]}],"usage":{"prompt_tokens":1,"total_tokens":2}}),
    ] {
        *f.backend.response.lock().unwrap() = (StatusCode::OK, response);
        assert!(
            f.adapter
                .embed(f.spec.endpoint.as_deref().unwrap(), request.clone())
                .await
                .is_err()
        );
    }
    *f.backend.response.lock().unwrap() = (
        StatusCode::OK,
        json!({"data":[{"index":1,"embedding":[2.0]},{"index":0,"embedding":[1.0]}]}),
    );
    let output = f
        .adapter
        .embed(f.spec.endpoint.as_deref().unwrap(), request)
        .await
        .unwrap();
    assert_eq!(output.vectors, vec![vec![1.0], vec![2.0]]);
    assert_eq!(output.prompt_tokens, None);
    assert_eq!(output.total_tokens, None);
    *f.backend.response.lock().unwrap() = (
        StatusCode::BAD_REQUEST,
        json!({"error":{"message":format!("sensitive:{IMAGE}")}}),
    );
    let error = f
        .adapter
        .embed(f.spec.endpoint.as_deref().unwrap(), f.request(json!("x")))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("sensitive"));
    assert!(!error.to_string().contains(IMAGE));
}

#[tokio::test]
async fn embedding_both_frozen_revisions_require_matching_loaded_identity() {
    for revision in [
        "d81235049384534c167caea52b85a694f6103d14",
        "f9a95aeb2096d0c0218e0cd6e6d901cd37be798a",
    ] {
        let mut f = fixture().await;
        f.spec.runtime.manifest.identity.upstream_revision = Some(revision.into());
        let build = if revision.starts_with("d812") {
            "b11429"
        } else {
            "b11565"
        };
        f.spec.runtime.manifest.identity.version = build.into();

        f.spec.runtime.manifest.runtime_id =
            RuntimeId::from_identity(&f.spec.runtime.manifest.identity);
        f.process.runtime_id = f.spec.runtime.manifest.runtime_id.clone();
        f.backend.props.lock().unwrap()["build_info"] =
            json!(format!("{build}-{}", &revision[..9]));
        f.qualify().await;
        let request =
            f.request(json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}}]}));
        f.validate(&request).unwrap();
        for (field, value) in [
            ("build_info", json!("b11565-aaaaaaaaa")),
            ("model_alias", json!("other-profile")),
            ("model_path", json!("other.gguf")),
            ("is_sleeping", json!(true)),
        ] {
            let old = f.backend.props.lock().unwrap()[field].clone();
            f.backend.props.lock().unwrap()[field] = value;
            f.adapter.observe_embeddings(&f.process).await;
            assert!(f.validate(&request).is_err());
            f.backend.props.lock().unwrap()[field] = old;
            f.adapter.observe_embeddings(&f.process).await;
            f.validate(&request).unwrap();
        }
        f.backend.models.lock().unwrap()["data"][0]["architecture"]["input_modalities"] =
            json!(["text", "audio"]);
        f.adapter.observe_embeddings(&f.process).await;
        assert!(f.validate(&request).is_err());
    }
}

#[tokio::test]
async fn embedding_artifact_bound_projector_uses_the_same_integrity_boundary() {
    let mut f = fixture().await;
    let path = f.directory.path().join("projector.gguf");
    let sha = hash_file(&path).await.unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    f.spec.settings.configured.clear();
    f.spec
        .model
        .auxiliary
        .push(scala_engine::PreparedAuxiliaryArtifact {
            role: scala_core::AuxiliaryArtifactRole::Projector,
            path: path.clone(),
            size_bytes: size,
            content_sha256: sha,
        });
    f.qualify().await;
    let request = f.request(json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}}]}));
    f.validate(&request).unwrap();
    f.spec.model.auxiliary[0].content_sha256 = "a".repeat(64);
    f.qualify().await;
    assert!(f.validate(&request).is_err());
    f.spec.model.auxiliary[0].content_sha256 = hash_file(&path).await.unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() = 0;
    std::fs::write(&path, bytes).unwrap();
    f.spec.model.auxiliary[0].content_sha256 = hash_file(&path).await.unwrap();
    f.qualify().await;
    f.validate(&request).unwrap(); // vision remains independent of the removed audio encoder
    let audio =
        f.request(json!({"content":[{"type":"input_audio","input_audio":{"data":base64(WAV)}}]}));
    assert!(f.validate(&audio).is_err());
    if let Some(ArtifactNativeIdentity::Gguf(identity)) = &mut f.spec.model.primary.native_identity
    {
        identity.pooling_type = Some(0);
    }
    assert!(
        !f.adapter
            .supports_model_capability(&f.spec.model.primary, ApiCapability::Embeddings)
    );
    f.qualify().await;
    assert!(f.validate(&request).is_err());
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
    ) -> Result<tokio::sync::watch::Receiver<Option<scala_engine::ProcessExit>>, EngineError> {
        Ok(self.exit.subscribe())
    }
    async fn stderr_tail(&self, _: &ProcessDescriptor) -> Result<Vec<String>, EngineError> {
        Ok(Vec::new())
    }
    async fn shutdown(&self) {}
}

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
    fn setting_definitions(&self) -> Vec<SettingDefinition> {
        llama_setting_definitions()
    }
    fn supports_model_capability(&self, m: &ModelArtifact, c: ApiCapability) -> bool {
        self.native.supports_model_capability(m, c)
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
        r: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        Ok(r.manifest.probe.clone())
    }
    async fn build_launch_spec(&self, r: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        Ok(LaunchSpec {
            model: r.model,
            runtime: r.runtime,
            settings: r.settings,
            ..self.template.clone()
        })
    }
    async fn prepare_launch_attempt(&self, s: &LaunchSpec) -> Result<(), EngineError> {
        self.native.prepare_embeddings(s).await;
        Ok(())
    }
    async fn clear_launch_state(&self, e: Option<&str>) {
        self.native.clear_launch_state(e).await;
    }
    async fn health(&self, _: &ProcessDescriptor) -> Result<bool, EngineError> {
        Ok(true)
    }
    async fn startup_observation(
        &self,
        p: &ProcessDescriptor,
        _: &[String],
    ) -> Result<StartupObservation, EngineError> {
        self.native.observe_embeddings(p).await;
        Ok(StartupObservation::Ready(BTreeMap::new()))
    }
    async fn effective_generation_settings(
        &self,
        _: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        Ok(EffectiveGenerationSettings {
            temperature: Some(0.0),
            top_p: Some(1.0),
        })
    }
    fn validate_embedding_request(
        &self,
        r: &InstalledRuntime,
        m: &ModelArtifact,
        s: &ResolvedSettings,
        e: &str,
        q: &EmbeddingRequest,
    ) -> Result<(), EngineError> {
        self.native.validate_embedding_request(r, m, s, e, q)
    }
    async fn embed(
        &self,
        e: &str,
        q: EmbeddingRequest,
    ) -> Result<scala_engine::EmbeddingOutput, EngineError> {
        self.native.embed(e, q).await
    }
    async fn infer(&self, _: &str, _: InferenceRequest) -> Result<InferenceOutput, EngineError> {
        panic!("Embeddings must never use generation")
    }
    async fn infer_stream(
        &self,
        _: &str,
        _: InferenceRequest,
        _: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        panic!("Embeddings must never use generation")
    }
}

#[tokio::test]
async fn embedding_public_api_cold_profile_jit_float_base64_and_errors() {
    use scala_core::{AppPaths, ApplicationCore, EngineId, ModelProfilesStore};
    use scala_engine::{
        EngineRegistry, RuntimeManager, RuntimeManagerOptions, RuntimePackManager, RuntimeStore,
    };
    use std::sync::atomic::Ordering;
    let f = fixture().await;
    let root = f.directory.path().join("app");
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
            serde_json::to_string(&f.spec.model.primary.path).unwrap()
        ),
    )
    .unwrap();
    let core = ApplicationCore::load_from_paths(paths.clone())
        .await
        .unwrap();
    core.refresh_models().await.unwrap();
    let model = core
        .snapshot()
        .await
        .models
        .into_iter()
        .find(|m| m.path == f.spec.model.primary.path)
        .unwrap();
    let profile = f.spec.settings.model_profile_id.clone().unwrap();
    let overrides = f
        .spec
        .settings
        .configured
        .iter()
        .map(|(id, s)| (id.clone(), s.value.clone()))
        .collect::<BTreeMap<_, _>>();
    ModelProfilesStore::new(&paths)
        .update({
            let profile = profile.clone();
            move |state| {
                state.create(
                    profile.clone(),
                    "Synthetic embedding",
                    model.id,
                    EngineId::new(ENGINE_ID).unwrap(),
                )?;
                for (id, value) in overrides {
                    state
                        .profiles
                        .get_mut(&profile)
                        .unwrap()
                        .overrides
                        .insert(id, value);
                }
                Ok(())
            }
        })
        .await
        .unwrap();
    let store = RuntimeStore::new(&paths);
    let installation = store.installation_path(&f.spec.runtime.manifest);
    std::fs::create_dir_all(&installation).unwrap();
    std::fs::copy(&f.spec.executable, installation.join("llama-server")).unwrap();
    std::fs::write(
        installation.join("runtime.json"),
        serde_json::to_vec(&f.spec.runtime.manifest).unwrap(),
    )
    .unwrap();
    let supervisor = Arc::new(SyntheticSupervisor {
        launches: Default::default(),
        exit: tokio::sync::watch::channel(None).0,
    });
    let adapter = Arc::new(ManagedAdapter {
        native: LlamaCppAdapter::from_config(None, Path::new(".")),
        template: f.spec.clone(),
    });
    let mut registry = EngineRegistry::default();
    registry.register(adapter.clone()).unwrap();
    let packs = RuntimePackManager::new(&paths, registry.clone(), Vec::new()).unwrap();
    let manager = RuntimeManager::initialize(
        core.clone(),
        registry,
        packs,
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
    let before_settings = std::fs::read(&paths.model_profiles_file).unwrap();
    for input in [
        json!("legacy"),
        json!(["first", "second"]),
        json!({"content":[{"type":"image_url","image_url":{"url":IMAGE}},{"type":"text","text":"  exact\n"}]}),
        json!({"content":[{"type":"input_audio","input_audio":{"data":base64(WAV),"format":"wav"}}]}),
        json!({"content":[{"type":"input_video","input_video":{"url":format!("data:video/x-msvideo;base64,{}",base64(AVI))}}]}),
        json!(["before",{"content":[{"type":"image_url","image_url":{"url":IMAGE}},{"type":"text","text":"after"}]}]),
    ] {
        let n = if let Value::Array(v) = &input {
            v.len()
        } else {
            1
        };
        let data = (0..n)
            .rev()
            .map(|index| json!({"index":index,"embedding":[index as f32,0.75]}))
            .collect::<Vec<_>>();
        *f.backend.response.lock().unwrap() = (
            StatusCode::OK,
            json!({"data":data,"usage":{"prompt_tokens":17,"total_tokens":17}}),
        );
        for encoding in ["float", "base64"] {
            assert!(manager.status().await.backends.is_empty());
            let response = reqwest::Client::new()
                .post(format!("{endpoint}/v1/embeddings"))
                .header("x-request-id", "embedding-correlation")
                .header("x-scala-session-id", "embedding-session")
                .json(&json!({"model":profile,"input":input,"encoding_format":encoding}))
                .send()
                .await
                .unwrap();
            assert!(
                response.headers()["x-request-id"]
                    .to_str()
                    .unwrap()
                    .starts_with("req_")
            );
            let status = response.status();
            let value: Value = response.json().await.unwrap();
            assert_eq!(status, StatusCode::OK, "{value}");
            assert_eq!(value["model"], profile.as_str());
            assert_eq!(
                value["usage"],
                json!({"prompt_tokens":17,"total_tokens":17})
            );
            assert_eq!(value["data"].as_array().unwrap().len(), n);
            for index in 0..n {
                assert_eq!(value["data"][index]["index"], index);
                let expected = vec![index as f32, 0.75];
                if encoding == "float" {
                    assert_eq!(value["data"][index]["embedding"], json!(expected));
                } else {
                    assert_eq!(
                        value["data"][index]["embedding"],
                        base64(
                            &expected
                                .iter()
                                .flat_map(|v| v.to_le_bytes())
                                .collect::<Vec<_>>()
                        )
                    );
                }
            }
            assert_eq!(
                f.backend.calls.lock().unwrap().last().unwrap().1["input"],
                if input.is_string() {
                    json!([input])
                } else {
                    input.clone()
                }
            );
            manager.unload(profile.clone()).await.unwrap();
            assert!(adapter.native.embedding_proofs.read().unwrap().is_empty());
        }
    }
    let before = supervisor.launches.load(Ordering::SeqCst);
    for input in [
        json!([1, 2]),
        json!({"content":[]}),
        json!({"content":[{"type":"file"}]}),
        json!({"content":[{"type":"image_url","image_url":{"url":"http://169.254.169.254/secret"}}]}),
        json!({"content":[{"type":"input_audio","input_audio":{"data":"!!!!"}}]}),
        json!({"content":[{"type":"input_video","input_video":{"url":"file:///etc/passwd"}}]}),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{endpoint}/v1/embeddings"))
            .json(&json!({"model":profile,"input":input}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["error"]["param"], "input");
        assert_eq!(value["error"]["type"], "invalid_request_error");
    }
    assert_eq!(supervisor.launches.load(Ordering::SeqCst), before);
    assert_eq!(
        std::fs::read(&paths.model_profiles_file).unwrap(),
        before_settings
    );
    f.backend.props.lock().unwrap()["modalities"]["audio"] = json!(false);
    let response=reqwest::Client::new().post(format!("{endpoint}/v1/embeddings")).json(&json!({"model":profile,"input":{"content":[{"type":"input_audio","input_audio":{"data":base64(WAV)}}]}})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["error"]["code"], "unsupported_embedding_media");
    manager.shutdown().await;
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}
