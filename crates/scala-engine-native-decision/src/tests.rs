use super::*;
use axum::{Json, Router, extract::State, http::StatusCode};
use scala_core::{
    ModelRegistry, RuntimeId, RuntimeIdentity, RuntimeRequirements, SettingId, SettingValue,
    SettingsSchema,
};
use std::future::IntoFuture;
use std::sync::{Arc, Mutex};

struct Fixture {
    _dir: tempfile::TempDir,
    adapter: NativeDecisionAdapter,
    runtime: InstalledRuntime,
    model: ModelArtifact,
    settings: ResolvedSettings,
}
impl Fixture {
    fn new(backend: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut sources = serde_json::Map::new();
        let roles: &[(&str, &[&str])] = if backend == "vllm-labels" {
            &[(
                "model",
                &[
                    "config.json",
                    "model.safetensors",
                    "tokenizer.json",
                    "tokenizer_config.json",
                    "shim.py",
                    "serve.json",
                ],
            )]
        } else {
            &[
                (
                    "adapter",
                    &[
                        "adapter_config.json",
                        "adapter_model.safetensors",
                        "decision_readout.json",
                        "decision_readout.safetensors",
                        "calibration.json",
                    ],
                ),
                ("base", &["config.json", "model.safetensors"]),
            ]
        };
        for (role, names) in roles {
            let path = root.join(role);
            std::fs::create_dir(&path).unwrap();
            let mut files = serde_json::Map::new();
            for name in *names {
                std::fs::write(path.join(name), b"synthetic").unwrap();
                files.insert(
                    name.to_string(),
                    json!({"size_bytes":9,"sha256":format!("{:x}",Sha256::digest(b"synthetic"))}),
                );
            }
            sources.insert(role.to_string(),json!({"path":path,"repository":"fixture/model","revision":"1".repeat(40),"files":files}));
        }
        let bindings = if backend == "vllm-labels" {
            json!({"shim":"shim.py","serve_config":"serve.json"})
        } else {
            json!({"calibration":"calibration.json"})
        };
        let path = root.join("fixture.decisionbundle");
        std::fs::write(&path,serde_json::to_vec(&json!({"schema_version":1,"backend":backend,"sources":sources,"bindings":bindings})).unwrap()).unwrap();
        let model = ModelRegistry::discover(std::slice::from_ref(&root)).artifacts()[0].clone();
        let executable = root.join("fixture-runtime");
        let probe = json!({"protocol":PROTOCOL,"engine_id":ENGINE_ID,"version":"1","revision":"2".repeat(64),"backend":backend,"model_code_sha256":if backend == "vllm-labels" { Some(format!("{:x}",Sha256::digest(b"synthetic"))) } else { None }});
        // Metadata-only fake executable, never a model server or forward pass.
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ncat <<'PROBE'\n{probe}\nPROBE\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let identity: RuntimeIdentity = serde_json::from_value(json!({"engine_id":ENGINE_ID,"package_family":ENGINE_ID,"version":"1","upstream_revision":"2".repeat(64),"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"accelerator":"external","variant":backend,"package":{"provider_id":"external-binary","repository":null,"release_tag":null,"asset_id":executable,"asset_name":"fixture-runtime"}})).unwrap();
        let runtime_id = RuntimeId::from_identity(&identity);
        let runtime = serde_json::from_value(json!({"manifest":{"schema_version":scala_core::RUNTIME_MANIFEST_SCHEMA_VERSION,"runtime_id":runtime_id,"identity":identity,"supported_formats":["decisionbundle"],"requirements":RuntimeRequirements::default(),"acquisition_method":"external_binary","source_url":null,"downloaded_archive_sha256":null,"entrypoint":executable,"entrypoint_sha256":format!("{:x}",Sha256::digest(std::fs::read(&executable).unwrap())),"installed_at_unix":null,"probe":{"compatible":true,"observed_engine_id":ENGINE_ID,"observed_version":"1","observed_revision":"2".repeat(64),"detail":probe.to_string(),"observed_at_unix":1}},"installation_root":root})).unwrap();
        Self {
            _dir: dir,
            adapter: NativeDecisionAdapter::from_config(None, &root),
            runtime,
            model,
            settings: ResolvedSettings {
                engine_id: ENGINE_ID.into(),
                model_profile_id: Some(ModelProfileId::new("fixture").unwrap()),
                ..Default::default()
            },
        }
    }
    fn request(&self) -> DecisionRequest {
        serde_json::from_value(
            json!({"model_profile_id":"fixture","state":[{"item":"synthetic"}],"questions":{
            "choice":{"type":"choice","instructions":{"task":"pick"},"criteria":{"a":null,"b":"B"}},
            "score":{"type":"score","instructions":["rate"],"criteria":["low",{"high":1}]},
            "yes":{"type":"noul","instructions":"is it?"}}}),
        )
        .unwrap()
    }
}

#[test]
fn filenames_and_provenance_do_not_grant_decision_or_generation() {
    let f = Fixture::new("vllm-labels");
    assert!(native_decision_candidate(
        &f.adapter,
        &f.runtime,
        &f.model,
        &f.settings
    ));
    assert!(!native_decision_supported(
        &f.adapter,
        &f.runtime,
        &f.model,
        &f.settings
    ));
    assert!(
        !f.adapter
            .supports_model_capability(&f.model, ApiCapability::ChatCompletions)
    );
    let mut model = f.model.clone();
    model.display_name = "h2o-lightning-4b-v1-1-decision".into();
    model.path = f._dir.path().join("missing.decisionbundle");
    assert!(!native_decision_candidate(
        &f.adapter,
        &f.runtime,
        &model,
        &f.settings
    ));
    let mut runtime = f.runtime.clone();
    runtime.manifest.probe.compatible = false;
    assert!(!native_decision_candidate(
        &f.adapter,
        &runtime,
        &f.model,
        &f.settings
    ));
    let mut settings = f.settings.clone();
    settings.configured.insert(
        SettingId::new("native_decision.temperature").unwrap(),
        scala_core::ResolvedSetting {
            value: SettingValue::Float(0.8),
            source: scala_core::SettingSource::ModelProfile {
                model_profile_id: ModelProfileId::new("fixture").unwrap(),
            },
        },
    );
    assert!(!native_decision_candidate(
        &f.adapter, &f.runtime, &f.model, &settings
    ));
    runtime = f.runtime.clone();
    runtime.manifest.identity.variant = "torch-readout".into();
    assert!(!native_decision_candidate(
        &f.adapter,
        &runtime,
        &f.model,
        &f.settings
    ));
}

#[derive(Clone)]
struct Backend {
    health: Arc<Mutex<Value>>,
    calls: Arc<Mutex<Vec<Value>>>,
    response: Value,
}
async fn health(State(b): State<Backend>) -> Json<Value> {
    Json(b.health.lock().unwrap().clone())
}
async fn decide(State(b): State<Backend>, Json(v): Json<Value>) -> (StatusCode, Json<Value>) {
    b.calls.lock().unwrap().push(v.clone());
    if v == json!({}) {
        (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":{"type":"invalid_request_error","message":"\"state\" must be provided"}}),
            ),
        )
    } else {
        (StatusCode::OK, Json(b.response))
    }
}

#[tokio::test]
async fn both_backends_qualify_exact_pairs_and_preserve_native_answers() {
    for backend in ["vllm-labels", "torch-readout"] {
        let mut f = Fixture::new(backend);
        f.model = f
            .adapter
            .prepare_model_input(&f.model)
            .await
            .unwrap()
            .primary;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = format!("http://{address}");
        let response = json!({"model":"native/model","answers":{
            "choice":{"type":"choice","choice":"b","probabilities":{"a":0.2,"b":0.8},"confidence":0.6,"unknown_probability":0.1,"abstained":false},
            "score":{"type":"score","score":0.8,"legend":{"0":"low","1":{"high":1}},"probabilities":{"0":0.2,"1":0.8}},
            "yes":{"type":"noul","noul":0.73,"unknown_probability":0.04,"abstained":false}},"usage":{"input_tokens":7,"output_tokens":0,"latency_ms":2}});
        let b = Backend {
            health: Arc::new(Mutex::new(
                json!({"protocol":PROTOCOL,"runtime_revision":"2".repeat(64),"backend":backend,"bundle_sha256":f.model.hash,"launch_nonce":"nonce","native_readout":true}),
            )),
            calls: Arc::new(Mutex::new(vec![])),
            response: response.clone(),
        };
        let task = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/health", axum::routing::get(health))
                    .route("/v1/systemone", axum::routing::post(decide))
                    .with_state(b.clone()),
            )
            .into_future(),
        );
        let mut spec = f
            .adapter
            .build_launch_spec(LaunchRequest {
                model: PreparedModelInput {
                    primary: f.model.clone(),
                    auxiliary: vec![],
                    primary_file_identity: None,
                },
                runtime: f.runtime.clone(),
                accelerator_binding: None,
                backend_address: address,
                settings: f.settings.clone(),
                settings_schema: SettingsSchema::default(),
            })
            .await
            .unwrap();
        let i = spec
            .arguments
            .iter()
            .position(|a| a == "--launch-nonce")
            .unwrap();
        spec.arguments[i + 1] = "nonce".into();
        f.adapter.prepare_launch_attempt(&spec).await.unwrap();
        assert!(!native_decision_supported(
            &f.adapter,
            &f.runtime,
            &f.model,
            &f.settings
        ));
        assert!(f.adapter.qualify(&endpoint).await.unwrap());
        assert!(native_decision_supported(
            &f.adapter,
            &f.runtime,
            &f.model,
            &f.settings
        ));
        let request = f.request();
        let output = f.adapter.decide(&endpoint, request.clone()).await.unwrap();
        assert_eq!(
            serde_json::to_value(&output.answers).unwrap(),
            response["answers"]
        );
        assert_eq!(output.usage.unwrap().total_tokens, 7);
        assert_eq!(
            b.calls.lock().unwrap()[1],
            json!({"state":request.state,"questions":request.questions})
        );
        assert!(b.calls.lock().unwrap()[1].get("model").is_none());
        let mut changed = f.settings.clone();
        changed.model_profile_id = Some(ModelProfileId::new("other").unwrap());
        assert!(!native_decision_supported(
            &f.adapter, &f.runtime, &f.model, &changed
        ));
        b.health.lock().unwrap()["launch_nonce"] = json!("different-process");
        assert!(f.adapter.qualify(&endpoint).await.is_err());
        assert!(!native_decision_supported(
            &f.adapter,
            &f.runtime,
            &f.model,
            &f.settings
        ));
        f.adapter.clear_launch_state(Some(&endpoint)).await;
        assert!(!native_decision_supported(
            &f.adapter,
            &f.runtime,
            &f.model,
            &f.settings
        ));
        assert!(f.adapter.decide(&endpoint, request).await.is_err());
        task.abort();
    }
}

#[test]
fn malformed_native_answers_fail_closed() {
    let f = Fixture::new("torch-readout");
    for answers in [
        json!({}),
        json!({"choice":{"type":"noul","noul":0.5},"score":{"type":"score","score":0.5},"yes":{"type":"noul","noul":0.5}}),
        json!({"choice":{"type":"choice","choice":"missing"},"score":{"type":"score","score":0.5},"yes":{"type":"noul","noul":0.5}}),
    ] {
        assert!(validate_answers(&f.request(), &serde_json::from_value(answers).unwrap()).is_err());
    }
}

#[tokio::test]
async fn multiple_explicit_external_variants_are_discovered_independently() {
    let labels = Fixture::new("vllm-labels");
    let readout = Fixture::new("torch-readout");
    let config: EngineConfig = serde_json::from_value(json!({"enabled":true,"native":{"binaries":[labels.runtime.entrypoint_path(),readout.runtime.entrypoint_path(),labels._dir.path().join("missing-runtime")]}})).unwrap();
    let adapter = NativeDecisionAdapter::from_config(Some(&config), labels._dir.path());
    let probes = adapter.external_runtime_probes().await;
    assert_eq!(probes.len(), 3);
    for (probe, backend) in probes[..2].iter().zip(["vllm-labels", "torch-readout"]) {
        let probe = probe.as_ref().unwrap();
        let InstallationState::Installed { installation } = &probe.installation else {
            panic!("missing synthetic external runtime");
        };
        assert_eq!(installation.runtime_variant.as_deref(), Some(backend));
        assert!(probe.healthy);
    }
    assert!(probes[2].is_err());
}

#[test]
fn missing_native_usage_stays_absent_and_invalid_counts_fail() {
    assert!(native_usage(None).unwrap().is_none());
    assert!(
        native_usage(Some(&json!({"input_tokens":12})))
            .unwrap()
            .is_none()
    );
    assert!(native_usage(Some(&json!({"input_tokens":-1,"output_tokens":0}))).is_err());
    assert!(native_usage(Some(&json!({"input_tokens":u64::MAX,"output_tokens":1}))).is_err());
}

#[tokio::test]
async fn invalid_inference_configuration_preserves_runtime_identity() {
    let f = Fixture::new("vllm-labels");
    let config: EngineConfig = serde_json::from_value(json!({"enabled":true,"native":{"binary":f.runtime.entrypoint_path()},"settings":{"temperature":0.8}})).unwrap();
    let adapter = NativeDecisionAdapter::from_config(Some(&config), f._dir.path());
    let probe = adapter.probe().await.unwrap();
    assert!(probe.healthy);
    assert!(matches!(
        probe.installation,
        InstallationState::Installed { .. }
    ));
    assert!(adapter.runtime_compatibility(&f.runtime).is_supported());
    assert!(!native_decision_candidate(
        &adapter,
        &f.runtime,
        &f.model,
        &f.settings
    ));
    assert!(
        adapter
            .validate_configuration(
                &f.runtime,
                Some(&f.model),
                &HostCapabilities::current_without_accelerator_probe(),
                &f.settings
            )
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn slow_dependency_inventory_is_reused_and_topology_changes_rebuild_it() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let venv = root.join("venv");
    assert!(
        std::process::Command::new("python3")
            .args(["-m", "venv", "--without-pip"])
            .arg(&venv)
            .status()
            .unwrap()
            .success()
    );
    let python = venv.join("bin/python");
    let output = std::process::Command::new(&python)
        .args([
            "-I",
            "-B",
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()
        .unwrap();
    let site = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let dist = site.join("fixture-1.0.dist-info");
    std::fs::create_dir(&dist).unwrap();
    std::fs::write(dist.join("METADATA"), b"Name: fixture\nVersion: 1.0\n").unwrap();
    let record = dist.join("RECORD");
    std::fs::write(
        &record,
        b"dependency.py,,\nfixture-1.0.dist-info/RECORD,,\n",
    )
    .unwrap();
    let dependency = site.join("dependency.py");
    std::fs::write(&dependency, b"original").unwrap();
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("shim.py"), b"synthetic").unwrap();
    let config = root.join("runtime.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({"source":{"path":source,"files":{"shim.py":{}}}})).unwrap(),
    )
    .unwrap();
    let runner = root.join("server.py");
    std::fs::write(
        &runner,
        b"raise AssertionError('observation must never launch')",
    )
    .unwrap();
    let count = root.join("enumerations");
    let interpreter = root.join("slow-python");
    // Delays inventory construction beyond the old hard two-second deadline.
    // The real observation.py still enumerates the synthetic wheel's RECORD.
    std::fs::write(
        &interpreter,
        format!(
            "#!/bin/sh\nprintf 'inventory\\n' >> {count:?}\nsleep 2.1\nexec {python:?} \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = root.join("native-decision-server");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\nexec {interpreter:?} -I {runner:?} \"$@\"\n"),
    )
    .unwrap();
    let cfg: EngineConfig =
        serde_json::from_value(json!({"enabled":true,"native":{"binary":binary}})).unwrap();
    let adapter = NativeDecisionAdapter::from_config(Some(&cfg), &root);
    let enumerations = || std::fs::read_to_string(&count).unwrap().lines().count();
    let start = std::time::Instant::now();
    let first = adapter.external_runtime_observation_key().await.unwrap();
    assert!(start.elapsed() > Duration::from_secs(2));
    assert_eq!(
        adapter.external_runtime_observation_key().await.as_ref(),
        Some(&first)
    );
    assert_eq!(enumerations(), 1);
    std::fs::write(&dependency, b"modified").unwrap();
    let changed = adapter.external_runtime_observation_key().await.unwrap();
    assert_ne!(changed, first);
    assert_eq!(
        enumerations(),
        1,
        "file edits reuse the dependency inventory"
    );
    // RECORD edits must discover newly declared paths even without a directory
    // change (the new member is initially missing).
    std::fs::write(
        &record,
        b"dependency.py,,\nnew.py,,\nfixture-1.0.dist-info/RECORD,,\n",
    )
    .unwrap();
    let expanded = adapter.external_runtime_observation_key().await.unwrap();
    assert_ne!(expanded, changed);
    assert_eq!(enumerations(), 2);
    let new = site.join("new.py");
    std::fs::write(&new, b"original").unwrap();
    let added = adapter.external_runtime_observation_key().await.unwrap();
    assert_ne!(added, expanded);
    assert_eq!(
        enumerations(),
        3,
        "directory additions rebuild the inventory"
    );
    std::fs::write(&new, b"modified").unwrap();
    assert_ne!(
        adapter.external_runtime_observation_key().await.unwrap(),
        added
    );
    assert_eq!(enumerations(), 3, "new wheel members remain watched");
    std::fs::write(&config, b"invalid config").unwrap();
    assert!(adapter.external_runtime_observation_key().await.is_none());
    assert_eq!(enumerations(), 4);
    assert!(adapter.observation_paths.lock().await.is_empty());
}
