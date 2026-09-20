use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{CoreError, EffectivePublicAuthMode, PublicAuthMode, PublicAuthStatus, Result};

pub const SUPPORTED_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub runtimes_dir: PathBuf,
    pub runtime_cache_dir: PathBuf,
    pub runtime_selections_file: PathBuf,
    pub settings_file: PathBuf,
    pub settings_lock_file: PathBuf,
    pub model_profiles_file: PathBuf,
    pub model_profiles_lock_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "Scala", "Scala").ok_or(CoreError::PathsUnavailable)?;
        let state_dir = dirs
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_local_dir().join("state"));
        let data_dir = dirs.data_dir().to_path_buf();
        let cache_dir = dirs.cache_dir().to_path_buf();
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            config_file: dirs.config_dir().join("config.toml"),
            runtimes_dir: data_dir.join("runtimes"),
            runtime_selections_file: data_dir.join("runtime-selections.json"),
            settings_file: data_dir.join("settings.json"),
            settings_lock_file: data_dir.join(".settings.lock"),
            model_profiles_file: data_dir.join("model-profiles.json"),
            model_profiles_lock_file: data_dir.join(".model-profiles.lock"),
            data_dir,
            state_dir,
            runtime_cache_dir: cache_dir.join("runtime-packs"),
            cache_dir,
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
            &self.runtimes_dir,
            &self.data_dir.join("models"),
            &self.runtime_cache_dir,
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
    pub link: LinkConfig,
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
            link: LinkConfig::default(),
            engine: BTreeMap::new(),
        }
    }
}

/// Optional local Wayfinder integration; no peer state is persisted here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LinkConfig {
    pub enabled: bool,
}

impl LinkConfig {
    /// Edit only the Link table; runtime/model configuration remains owner-managed.
    pub fn save(&self, paths: &AppPaths) -> std::result::Result<(), String> {
        use std::io::Write;
        let save = || -> std::result::Result<(), Box<dyn std::error::Error>> {
            fs::create_dir_all(&paths.config_dir)?;
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(paths.config_dir.join(".link-config.lock"))?;
            fs2::FileExt::lock_exclusive(&lock)?;
            let text = match fs::read_to_string(&paths.config_file) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            let mut document: toml::Table = toml::from_str(&text)?;
            document.insert("link".into(), toml::Value::try_from(self)?);
            let text = toml::to_string_pretty(&document)?;
            let config: AppConfig = toml::from_str(&text)?;
            config.validate()?;
            let mut file = tempfile::NamedTempFile::new_in(&paths.config_dir)?;
            file.write_all(text.as_bytes())?;
            file.as_file().sync_all()?;
            file.persist(&paths.config_file)?;
            Ok(())
        };
        save().map_err(|e| e.to_string())
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
        if let Some(path) = &mut self.models.model_downloads_path
            && path.is_relative()
        {
            *path = config_dir.join(&*path);
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
    pub auth: PublicAuthMode,
    pub jit: JitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JitConfig {
    pub enabled: bool,
    pub primary_idle_ttl_seconds: u64,
    pub auxiliary_idle_ttl_seconds: u64,
    pub max_idle_auxiliary_backends: usize,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            primary_idle_ttl_seconds: 3600,
            auxiliary_idle_ttl_seconds: 300,
            max_idle_auxiliary_backends: 2,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8742,
            auth: PublicAuthMode::Auto,
            jit: JitConfig::default(),
        }
    }
}

impl ServerConfig {
    pub fn ip_addr(&self) -> Result<IpAddr> {
        self.host.parse().map_err(|_| CoreError::InvalidServerHost {
            host: self.host.clone(),
        })
    }

    pub fn socket_addr(&self) -> Result<std::net::SocketAddr> {
        Ok(std::net::SocketAddr::new(self.ip_addr()?, self.port))
    }

    pub fn is_loopback(&self) -> Result<bool> {
        Ok(self.ip_addr()?.is_loopback())
    }

    pub fn effective_auth_mode(&self) -> Result<EffectivePublicAuthMode> {
        Ok(match self.auth {
            PublicAuthMode::Auto if self.is_loopback()? => EffectivePublicAuthMode::Disabled,
            PublicAuthMode::Auto | PublicAuthMode::Required => EffectivePublicAuthMode::Required,
            PublicAuthMode::Disabled => EffectivePublicAuthMode::Disabled,
        })
    }

    pub fn public_auth_status(&self, active_key_count: usize) -> Result<PublicAuthStatus> {
        let address = self.socket_addr()?;
        let loopback = address.ip().is_loopback();
        let effective_mode = self.effective_auth_mode()?;
        Ok(PublicAuthStatus {
            bind: address.to_string(),
            loopback,
            configured_mode: self.auth,
            effective_mode,
            active_key_count,
            bind_allowed: effective_mode == EffectivePublicAuthMode::Disabled
                || active_key_count > 0,
            insecure_remote: !loopback && self.auth == PublicAuthMode::Disabled,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub paths: Vec<PathBuf>,
    /// Destination root for models downloaded by Scala. When omitted,
    /// downloads land under `<data_dir>/models`, preserving the historical
    /// managed library root. Relative paths resolve against the config
    /// directory, matching `paths`. The effective destination automatically
    /// participates in model discovery and need not be repeated in `paths`.
    pub model_downloads_path: Option<PathBuf>,
}

impl ModelConfig {
    /// Returns the effective managed download root, falling back to the
    /// historical `<data_dir>/models` location when unset.
    pub fn effective_downloads_path(&self, data_dir: &Path) -> PathBuf {
        self.model_downloads_path
            .clone()
            .unwrap_or_else(|| data_dir.join("models"))
    }
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

#[cfg(test)]
mod tests {
    use super::{AppConfig, ServerConfig};
    use crate::{EffectivePublicAuthMode, PublicAuthMode};

    #[test]
    fn model_downloads_path_defaults_to_data_dir_models() {
        let config: AppConfig = toml::from_str(
            r#"
            version = 1

            [server]
            host = "127.0.0.1"
            port = 8742
            "#,
        )
        .expect("parse default config");
        assert!(config.models.model_downloads_path.is_none());
        let data_dir = std::path::Path::new("/var/lib/scala");
        assert_eq!(
            config.models.effective_downloads_path(data_dir).as_path(),
            std::path::Path::new("/var/lib/scala/models"),
        );
    }

    #[test]
    fn model_downloads_path_absolute_is_preserved() {
        let mut config: AppConfig = toml::from_str(
            r#"
            version = 1

            [server]
            host = "127.0.0.1"
            port = 8742

            [models]
            model_downloads_path = "/mnt/ai/models"
            "#,
        )
        .expect("parse configured download path");
        config.resolve_model_paths(std::path::Path::new("/etc/scala"));
        assert_eq!(
            config.models.model_downloads_path.as_deref().unwrap(),
            std::path::Path::new("/mnt/ai/models"),
        );
        assert_eq!(
            config
                .models
                .effective_downloads_path(std::path::Path::new("/var/lib/scala"))
                .as_path(),
            std::path::Path::new("/mnt/ai/models"),
        );
    }

    #[test]
    fn model_downloads_path_relative_resolves_against_config_dir() {
        let mut config: AppConfig = toml::from_str(
            r#"
            version = 1

            [server]
            host = "127.0.0.1"
            port = 8742

            [models]
            model_downloads_path = "downloads"
            paths = ["extra-models"]
            "#,
        )
        .expect("parse relative download path");
        config.resolve_model_paths(std::path::Path::new("/etc/scala"));
        assert_eq!(
            config.models.model_downloads_path.as_deref().unwrap(),
            std::path::Path::new("/etc/scala/downloads"),
        );
        assert_eq!(
            config.models.paths.first().unwrap().as_path(),
            std::path::Path::new("/etc/scala/extra-models"),
        );
    }

    #[test]
    fn existing_version_one_config_defaults_to_safe_auto_auth() {
        let config: AppConfig = toml::from_str(
            r#"
            version = 1

            [server]
            host = "127.0.0.1"
            port = 8742
            "#,
        )
        .expect("parse existing config");

        assert_eq!(config.server.auth, PublicAuthMode::Auto);
        assert_eq!(
            config.server.effective_auth_mode().expect("effective auth"),
            EffectivePublicAuthMode::Disabled
        );
    }

    #[test]
    fn public_auth_policy_matrix_fails_remote_auto_closed() {
        let cases = [
            (
                "127.0.0.1",
                PublicAuthMode::Auto,
                EffectivePublicAuthMode::Disabled,
                true,
                false,
            ),
            (
                "0.0.0.0",
                PublicAuthMode::Auto,
                EffectivePublicAuthMode::Required,
                false,
                false,
            ),
            (
                "127.0.0.1",
                PublicAuthMode::Required,
                EffectivePublicAuthMode::Required,
                false,
                false,
            ),
            (
                "192.168.1.50",
                PublicAuthMode::Disabled,
                EffectivePublicAuthMode::Disabled,
                true,
                true,
            ),
        ];

        for (host, auth, effective, allowed_without_keys, insecure_remote) in cases {
            let server = ServerConfig {
                host: host.to_owned(),
                port: 8742,
                auth,
                ..ServerConfig::default()
            };
            let status = server.public_auth_status(0).expect("auth status");
            assert_eq!(status.effective_mode, effective, "host {host}");
            assert_eq!(status.bind_allowed, allowed_without_keys, "host {host}");
            assert_eq!(status.insecure_remote, insecure_remote, "host {host}");
        }
    }

    #[test]
    fn active_key_allows_required_bind() {
        let server = ServerConfig {
            host: "::".to_owned(),
            port: 8742,
            auth: PublicAuthMode::Auto,
            ..ServerConfig::default()
        };
        assert!(
            server
                .public_auth_status(1)
                .expect("auth status")
                .bind_allowed
        );
    }
}
