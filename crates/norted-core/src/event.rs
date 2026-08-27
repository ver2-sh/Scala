use serde::{Deserialize, Serialize};

use crate::{ModelId, ServerState};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    ModelDiscovered(ModelId),
    RegistryRefreshed { model_count: usize },
    ServerChanged(ServerState),
    Log { level: LogLevel, message: String },
}
