use std::sync::Arc;

use color_eyre::Result;
use norted_core::{ApplicationCore, ConfigSource, LoadedConfig, RegistryState};
use norted_engine::EngineRegistry;
use serde_json::json;

use crate::doctor::DoctorCheck;

pub async fn status(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    core.refresh_server_state().await;
    let snapshot = core.snapshot().await;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        println!("Norted Server");
        println!("  Server:          {}", snapshot.server.label());
        if let Some(endpoint) = snapshot.server.endpoint() {
            println!("  Endpoint:        {endpoint}");
        }
        match snapshot.registry_state {
            RegistryState::Ready | RegistryState::ReadyWithWarnings { .. } => {
                println!("  Models:          {} discovered", snapshot.models.len());
            }
            state => println!("  Models:          {}", state.label()),
        }
        println!(
            "  Engines:         {} installed, {} running",
            snapshot.installed_engine_count, snapshot.running_engine_count
        );
        println!(
            "  Active model:    {}",
            snapshot.active_model.as_deref().unwrap_or("none")
        );
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
        println!("{:<28} {:<7} {:>12}  PATH", "MODEL", "FORMAT", "SIZE");
        for model in snapshot.models {
            println!(
                "{:<28} {:<7} {:>12}  {}",
                model.display_name,
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

pub fn engines(json_output: bool) -> Result<()> {
    let registry = EngineRegistry::default();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "engines": [], "count": registry.len() }))?
        );
    } else {
        println!("No engine adapters are installed.");
        println!("No inference engine support is implemented in this bootstrap.");
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
