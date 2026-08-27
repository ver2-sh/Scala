use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};

use crate::{
    AppConfig, AppEvent, AppPaths, LoadedConfig, LogLevel, ModelArtifact, ModelRegistry, Result,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeStatus {
    Stopped,
    Starting,
    Running { process_id: u32 },
    Stopping,
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ServerState {
    Stopped,
    Starting,
    Running { endpoint: String },
    Stopping,
    Failed { message: String },
}

impl ServerState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running { .. } => "Running",
            Self::Stopping => "Stopping",
            Self::Failed { .. } => "Failed",
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        match self {
            Self::Running { endpoint } => Some(endpoint),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub server: ServerState,
    pub models: Vec<ModelArtifact>,
    pub installed_engine_count: usize,
    pub running_engine_count: usize,
    pub active_model: Option<String>,
}

#[derive(Debug)]
struct MutableState {
    server: ServerState,
    registry: ModelRegistry,
}

#[derive(Debug)]
pub struct ApplicationCore {
    pub config: AppConfig,
    pub config_path: std::path::PathBuf,
    pub paths: AppPaths,
    state: RwLock<MutableState>,
    events: broadcast::Sender<AppEvent>,
}

impl ApplicationCore {
    pub async fn load() -> Result<Arc<Self>> {
        let paths = AppPaths::discover()?;
        paths.ensure_required()?;
        let LoadedConfig {
            config,
            path: config_path,
            ..
        } = LoadedConfig::load(&paths)?;
        config.server.ip_addr()?;
        let registry = ModelRegistry::discover(&config.models.paths);
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            config,
            config_path,
            paths,
            state: RwLock::new(MutableState {
                server: ServerState::Stopped,
                registry,
            }),
            events,
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub async fn snapshot(&self) -> AppSnapshot {
        let state = self.state.read().await;
        AppSnapshot {
            server: state.server.clone(),
            models: state.registry.artifacts().to_vec(),
            installed_engine_count: 0,
            running_engine_count: 0,
            active_model: None,
        }
    }

    pub async fn refresh_models(&self) {
        let registry = ModelRegistry::discover(&self.config.models.paths);
        let model_count = registry.artifacts().len();
        for artifact in registry.artifacts() {
            let _ = self
                .events
                .send(AppEvent::ModelDiscovered(artifact.id.clone()));
        }
        self.state.write().await.registry = registry;
        let _ = self
            .events
            .send(AppEvent::RegistryRefreshed { model_count });
    }

    pub async fn model_warnings(&self) -> Vec<String> {
        self.state.read().await.registry.warnings().to_vec()
    }

    pub async fn set_server_state(&self, state: ServerState) {
        self.state.write().await.server = state.clone();
        let _ = self.events.send(AppEvent::ServerChanged(state));
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        let _ = self.events.send(AppEvent::Log {
            level,
            message: message.into(),
        });
    }
}
