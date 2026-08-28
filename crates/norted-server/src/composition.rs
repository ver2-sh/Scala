use std::sync::Arc;

use color_eyre::Result;
use norted_core::ApplicationCore;
use norted_engine::{
    EngineRegistry, RuntimeCatalogProvider, RuntimeManager, RuntimeManagerOptions,
    RuntimePackManager, TokioProcessSupervisor,
};
use norted_engine_llama_cpp::{
    ENGINE_ID as LLAMA_CPP_ENGINE_ID, LlamaCppAdapter, LlamaCppRuntimeCatalogProvider,
};
use norted_engine_ninfer::{
    ENGINE_ID as NINFER_ENGINE_ID, NinferAdapter, NinferRuntimeCatalogProvider,
};
use norted_engine_q27::{ENGINE_ID as Q27_ENGINE_ID, Q27Adapter, Q27RuntimeCatalogProvider};

pub async fn runtime_manager(core: Arc<ApplicationCore>) -> Result<Arc<RuntimeManager>> {
    let registry = engine_registry(&core)?;
    let packs = runtime_pack_manager(&core, registry.clone())?;
    Ok(RuntimeManager::initialize(
        core,
        registry,
        packs,
        Arc::new(TokioProcessSupervisor::default()),
        RuntimeManagerOptions::default(),
    )
    .await)
}

pub fn engine_registry(core: &ApplicationCore) -> Result<EngineRegistry> {
    let config_directory = core
        .config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut registry = EngineRegistry::default();
    registry.register(Arc::new(LlamaCppAdapter::from_config(
        core.config.engine.get(LLAMA_CPP_ENGINE_ID),
        config_directory,
    )))?;
    registry.register(Arc::new(Q27Adapter::from_config(
        core.config.engine.get(Q27_ENGINE_ID),
        config_directory,
    )))?;
    registry.register(Arc::new(NinferAdapter::from_config(
        core.config.engine.get(NINFER_ENGINE_ID),
        config_directory,
    )))?;
    Ok(registry)
}

pub fn runtime_pack_manager(
    core: &ApplicationCore,
    registry: EngineRegistry,
) -> Result<Arc<RuntimePackManager>> {
    let providers: Vec<Arc<dyn RuntimeCatalogProvider>> = vec![
        Arc::new(LlamaCppRuntimeCatalogProvider::new()),
        Arc::new(Q27RuntimeCatalogProvider::new()),
        Arc::new(NinferRuntimeCatalogProvider::new()),
    ];
    Ok(RuntimePackManager::new(&core.paths, registry, providers)?)
}
