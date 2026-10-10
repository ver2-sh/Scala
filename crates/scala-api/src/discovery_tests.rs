//! Synthetic source bundles and metadata-only Python executables; never servers.
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use scala_core::{EngineConfig, ModelProfileId, SettingsPatch};
use scala_engine::{
    EngineAdapter, EngineRegistry, RuntimeManagerOptions, RuntimePackManager,
    TokioProcessSupervisor,
};
use scala_engine_native_decision::NativeDecisionAdapter;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
use tower::ServiceExt;

struct Fixture {
    _temp: tempfile::TempDir,
    state: PublicApiState,
    packs: Arc<RuntimePackManager>,
    adapter: Arc<NativeDecisionAdapter>,
    counts: Vec<PathBuf>,
    dependencies: Vec<PathBuf>,
    binaries: Vec<PathBuf>,
    startup_time: std::time::Duration,
}

impl Fixture {
    async fn new() -> Self {
        let (temp, _, _, core) = tests::control_fixture().await;
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
        let models = root.join("models");
        let mut binaries = Vec::new();
        let mut counts = Vec::new();
        let mut dependencies = Vec::new();
        for (name, backend) in [
            ("h2o-lightning", "vllm-labels"),
            ("imajev", "torch-readout"),
        ] {
            let source = root.join(name);
            std::fs::create_dir(&source).unwrap();
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
            let mut sources = serde_json::Map::new();
            for (role, names) in roles {
                let path = source.join(role);
                std::fs::create_dir(&path).unwrap();
                let mut files = serde_json::Map::new();
                for name in *names {
                    std::fs::write(path.join(name), b"synthetic").unwrap();
                    files.insert(name.to_string(), json!({"size_bytes":9,"sha256":format!("{:x}",Sha256::digest(b"synthetic"))}));
                }
                sources.insert(role.to_string(), json!({"path":path,"repository":"fixture/model","revision":"1".repeat(40),"files":files}));
            }
            let bindings = if backend == "vllm-labels" {
                json!({"shim":"shim.py","serve_config":"serve.json"})
            } else {
                json!({"calibration":"calibration.json"})
            };
            std::fs::write(models.join(format!("{name}.decisionbundle")), serde_json::to_vec(&json!({"schema_version":1,"backend":backend,"sources":sources,"bindings":bindings})).unwrap()).unwrap();
            let runtime_dir = root.join(format!("{name}-runtime"));
            let runtime_source = root.join(format!("{name}-implementation"));
            std::fs::create_dir(&runtime_dir).unwrap();
            std::fs::create_dir(&runtime_source).unwrap();
            let dependency = runtime_source.join("dependency.py");
            std::fs::write(&dependency, b"synthetic").unwrap();
            let count = root.join(format!("{name}.count"));
            let probe = json!({"protocol":"scala-native-decision-v1","engine_id":"native_decision","version":"1","revision":if backend == "vllm-labels" { "2".repeat(64) } else { "3".repeat(64) },"backend":backend,"model_code_sha256":if backend == "vllm-labels" { Some(format!("{:x}",Sha256::digest(b"synthetic"))) } else { None }});
            let runner = runtime_dir.join("server.py");
            std::fs::write(&runner, format!("import sys,json,time\nfrom pathlib import Path\nassert sys.argv[1:] == ['--scala-probe'], 'fixture cannot serve'\nwith Path({count:?}).open('a') as f: f.write('probe\\n')\ntime.sleep(0.08)\nassert Path({dependency:?}).read_bytes() == b'synthetic', 'dependency changed'\nprint({:?})\n", probe.to_string())).unwrap();
            std::fs::write(
                runtime_dir.join("runtime.json"),
                serde_json::to_vec(
                    &json!({"source":{"path":runtime_source,"files":{"dependency.py":{}}}}),
                )
                .unwrap(),
            )
            .unwrap();
            let binary = runtime_dir.join("native-decision-server");
            std::fs::write(
                &binary,
                format!(
                    "#!/bin/sh\nexec {} -I {} \"$@\"\n",
                    python.display(),
                    runner.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            binaries.push(binary);
            counts.push(count);
            dependencies.push(dependency);
        }
        core.refresh_models().await.unwrap();
        let discovered = core.snapshot().await.models;
        scala_core::ModelProfilesStore::new(&core.paths)
            .update(move |profiles| {
                for (id, stem, engine) in [
                    ("gemma", "fixture", "llama.cpp"),
                    ("h2o-lightning", "h2o-lightning", "native_decision"),
                    ("imajev", "imajev", "native_decision"),
                ] {
                    let model = discovered
                        .iter()
                        .find(|m| m.path.file_stem().unwrap() == stem)
                        .unwrap();
                    profiles.create(
                        ModelProfileId::new(id).unwrap(),
                        id,
                        model.id.clone(),
                        scala_core::EngineId::new(engine).unwrap(),
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let config: EngineConfig =
            serde_json::from_value(json!({"enabled":true,"native":{"binaries":binaries}})).unwrap();
        let adapter = Arc::new(NativeDecisionAdapter::from_config(Some(&config), &root));
        let mut registry = EngineRegistry::default();
        registry.register(adapter.clone()).unwrap();
        let packs = RuntimePackManager::new(
            &core.paths,
            registry.clone(),
            Vec::<Arc<dyn scala_engine::RuntimeCatalogProvider>>::new(),
        )
        .unwrap();
        let startup = std::time::Instant::now();
        let runtime = RuntimeManager::initialize(
            core.clone(),
            registry,
            packs.clone(),
            Arc::new(TokioProcessSupervisor::default()),
            RuntimeManagerOptions::default(),
        )
        .await;
        let state = PublicApiState {
            core,
            runtime,
            instance_id: "synthetic".into(),
            link: None,
        };
        Self {
            _temp: temp,
            state,
            packs,
            adapter,
            counts,
            dependencies,
            binaries,
            startup_time: startup.elapsed(),
        }
    }

    fn probes(&self) -> Vec<usize> {
        self.counts
            .iter()
            .map(|p| {
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .lines()
                    .count()
            })
            .collect()
    }

    async fn get(&self, path: &str) -> Value {
        let router = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/models/{model}", get(retrieve_model))
            .with_state(self.state.clone());
        let response = router
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
    }
}

#[tokio::test]
async fn native_discovery_reuses_observations_and_supports_both_clients_without_loading() {
    let f = Fixture::new().await;
    let start = std::time::Instant::now();
    let cold = f.get("/v1/models").await;
    let cold_time = start.elapsed();
    assert_eq!(cold["data"].as_array().unwrap().len(), 4);
    assert_eq!(f.probes(), [1, 1]);
    let start = std::time::Instant::now();
    let warm = f.get("/v1/models?output_modalities=decisions").await;
    let warm_time = start.elapsed();
    let ids: Vec<_> = warm["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["h2o-lightning", "imajev"]);
    for model in warm["data"].as_array().unwrap() {
        assert_eq!(
            model["architecture"]["output_modalities"],
            json!(["decisions"])
        );
        assert_eq!(model["capabilities"]["decision_candidate"], true);
        assert!(model["capabilities"].get("decision").is_none());
        assert_eq!(model["owned_by"], "scala-user");
    }
    for model in cold["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["id"] == "fixture" || m["id"] == "gemma")
    {
        assert!(model.get("architecture").is_none());
        assert!(model.get("capabilities").is_none());
    }
    assert_eq!(
        f.get("/v1/models/h2o-lightning").await["id"],
        "h2o-lightning"
    );
    assert!(
        f.get("/v1/models?output_modalities=unsupported").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.get("/v1/models").await, cold);
    assert_eq!(f.probes(), [1, 1]);
    // The TUI profile editor uses this same settings-resolution entrypoint.
    let profiles = scala_core::ModelProfilesStore::new(&f.state.core.paths)
        .read()
        .await
        .unwrap();
    for id in ["h2o-lightning", "imajev", "h2o-lightning", "imajev"] {
        let profile = &profiles.profiles[&ModelProfileId::new(id).unwrap()];
        let model = f.state.core.model(&profile.model_id).await.unwrap();
        let settings = scala_core::SettingsStore::new(&f.state.core.paths)
            .read()
            .await
            .unwrap()
            .resolve(
                &profile.id,
                profile.engine_id.as_str(),
                &profile.overrides,
                &SettingsPatch::default(),
                &f.state.core.paths.data_dir,
            )
            .unwrap();
        f.packs
            .settings_schema_for_model_for_engine_with_settings(
                &model,
                profile.engine_id.as_str(),
                None,
                Some(&settings),
            )
            .await
            .unwrap();
    }
    assert_eq!(f.probes(), [1, 1]);
    assert!(f.state.runtime.status().await.backends.is_empty());
    eprintln!(
        "synthetic four-profile discovery: startup={:?}, first={cold_time:?}, warm={warm_time:?}, integrity probes={:?}",
        f.startup_time,
        f.probes()
    );
}

#[tokio::test]
async fn native_discovery_refreshes_and_dependency_changes_fail_closed_before_launch() {
    let f = Fixture::new().await;
    f.get("/v1/models").await;
    let observation = f.packs.inspect_local().await;
    let runtime = observation
        .installed
        .iter()
        .find(|r| r.runtime.manifest.identity.variant == "vllm-labels")
        .unwrap()
        .runtime
        .clone();
    std::fs::write(&f.dependencies[0], b"different").unwrap();
    // Execution always reprobes, even with the previous discovery object.
    assert!(f.adapter.probe_runtime(&runtime).await.is_err());
    let profiles = scala_core::ModelProfilesStore::new(&f.state.core.paths)
        .read()
        .await
        .unwrap();
    let profile = &profiles.profiles[&ModelProfileId::new("h2o-lightning").unwrap()];
    let model = f.state.core.model(&profile.model_id).await.unwrap();
    let settings = scala_core::ResolvedSettings {
        engine_id: "native_decision".into(),
        model_profile_id: Some(profile.id.clone()),
        ..Default::default()
    };
    assert!(
        f.adapter
            .build_launch_spec(scala_engine::LaunchRequest {
                model: scala_engine::PreparedModelInput {
                    primary: model,
                    auxiliary: Vec::new(),
                    primary_file_identity: None
                },
                runtime: runtime.clone(),
                accelerator_binding: None,
                backend_address: "127.0.0.1:1".parse().unwrap(),
                settings,
                settings_schema: scala_core::SettingsSchema::default(),
            })
            .await
            .is_err()
    );
    let listed = f.get("/v1/models?output_modalities=decisions").await;
    assert_eq!(listed["data"][0]["id"], "imajev");
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    std::fs::write(&f.dependencies[0], b"synthetic").unwrap();
    f.packs.refresh_local_observations().await;
    assert_eq!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    std::fs::remove_file(&f.dependencies[1]).unwrap();
    assert_eq!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::write(&f.dependencies[1], b"synthetic").unwrap();
    let mut executable = std::fs::read(&f.binaries[0]).unwrap();
    executable.extend_from_slice(b"# changed launcher\n");
    std::fs::write(&f.binaries[0], executable).unwrap();
    assert!(f.adapter.probe_runtime(&runtime).await.is_err());
    let refreshed = f.packs.inspect_local().await;
    let current = refreshed
        .installed
        .iter()
        .find(|r| r.runtime.manifest.identity.variant == "vllm-labels")
        .unwrap();
    assert_ne!(
        current.runtime.manifest.entrypoint_sha256,
        runtime.manifest.entrypoint_sha256
    );
    std::fs::remove_file(&f.binaries[0]).unwrap();
    std::fs::remove_file(&f.dependencies[1]).unwrap();
    assert!(f.adapter.probe_runtime(&runtime).await.is_err());
    assert!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(f.state.runtime.status().await.backends.is_empty());
}

#[tokio::test]
async fn native_discovery_rechecks_persistent_selections_and_profile_configuration() {
    let f = Fixture::new().await;
    f.get("/v1/models").await;
    let profiles = scala_core::ModelProfilesStore::new(&f.state.core.paths);
    let id = ModelProfileId::new("h2o-lightning").unwrap();
    let profile = profiles.read().await.unwrap().profiles[&id].clone();
    profiles
        .update({
            let id = id.clone();
            move |state| {
                state.profiles.get_mut(&id).unwrap().overrides.0.insert(
                    scala_core::SettingId::new("native_decision.temperature").unwrap(),
                    scala_core::SettingValue::Float(0.8),
                );
                Ok(())
            }
        })
        .await
        .unwrap();
    assert_eq!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    profiles
        .update({
            let id = id.clone();
            move |state| {
                state.profiles.get_mut(&id).unwrap().overrides = SettingsPatch::default();
                let chat = state.profiles[&ModelProfileId::new("fixture").unwrap()]
                    .model_id
                    .clone();
                state.create(
                    ModelProfileId::new("unsupported-gguf").unwrap(),
                    "Unsupported",
                    chat,
                    scala_core::EngineId::new("native_decision").unwrap(),
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let inspection = f.packs.inspect_local().await;
    let wrong_runtime = inspection
        .installed
        .iter()
        .find(|r| r.runtime.manifest.identity.variant == "torch-readout")
        .unwrap();
    let mut selections = scala_core::RuntimeSelections::default();
    selections.model_overrides.insert(
        profile.model_id,
        wrong_runtime.runtime.manifest.runtime_id.clone(),
    );
    std::fs::write(
        &f.state.core.paths.runtime_selections_file,
        serde_json::to_vec(&selections).unwrap(),
    )
    .unwrap();
    assert_eq!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let unsupported = f.get("/v1/models/unsupported-gguf").await;
    assert!(unsupported.get("architecture").is_none());
    assert!(unsupported.get("capabilities").is_none());
    std::fs::write(
        &f.state.core.paths.runtime_selections_file,
        b"invalid selections",
    )
    .unwrap();
    assert!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_file(&f.state.core.paths.runtime_selections_file).unwrap();
    assert_eq!(
        f.get("/v1/models?output_modalities=decisions").await["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(f.probes(), [1, 1]);
    assert!(f.state.runtime.status().await.backends.is_empty());

    // A changed explicit runtime configuration gets its own inventory, never
    // the observations of the previous registry/manager.
    let config: EngineConfig =
        serde_json::from_value(json!({"enabled":true,"native":{"binary":f.binaries[1]}})).unwrap();
    let mut registry = EngineRegistry::default();
    registry
        .register(Arc::new(NativeDecisionAdapter::from_config(
            Some(&config),
            f._temp.path(),
        )))
        .unwrap();
    let packs = RuntimePackManager::new(
        &f.state.core.paths,
        registry.clone(),
        Vec::<Arc<dyn scala_engine::RuntimeCatalogProvider>>::new(),
    )
    .unwrap();
    let runtime = RuntimeManager::initialize(
        f.state.core.clone(),
        registry,
        packs,
        Arc::new(TokioProcessSupervisor::default()),
        RuntimeManagerOptions::default(),
    )
    .await;
    let state = PublicApiState {
        runtime,
        ..f.state.clone()
    };
    let listed = public_models(&state).await.unwrap();
    let decisions = filter_models(
        listed,
        &ModelDiscoveryQuery {
            output_modalities: Some("decisions".into()),
        },
    );
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].id, "imajev");
    assert!(state.runtime.status().await.backends.is_empty());
}
