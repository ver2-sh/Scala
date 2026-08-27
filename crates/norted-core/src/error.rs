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
    #[error("could not create application directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("server host `{host}` is not a valid IP address")]
    InvalidServerHost { host: String },
}

pub type Result<T> = std::result::Result<T, CoreError>;
