//! Explicit operational qualification instance, using the same AppPaths and
//! ApiServer boundaries as local contract fixtures. Never discovers live state.
//! Usage: isolated_native_decision NEW_STATE_ROOT BUNDLE_DIRECTORY RUNTIME...
use scala_api::{ApiServer, PublicAuth, PublicAuthVerifier};
use scala_core::{
    AppPaths, ApplicationCore, ArtifactFormat, EngineId, ModelProfileId, ModelProfilesStore,
    ModelRole,
};
use scala_engine::{
    EngineRegistry, RuntimeCatalogProvider, RuntimeManager, RuntimeManagerOptions,
    RuntimePackManager, TokioProcessSupervisor,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Key(String);
#[async_trait::async_trait]
impl PublicAuthVerifier for Key {
    async fn verify(&self, credential: &str) -> bool {
        credential == self.0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if args.len() < 3 || !args.iter().all(|p| p.is_absolute()) {
        return Err(
            "usage: isolated_native_decision NEW_ABSOLUTE_STATE_ROOT BUNDLE_DIRECTORY RUNTIME..."
                .into(),
        );
    }
    let root = &args[0];
    // create_dir, not create_dir_all: never overwrite an existing state tree.
    std::fs::create_dir(root)?;
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
    paths.ensure_required()?;
    let binaries = args[2..]
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    std::fs::write(
        &paths.config_file,
        format!(
            "version = 1\n[server]\nhost = \"127.0.0.1\"\nport = 0\n[link]\nenabled = false\n[models]\npaths = [{}]\n[engine.native_decision]\nenabled = true\n[engine.native_decision.native]\nbinaries = [{}]\n",
            serde_json::to_string(&args[1])?,
            binaries
        ),
    )?;
    let core = ApplicationCore::load_from_paths(paths.clone()).await?;
    core.refresh_models().await?;
    let models = core.snapshot().await.models;
    let profile_models = models.clone();
    ModelProfilesStore::new(&paths)
        .update(move |state| {
            for model in profile_models
                .iter()
                .filter(|m| m.format == ArtifactFormat::DecisionBundle)
            {
                let name = format!(
                    "{}-decision",
                    model.path.file_stem().unwrap().to_string_lossy()
                );
                let id = ModelProfileId::new(&name)?;
                state.create(
                    id.clone(),
                    &name,
                    model.id.clone(),
                    EngineId::new("native_decision")?,
                )?;
                state.profiles.get_mut(&id).unwrap().role = ModelRole::Auxiliary;
            }
            Ok(())
        })
        .await?;
    let mut registry = EngineRegistry::default();
    let adapter = Arc::new(
        scala_engine_native_decision::NativeDecisionAdapter::from_config(
            core.config.engine.get("native_decision"),
            &paths.config_dir,
        ),
    );
    registry.register(adapter)?;
    let packs = RuntimePackManager::new(
        &paths,
        registry.clone(),
        Vec::<Arc<dyn RuntimeCatalogProvider>>::new(),
    )?;
    // Exercise normal bounded discovery, without priming an adapter directly.
    // Operational setup can wait for its independently owned verification.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    let mut observations = Vec::new();
    loop {
        let start = std::time::Instant::now();
        let inventory = packs.list().await?;
        observations.push(json!({"seconds":start.elapsed().as_secs_f64(),"installed":inventory.installed.len(),"warnings":inventory.warnings}));
        if !inventory.installed.is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("bounded runtime discovery never completed verification".into());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    std::fs::write(
        root.join("discovery-bootstrap.json"),
        serde_json::to_vec_pretty(&observations)?,
    )?;

    let mut resolutions = serde_json::Map::new();
    for model in &models {
        match packs.compatible_installed_for_model(model).await {
            Ok(candidates) if candidates.len() == 1 => {
                packs
                    .select_model(model, candidates[0].runtime_id.clone())
                    .await?;
                resolutions.insert(
                    model.id.to_string(),
                    json!({"runtime_id":candidates[0].runtime_id}),
                );
            }
            Ok(_) => {
                resolutions.insert(
                    model.id.to_string(),
                    json!({"error":"ambiguous installed runtimes"}),
                );
            }
            Err(error) => {
                resolutions.insert(model.id.to_string(), json!({"error":error.to_string()}));
            }
        }
    }
    std::fs::write(
        root.join("resolutions.json"),
        serde_json::to_vec_pretty(&resolutions)?,
    )?;
    std::fs::write(
        root.join("runtime-inventory.json"),
        serde_json::to_vec_pretty(&packs.list().await?)?,
    )?;
    let runtime = RuntimeManager::initialize(
        core.clone(),
        registry,
        packs,
        Arc::new(TokioProcessSupervisor::default()),
        RuntimeManagerOptions::default(),
    )
    .await;
    let credential = uuid::Uuid::new_v4().to_string();
    let server = ApiServer::bind(
        core,
        runtime,
        PublicAuth::required(Arc::new(Key(credential.clone()))),
    )
    .await?;
    let descriptor: scala_core::RuntimeDescriptor =
        std::fs::read_dir(paths.state_dir.join("runtime/servers"))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .find_map(|entry| {
                serde_json::from_slice::<scala_core::RuntimeDescriptor>(
                    &std::fs::read(entry.path()).ok()?,
                )
                .ok()
            })
            .filter(|descriptor| descriptor.process_id == std::process::id())
            .ok_or("missing owned private control descriptor")?;
    // Local, ephemeral test credentials; never credentials from a live instance.
    std::fs::write(
        root.join("access.json"),
        serde_json::to_vec_pretty(
            &json!({"process_id":std::process::id(),"public_endpoint":format!("http://{}",server.local_addr()),"api_key":credential,"control_endpoint":descriptor.control_endpoint,"control_token":descriptor.control_token}),
        )?,
    )?;
    println!(
        "isolated native Decision instance at {}",
        server.local_addr()
    );
    let stop = root.join("stop");
    server
        .run(async move {
            let _ = tokio::time::timeout(Duration::from_secs(1800), async {
                while !stop.exists() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await;
        })
        .await?;
    std::fs::remove_file(root.join("access.json"))?;
    Ok(())
}
