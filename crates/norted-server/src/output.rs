use std::sync::Arc;

use color_eyre::Result;
use norted_core::{ApplicationCore, ConfigSource, LoadedConfig, RegistryState};
use norted_engine::{ControlClient, ControlStatus, InstallationState};
use serde_json::json;

use crate::doctor::DoctorCheck;

pub async fn status(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    core.ensure_model_discovery().await?;
    let observation_error = core
        .refresh_server_state()
        .await
        .err()
        .map(|error| error.to_string());
    let mut snapshot = core.snapshot().await;
    if let Some(message) = observation_error {
        snapshot.server = norted_core::ServerState::Unknown { message };
    }
    let control = match ControlClient::discover(&core.paths).await {
        Ok(client) => match client.status().await {
            Ok(status) => Some(status),
            Err(error) => {
                tracing::warn!(%error, "private control status is unavailable");
                None
            }
        },
        Err(_) => None,
    };
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "server": snapshot.server,
                "registry_state": snapshot.registry_state,
                "models": snapshot.models,
                "registry_warnings": snapshot.registry_warnings,
                "control": control,
            }))?
        );
    } else {
        println!("Norted Server");
        println!("  Server:          {}", snapshot.server.label());
        if let norted_core::ServerState::Unknown { message } = &snapshot.server {
            println!("  Observation:     unavailable: {message}");
        }
        if let Some(endpoint) = snapshot.server.endpoint() {
            println!("  Endpoint:        {endpoint}");
        }
        match snapshot.registry_state {
            RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => {
                println!("  Models:          {} discovered", snapshot.models.len());
            }
            state => println!("  Models:          {}", state.label()),
        }
        if let Some(control) = &control {
            println!(
                "  Engines:         {} available, {} installed, {} running",
                control.available_engine_count,
                control.installed_engine_count,
                control.running_engine_count
            );
            println!("  Backend:         {:?}", control.backend.lifecycle);
            println!(
                "  Active model:    {}",
                control
                    .backend
                    .model_id
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    .unwrap_or("none")
            );
            if let Some(engine) = &control.backend.engine_id {
                println!("  Active engine:   {engine}");
            }
        } else {
            println!("  Engines:         unavailable (no private control observation)");
            println!("  Active model:    unavailable");
        }
    }
    Ok(())
}

pub async fn models(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    core.ensure_model_discovery().await?;
    let snapshot = core.snapshot().await;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "data": snapshot.models,
                "warnings": snapshot.registry_warnings,
            }))?
        );
    } else if snapshot.models.is_empty() {
        println!("No model artifacts discovered.");
        if core.config.models.paths.is_empty() {
            println!(
                "Configure one or more search directories in {} under [models].paths.",
                core.config_path.display()
            );
        }
    } else {
        println!("{:<36} {:<7} {:>12}  PATH", "MODEL ID", "FORMAT", "SIZE");
        for model in snapshot.models {
            println!(
                "{:<36} {:<7} {:>12}  {}",
                model.id,
                model.format.as_str(),
                format_bytes(model.size_bytes),
                model.path.display()
            );
        }
    }
    if !json_output {
        for warning in &snapshot.registry_warnings {
            eprintln!("Warning: {warning}");
        }
    }
    Ok(())
}

pub async fn engines(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    let (status, source) = match ControlClient::discover(&core.paths).await {
        Ok(client) => match client.status().await {
            Ok(status) => (status, "running_server"),
            Err(_) => (
                crate::composition::runtime_manager(core)
                    .await?
                    .status()
                    .await,
                "local_probe",
            ),
        },
        Err(_) => (
            crate::composition::runtime_manager(core)
                .await?
                .status()
                .await,
            "local_probe",
        ),
    };
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": source,
                "engines": status.engines,
                "count": status.available_engine_count,
            }))?
        );
    } else {
        println!("Inference engines ({source})");
        for engine in &status.engines {
            let (state, installation) = match &engine.probe.installation {
                InstallationState::NotInstalled => ("not installed", None),
                InstallationState::Invalid { .. } => ("invalid", None),
                InstallationState::Installed { installation } => (
                    if engine.probe.healthy {
                        "available"
                    } else {
                        "unhealthy"
                    },
                    Some(installation),
                ),
            };
            println!("  {}: {state}", engine.identity.display_name);
            println!("    Health: {}", engine.probe.detail);
            if let Some(installation) = installation {
                println!("    Binary: {}", installation.binary_path.display());
                println!(
                    "    Version: {}",
                    installation.engine.version.as_deref().unwrap_or("unknown")
                );
                println!(
                    "    Revision: {}",
                    installation.engine.revision.as_deref().unwrap_or("unknown")
                );
                println!("    Source: external configured binary");
            }
        }
    }
    Ok(())
}

pub fn control_operation(operation: &str, status: &ControlStatus, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "status": status,
            }))?
        );
    } else {
        println!(
            "{} complete.",
            if operation == "load" {
                "Load"
            } else {
                "Unload"
            }
        );
        println!("  Backend: {:?}", status.backend.lifecycle);
        println!(
            "  Model:   {}",
            status
                .backend
                .model_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
                .unwrap_or("none")
        );
        if let Some(engine) = &status.backend.engine_id {
            println!("  Engine:  {engine}");
        }
    }
    Ok(())
}

pub fn config(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    let loaded = LoadedConfig::load(&core.paths)?;
    if json_output {
        let source = match loaded.source {
            ConfigSource::File => "file",
            ConfigSource::Defaults => "defaults",
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": loaded.path,
                "source": source,
                "config": loaded.config,
            }))?
        );
    } else {
        let source = match loaded.source {
            ConfigSource::File => "file",
            ConfigSource::Defaults => "built-in defaults; no file written",
        };
        println!("# path: {}", loaded.path.display());
        println!("# source: {source}");
        print!("{}", toml::to_string_pretty(&loaded.config)?);
    }
    Ok(())
}

pub fn doctor(checks: &[DoctorCheck], json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(checks)?);
    } else {
        println!("Norted Server doctor");
        for check in checks {
            println!(
                "  {:<4}  {:<20} {}",
                check.status.label(),
                check.name,
                check.detail
            );
        }
        let failures = checks.iter().filter(|check| check.fatal).count();
        let warnings = checks
            .iter()
            .filter(|check| matches!(check.status, crate::doctor::CheckStatus::Warning))
            .count();
        println!();
        println!("{failures} fatal problem(s), {warnings} warning(s)");
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else {
        format!("{:.1} KiB", bytes / 1024.0)
    }
}
