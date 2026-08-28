use std::sync::Arc;

use color_eyre::Result;
use norted_core::{ApplicationCore, ConfigSource, LoadedConfig, RegistryState};
use norted_engine::{ControlClient, ControlStatus, InstallationState};
use serde_json::json;

use crate::doctor::DoctorCheck;

pub async fn status(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
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
                "  Adapters:        {} registered, {} external binaries, {} backend running",
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
            if let Some(runtime) = &control.backend.runtime_id {
                println!("  Active runtime:  {runtime}");
            }
            if let Some(version) = &control.backend.runtime_version {
                println!("  Runtime version: {version}");
            }
            if let Some(variant) = &control.backend.runtime_variant {
                println!("  Runtime variant: {variant}");
            }
            if let Some(digest) = &control.backend.runtime_executable_sha256 {
                println!("  Runtime SHA-256: {digest}");
            }
        } else {
            println!("  Adapters:        unavailable (no private control observation)");
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
        if let Some(runtime) = &status.backend.runtime_id {
            println!("  Runtime: {runtime}");
        }
        if let Some(version) = &status.backend.runtime_version {
            println!(
                "  Version: {version}{}",
                status
                    .backend
                    .runtime_variant
                    .as_deref()
                    .map(|variant| format!(" / {variant}"))
                    .unwrap_or_default()
            );
        }
        for event in status.recent_events.iter().filter(|event| {
            matches!(
                event.level,
                norted_engine::RuntimeNoticeLevel::Warning
                    | norted_engine::RuntimeNoticeLevel::Error
            )
        }) {
            eprintln!("  {:?}: {}", event.level, event.message);
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

pub fn runtimes_list(
    snapshot: &norted_engine::RuntimeListSnapshot,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(snapshot)?);
        return Ok(());
    }
    if snapshot.installed.is_empty() {
        println!("No runtime packs are installed or configured.");
    } else {
        println!(
            "{:<52} {:<12} {:<14} {:<12} VERSION",
            "RUNTIME ID", "ENGINE", "BACKEND", "STATE"
        );
        for status in &snapshot.installed {
            let runtime = &status.runtime.manifest;
            let state = if status.selected_for.is_empty() {
                format!("{:?}", status.compatibility)
            } else {
                format!("selected: {}", status.selected_for.join(", "))
            };
            println!(
                "{:<52} {:<12} {:<14} {:<12} {}",
                runtime.runtime_id,
                runtime.identity.engine_id,
                runtime.identity.accelerator,
                truncate(&state, 12),
                runtime.identity.version
            );
        }
    }
    for warning in &snapshot.warnings {
        eprintln!("Warning: {warning}");
    }
    Ok(())
}

pub fn runtimes_search(
    snapshot: &norted_engine::RuntimeSearchSnapshot,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(snapshot)?);
        return Ok(());
    }
    if snapshot.results.is_empty() {
        println!("No upstream runtime packs matched the query.");
    } else {
        println!(
            "{:<52} {:<11} {:<13} {:<10} {:>10}",
            "RUNTIME ID", "ENGINE", "BACKEND", "VERSION", "DOWNLOAD"
        );
        for result in &snapshot.results {
            let runtime = &result.entry.available;
            let installed = if result.installed { " installed" } else { "" };
            println!(
                "{:<52} {:<11} {:<13} {:<10} {:>10}{}",
                runtime.runtime_id,
                runtime.identity.engine_id,
                runtime.identity.accelerator,
                runtime.identity.version,
                format_bytes(runtime.download_size_bytes()),
                installed
            );
            println!(
                "  {} {} / {}  {:?}",
                runtime.identity.platform,
                runtime.identity.architecture,
                runtime.identity.variant,
                result.entry.compatibility
            );
        }
    }
    for error in &snapshot.provider_errors {
        eprintln!(
            "Warning: {}: {}{}",
            error.provider_id,
            error.message,
            if error.using_stale_cache {
                " (showing stale cache)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub fn runtime_info(value: &serde_json::Value, _json_output: bool) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn runtime_operation(
    operation: &str,
    runtime: &norted_core::InstalledRuntime,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "runtime": runtime,
            }))?
        );
    } else {
        println!("Runtime {operation} complete.");
        println!("  ID:       {}", runtime.manifest.runtime_id);
        println!("  Engine:   {}", runtime.manifest.identity.engine_id);
        println!("  Version:  {}", runtime.manifest.identity.version);
        println!("  Variant:  {}", runtime.manifest.identity.variant);
        println!("  Binary:   {}", runtime.entrypoint_path().display());
        println!("  SHA-256:  {}", runtime.manifest.entrypoint_sha256);
    }
    Ok(())
}

pub fn runtime_removed(runtime_id: &norted_core::RuntimeId, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": "remove",
                "runtime_id": runtime_id,
                "removed": true,
            }))?
        );
    } else {
        println!("Removed runtime {runtime_id}.");
    }
    Ok(())
}

pub fn runtime_selections(
    operation: &str,
    selections: &norted_core::RuntimeSelections,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "selections": selections,
            }))?
        );
    } else {
        println!("Runtime selection {operation} complete.");
        for (format, runtime) in &selections.format_defaults {
            println!(
                "  {} default: {runtime}",
                format.as_str().to_ascii_uppercase()
            );
        }
        for (model, runtime) in &selections.model_overrides {
            println!("  Model {model}: {runtime}");
        }
    }
    Ok(())
}

pub fn runtime_updates(
    checks: &[norted_engine::RuntimeUpdateCheck],
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "data": checks }))?
        );
    } else if checks.is_empty() {
        println!("No managed runtimes are installed.");
    } else {
        for check in checks {
            println!(
                "  {} {}: {:?}",
                check.runtime.manifest.identity.engine_id,
                check.runtime.manifest.identity.version,
                check.state
            );
        }
    }
    Ok(())
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_owned()
    } else {
        value
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}
