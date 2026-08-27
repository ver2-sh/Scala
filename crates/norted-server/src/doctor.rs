use std::fs::{self, OpenOptions};
use std::io::IsTerminal;
use std::path::Path;

use norted_core::{AppConfig, AppPaths, SUPPORTED_CONFIG_VERSION};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warning,
    Fail,
}

impl CheckStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warning => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    #[serde(skip)]
    pub fatal: bool,
}

pub fn run() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let paths = match AppPaths::discover() {
        Ok(paths) => {
            checks.push(pass(
                "application paths",
                format!("configuration at {}", paths.config_file.display()),
            ));
            paths
        }
        Err(error) => {
            checks.push(fail("application paths", error.to_string()));
            add_environment_checks(&mut checks);
            return checks;
        }
    };

    for (name, path) in [
        ("config directory", &paths.config_dir),
        ("data directory", &paths.data_dir),
        ("state directory", &paths.state_dir),
        ("cache directory", &paths.cache_dir),
        ("log directory", &paths.log_dir),
    ] {
        checks.push(directory_check(name, path));
    }

    let mut usable_config = None;
    if !paths.config_file.exists() {
        checks.push(pass(
            "configuration file",
            format!(
                "{} is absent; built-in defaults apply",
                paths.config_file.display()
            ),
        ));
        checks.push(pass("TOML syntax", "built-in defaults"));
        checks.push(pass(
            "configuration schema",
            format!("version {SUPPORTED_CONFIG_VERSION}"),
        ));
        usable_config = Some(AppConfig::default());
    } else {
        match fs::read_to_string(&paths.config_file) {
            Err(error) => checks.push(fail(
                "configuration file",
                format!("{}: {error}", paths.config_file.display()),
            )),
            Ok(text) => {
                checks.push(pass(
                    "configuration file",
                    format!("readable at {}", paths.config_file.display()),
                ));
                match toml::from_str::<toml::Value>(&text) {
                    Err(error) => checks.push(fail("TOML syntax", error.to_string())),
                    Ok(_) => {
                        checks.push(pass("TOML syntax", "valid TOML"));
                        match toml::from_str::<AppConfig>(&text) {
                            Err(error) => checks.push(fail(
                                "configuration schema",
                                format!("invalid structured configuration: {error}"),
                            )),
                            Ok(mut config) => {
                                if config.version == SUPPORTED_CONFIG_VERSION {
                                    checks.push(pass(
                                        "configuration schema",
                                        format!("supported version {}", config.version),
                                    ));
                                    config.resolve_model_paths(&paths.config_dir);
                                    usable_config = Some(config);
                                } else {
                                    checks.push(fail(
                                        "configuration schema",
                                        format!(
                                            "version {} is unsupported; expected {}",
                                            config.version, SUPPORTED_CONFIG_VERSION
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(config) = usable_config {
        match config.server.ip_addr() {
            Ok(address) => checks.push(pass(
                "server address",
                format!(
                    "{address}:{} is valid (bind not attempted)",
                    config.server.port
                ),
            )),
            Err(error) => checks.push(fail("server address", error.to_string())),
        }
        if config.models.paths.is_empty() {
            checks.push(warning(
                "model paths",
                "none configured; the model registry will be empty",
            ));
        } else {
            for path in &config.models.paths {
                checks.push(if path.is_dir() {
                    pass("model path", path.display().to_string())
                } else {
                    warning(
                        "model path",
                        format!("{} is missing or not a directory", path.display()),
                    )
                });
            }
        }
    }

    add_environment_checks(&mut checks);
    checks
}

fn directory_check(name: &'static str, path: &Path) -> DoctorCheck {
    if let Err(error) = fs::create_dir_all(path) {
        return fail(name, format!("{}: {error}", path.display()));
    }
    let probe = path.join(format!(".norted-doctor-{}.tmp", std::process::id()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(probe);
            pass(name, format!("{} is accessible", path.display()))
        }
        Err(error) => fail(name, format!("{} is not writable: {error}", path.display())),
    }
}

fn add_environment_checks(checks: &mut Vec<DoctorCheck>) {
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    checks.push(if stdin_tty && stdout_tty {
        pass(
            "terminal",
            format!(
                "interactive; TERM={}",
                std::env::var("TERM").unwrap_or_else(|_| "unset".into())
            ),
        )
    } else {
        warning(
            "terminal",
            format!("stdin_tty={stdin_tty}, stdout_tty={stdout_tty}; CLI remains available"),
        )
    });
    checks.push(pass(
        "environment",
        format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
    ));
}

fn pass(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Pass,
        detail: detail.into(),
        fatal: false,
    }
}

fn warning(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Warning,
        detail: detail.into(),
        fatal: false,
    }
}

fn fail(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Fail,
        detail: detail.into(),
        fatal: true,
    }
}
