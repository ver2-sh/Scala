use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock, broadcast};

use crate::{
    AppConfig, AppEvent, AppPaths, CoreError, LoadedConfig, LogLevel, ModelArtifact, ModelRegistry,
    Result, observe_runtime,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeStatus {
    Stopped,
    Starting,
    Running { process_id: u32 },
    Stopping,
    Failed { message: String },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RegistryState {
    NotScanned,
    Scanning,
    Ready,
    ReadyWithWarnings { warning_count: usize },
    Failed { message: String },
}

impl RegistryState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotScanned => "Not scanned",
            Self::Scanning => "Scanning",
            Self::Ready => "Ready",
            Self::ReadyWithWarnings { .. } => "Ready with warnings",
            Self::Failed { .. } => "Failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub server: ServerState,
    pub registry_state: RegistryState,
    pub models: Vec<ModelArtifact>,
    pub registry_warnings: Vec<String>,
    pub installed_engine_count: usize,
    pub running_engine_count: usize,
    pub active_model: Option<String>,
}

#[derive(Debug)]
struct MutableState {
    server: ServerState,
    registry_state: RegistryState,
    registry: ModelRegistry,
}

#[derive(Debug)]
pub struct ApplicationCore {
    pub config: AppConfig,
    pub config_path: std::path::PathBuf,
    pub paths: AppPaths,
    state: RwLock<MutableState>,
    registry_changed: Notify,
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
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            config,
            config_path,
            paths,
            state: RwLock::new(MutableState {
                server: ServerState::Stopped,
                registry_state: RegistryState::NotScanned,
                registry: ModelRegistry::default(),
            }),
            registry_changed: Notify::new(),
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
            registry_state: state.registry_state.clone(),
            models: state.registry.artifacts().to_vec(),
            registry_warnings: state.registry.warnings().to_vec(),
            installed_engine_count: 0,
            running_engine_count: 0,
            active_model: None,
        }
    }

    pub async fn start_model_discovery(self: &Arc<Self>) {
        if self.begin_model_discovery(false).await {
            let core = Arc::clone(self);
            tokio::spawn(async move {
                let _ = core.complete_model_discovery().await;
            });
        }
    }

    pub async fn ensure_model_discovery(&self) -> Result<()> {
        loop {
            let mut changed = std::pin::pin!(self.registry_changed.notified());
            changed.as_mut().enable();
            let state = self.state.read().await.registry_state.clone();
            match state {
                RegistryState::NotScanned => {
                    if self.begin_model_discovery(false).await {
                        return self.complete_model_discovery().await;
                    }
                }
                RegistryState::Scanning => changed.await,
                RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => return Ok(()),
                RegistryState::Failed { message } => {
                    return Err(CoreError::BlockingTask(message));
                }
            }
        }
    }

    pub async fn refresh_models(&self) -> Result<()> {
        if self.begin_model_discovery(true).await {
            self.complete_model_discovery().await
        } else {
            self.ensure_model_discovery().await
        }
    }

    pub async fn model_warnings(&self) -> Vec<String> {
        self.state.read().await.registry.warnings().to_vec()
    }

    pub async fn set_server_state(&self, state: ServerState) {
        self.state.write().await.server = state.clone();
        let _ = self.events.send(AppEvent::ServerChanged(state));
    }

    pub async fn refresh_server_state(&self) {
        let observed = observe_runtime(&self.paths).await;
        let mut state = self.state.write().await;
        if state.server != observed {
            state.server = observed.clone();
            let _ = self.events.send(AppEvent::ServerChanged(observed));
        }
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        let _ = self.events.send(AppEvent::Log {
            level,
            message: message.into(),
        });
    }

    async fn begin_model_discovery(&self, force: bool) -> bool {
        let mut state = self.state.write().await;
        if matches!(state.registry_state, RegistryState::Scanning)
            || (!force && !matches!(state.registry_state, RegistryState::NotScanned))
        {
            return false;
        }
        state.registry_state = RegistryState::Scanning;
        drop(state);
        let _ = self
            .events
            .send(AppEvent::RegistryChanged(RegistryState::Scanning));
        true
    }

    async fn complete_model_discovery(&self) -> Result<()> {
        match discover_models(self.config.models.paths.clone()).await {
            Ok(registry) => {
                let registry_state = if registry.warnings().is_empty() {
                    RegistryState::Ready
                } else {
                    RegistryState::ReadyWithWarnings {
                        warning_count: registry.warnings().len(),
                    }
                };
                let mut state = self.state.write().await;
                state.registry = registry;
                state.registry_state = registry_state.clone();
                drop(state);
                self.registry_changed.notify_waiters();
                let _ = self.events.send(AppEvent::RegistryChanged(registry_state));
                Ok(())
            }
            Err(error) => {
                let registry_state = RegistryState::Failed {
                    message: error.to_string(),
                };
                self.state.write().await.registry_state = registry_state.clone();
                self.registry_changed.notify_waiters();
                let _ = self.events.send(AppEvent::RegistryChanged(registry_state));
                Err(error)
            }
        }
    }
}

async fn discover_models(paths: Vec<std::path::PathBuf>) -> Result<ModelRegistry> {
    tokio::task::spawn_blocking(move || ModelRegistry::discover(&paths))
        .await
        .map_err(|error| CoreError::BlockingTask(error.to_string()))
}
