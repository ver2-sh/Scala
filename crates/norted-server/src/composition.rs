use std::sync::Arc;

use color_eyre::Result;
use norted_core::ApplicationCore;
use norted_engine::{
    EngineRegistry, RuntimeManager, RuntimeManagerOptions, TokioProcessSupervisor,
};
use norted_engine_llama_cpp::{ENGINE_ID, LlamaCppAdapter};

pub async fn runtime_manager(core: Arc<ApplicationCore>) -> Result<Arc<RuntimeManager>> {
    let config_directory = core
        .config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let adapter = LlamaCppAdapter::from_config(core.config.engine.get(ENGINE_ID), config_directory);
    let mut registry = EngineRegistry::default();
    registry.register(Arc::new(adapter))?;
    Ok(RuntimeManager::initialize(
        core,
        registry,
        Arc::new(TokioProcessSupervisor::default()),
        RuntimeManagerOptions::default(),
    )
    .await)
}
