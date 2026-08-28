use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("could not determine platform application directories")]
    PathsUnavailable,
    #[error("could not read configuration at {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("configuration at {path} is not valid TOML: {source}")]
    ParseConfig {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error(
        "configuration schema version {found} is not supported; this build supports version {supported}"
    )]
    UnsupportedConfigVersion { found: u32, supported: u32 },
    #[error("could not create application directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("server host `{host}` is not a valid IP address")]
    InvalidServerHost { host: String },
    #[error("control listener must use a loopback address, found `{address}`")]
    InvalidControlAddress { address: std::net::SocketAddr },
    #[error("runtime state operation failed for {path}: {source}")]
    RuntimeState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("runtime descriptor at {path} is invalid: {source}")]
    InvalidRuntimeDescriptor {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("blocking task failed: {0}")]
    BlockingTask(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
