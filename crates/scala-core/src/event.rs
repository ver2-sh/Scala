use serde::{Deserialize, Serialize};

use crate::{RegistryState, ServerState};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    RegistryChanged(RegistryState),
    ServerChanged(ServerState),
    Log { level: LogLevel, message: String },
}
