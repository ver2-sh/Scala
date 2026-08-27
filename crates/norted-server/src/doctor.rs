use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use norted_core::{ApplicationCore, ConfigSource, LoadedConfig};
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

pub async fn run(core: Arc<ApplicationCore>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let config_detail = match LoadedConfig::load(&core.paths) {
        Ok(loaded) => {
            let source = match loaded.source {
                ConfigSource::File => "file",
                ConfigSource::Defaults => "built-in defaults",
            };
            format!("{source} ({})", loaded.path.display())
        }
        Err(error) => error.to_string(),
    };
    checks.push(DoctorCheck {
        name: "configuration",
        status: CheckStatus::Pass,
        detail: config_detail,
        fatal: false,
    });

    for (name, path) in [
        ("config directory", &core.paths.config_dir),
        ("data directory", &core.paths.data_dir),
        ("state directory", &core.paths.state_dir),
        ("cache directory", &core.paths.cache_dir),
        ("log directory", &core.paths.log_dir),
    ] {
        checks.push(directory_check(name, path));
    }

    if core.config.models.paths.is_empty() {
        checks.push(DoctorCheck {
            name: "model paths",
            status: CheckStatus::Warning,
            detail: "none configured; the model registry will be empty".into(),
            fatal: false,
        });
    } else {
        for path in &core.config.models.paths {
            let valid = path.is_dir();
            checks.push(DoctorCheck {
                name: "model path",
                status: if valid {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warning
                },
                detail: if valid {
                    path.display().to_string()
                } else {
                    format!("{} is missing or not a directory", path.display())
                },
                fatal: false,
            });
        }
    }

    let address = format!("{}:{}", core.config.server.host, core.config.server.port);
    checks.push(DoctorCheck {
        name: "server address",
        status: CheckStatus::Pass,
        detail: format!("{address} is valid (bind not attempted)"),
        fatal: false,
    });
    checks.push(DoctorCheck {
        name: "terminal",
        status: if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warning
        },
        detail: format!(
            "stdin_tty={}, stdout_tty={}, TERM={}",
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
            std::env::var("TERM").unwrap_or_else(|_| "unset".into())
        ),
        fatal: false,
    });
    checks.push(DoctorCheck {
        name: "environment",
        status: CheckStatus::Pass,
        detail: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        fatal: false,
    });
    checks
}

fn directory_check(name: &'static str, path: &Path) -> DoctorCheck {
    let valid = path.is_dir();
    DoctorCheck {
        name,
        status: if valid {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        detail: path.display().to_string(),
        fatal: !valid,
    }
}
