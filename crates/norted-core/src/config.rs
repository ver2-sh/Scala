use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

pub const SUPPORTED_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "Norted", "Norted Server")
            .ok_or(CoreError::PathsUnavailable)?;
        let state_dir = dirs
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_local_dir().join("state"));
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            config_file: dirs.config_dir().join("config.toml"),
            data_dir: dirs.data_dir().to_path_buf(),
            state_dir,
            cache_dir: dirs.cache_dir().to_path_buf(),
            log_dir: dirs.data_local_dir().join("logs"),
        })
    }

    pub fn ensure_required(&self) -> Result<()> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.cache_dir,
            &self.log_dir,
        ] {
            fs::create_dir_all(path).map_err(|source| CoreError::CreateDirectory {
                path: path.clone(),
                source,
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub server: ServerConfig,
    pub models: ModelConfig,
    pub tui: TuiConfig,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub engine: BTreeMap<String, EngineConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: SUPPORTED_CONFIG_VERSION,
            server: ServerConfig::default(),
            models: ModelConfig::default(),
            tui: TuiConfig::default(),
            engine: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_CONFIG_VERSION {
            return Err(CoreError::UnsupportedConfigVersion {
                found: self.version,
                supported: SUPPORTED_CONFIG_VERSION,
            });
        }
        self.server.ip_addr()?;
        Ok(())
    }

    pub fn resolve_model_paths(&mut self, config_dir: &Path) {
        for path in &mut self.models.paths {
            if path.is_relative() {
                *path = config_dir.join(&*path);
            }
        }
    }
}

/// Namespaced adapter configuration. Common settings stay normalized while
/// native arguments and environment variables remain adapter-owned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    pub enabled: bool,
    pub settings: BTreeMap<String, toml::Value>,
    pub native: BTreeMap<String, toml::Value>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8742,
        }
    }
}

impl ServerConfig {
    pub fn ip_addr(&self) -> Result<IpAddr> {
        self.host.parse().map_err(|_| CoreError::InvalidServerHost {
            host: self.host.clone(),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    pub no_color: bool,
    pub unicode: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            no_color: false,
            unicode: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ConfigSource {
    File,
    Defaults,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub path: PathBuf,
    pub source: ConfigSource,
}

impl LoadedConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        if !paths.config_file.exists() {
            return Ok(Self {
                config: AppConfig::default(),
                path: paths.config_file.clone(),
                source: ConfigSource::Defaults,
            });
        }
        let text =
            fs::read_to_string(&paths.config_file).map_err(|source| CoreError::ReadConfig {
                path: paths.config_file.clone(),
                source,
            })?;
        let mut config: AppConfig =
            toml::from_str(&text).map_err(|source| CoreError::ParseConfig {
                path: paths.config_file.clone(),
                source,
            })?;
        config.validate()?;
        config.resolve_model_paths(&paths.config_dir);
        Ok(Self {
            config,
            path: paths.config_file.clone(),
            source: ConfigSource::File,
        })
    }
}
