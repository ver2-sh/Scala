use std::sync::Arc;

use color_eyre::Result;
use norted_core::{
    ApiKeyStore, ApiKeySummary, ApplicationCore, ConfigSource, CreatedApiKey, LoadedConfig,
    PublicAuthStatus, RegistryState,
};
use norted_engine::{ControlClient, ControlStatus, InstallationState, ModelServingCapabilities};
use serde_json::json;

use crate::doctor::{DoctorReport, DoctorStatus};

pub async fn status(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    let observation_error = core
        .refresh_server_state()
        .await
        .err()
        .map(|error| error.to_string());
    let mut snapshot = core.snapshot().await;
    let auth = core
        .config
        .server
        .public_auth_status(ApiKeyStore::new(&core.paths).active_count().await?)?;
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
                "public_auth": auth,
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
        println!("  Public bind:     {}", auth.bind);
        println!("  Auth configured: {}", auth.configured_mode);
        println!("  Auth effective:  {}", auth.effective_mode);
        println!("  Active API keys: {}", auth.active_key_count);
        if auth.insecure_remote {
            println!("  SECURITY:        WARNING: remote authentication is disabled");
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
                "  Active profile:  {}",
                control
                    .backend
                    .model_profile_id
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    .unwrap_or("none")
            );
            println!(
                "  Artifact model:  {}",
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
            println!("  Active profile:  unavailable");
            println!("  Artifact model:  unavailable");
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

pub fn model_info(capabilities: &ModelServingCapabilities, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(capabilities)?);
    } else {
        println!("Model:                {}", capabilities.model_id);
        println!("Format:               {}", capabilities.format);
        if let Some(norted_core::ArtifactNativeIdentity::Ninfer(identity)) =
            &capabilities.native_identity
        {
            println!("Container version:    {}", identity.container_version);
            println!("Native model:         {}", identity.model_id);
            println!("Native weights:       {}", identity.weights_id);
        }
        if let Some(package) = &capabilities.package {
            println!("Package:              {}", package.kind);
            println!(
                "Package manifest:     {} v{}",
                package.manifest_schema, package.manifest_version
            );
            println!("Sharp validated:      {}", yes_no(package.sharp_validated));
            if let Some(lineage) = &package.canonical_lineage_key_short {
                println!("Package lineage:      {lineage}");
            }
        }
        println!(
            "Compatible engines:   {}",
            comma_list(&capabilities.compatible_engine_ids)
        );
        println!(
            "Compatible runtimes:  {}",
            comma_list(
                &capabilities
                    .compatible_installed_runtime_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            )
        );
        println!(
            "Selected runtime:     {}",
            capabilities
                .selected_runtime_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
                .unwrap_or("none")
        );
        println!("Active:               {}", yes_no(capabilities.active));
        println!(
            "Text input/output:    {} / {}",
            yes_no(capabilities.text_input),
            yes_no(capabilities.text_output)
        );
        println!("Responses:            {}", yes_no(capabilities.responses));
        println!(
            "Chat Completions:     {}",
            yes_no(capabilities.chat_completions)
        );
        println!("Streaming:            {}", yes_no(capabilities.streaming));
        println!("Tool calling:         {}", yes_no(capabilities.tools));
        println!("Vision:               {}", yes_no(capabilities.vision));
        println!(
            "Structured output:    {}",
            yes_no(capabilities.structured_output)
        );
    }
    Ok(())
}

pub fn models_search(
    search: &norted_model_library::CatalogSearch,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(search)?);
    } else if search.repositories.is_empty() {
        println!("No relevant Hugging Face model artifacts found.");
    } else {
        for repository in &search.repositories {
            println!("{}  @{}", repository.repository, repository.revision);
            for artifact in &repository.artifacts {
                println!(
                    "  {:<7} {:>12}  {}",
                    artifact.format.as_str().to_ascii_uppercase(),
                    artifact
                        .size_bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "unknown".to_owned()),
                    artifact.filename,
                );
                println!("           ref: {}", artifact.model_ref);
                if !artifact.required_companions.is_empty() {
                    println!(
                        "           companion: {}",
                        artifact.required_companions.join(", ")
                    );
                }
                if let Some(manifest) = &artifact.package_manifest {
                    println!("           package manifest candidate: {manifest}");
                }
                println!("           compatibility: unverified until downloaded and inspected");
            }
        }
    }
    Ok(())
}

pub fn model_operation(
    operation: &str,
    artifact: &norted_core::ModelArtifact,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "model": artifact,
            }))?
        );
    } else {
        println!(
            "{} {} as {} ({})",
            if operation == "import" {
                "Imported"
            } else {
                "Downloaded"
            },
            artifact.display_name,
            artifact.id,
            artifact.path.display(),
        );
        println!("Artifact compatibility remains subject to installed runtime inspection.");
    }
    Ok(())
}

pub fn model_removed(model_id: &norted_core::ModelId, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "operation": "remove", "model_id": model_id }))?
        );
    } else {
        println!("Removed managed model {model_id}.");
    }
    Ok(())
}

fn comma_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub fn auth_status(status: &PublicAuthStatus, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(status)?);
    } else {
        println!("Norted public API authentication");
        println!("  Bind:             {}", status.bind);
        println!(
            "  Exposure:         {}",
            if status.loopback {
                "loopback"
            } else {
                "remote"
            }
        );
        println!("  Configured mode:  {}", status.configured_mode);
        println!("  Effective mode:   {}", status.effective_mode);
        println!("  Active API keys:  {}", status.active_key_count);
        println!(
            "  Bind allowed:     {}",
            if status.bind_allowed { "yes" } else { "no" }
        );
        if status.insecure_remote {
            eprintln!(
                "WARNING: remote HTTP serving has authentication disabled; prompts, outputs, and credentials have no Norted transport protection."
            );
        }
    }
    Ok(())
}

pub fn api_keys(keys: &[ApiKeySummary], json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "data": keys }))?
        );
    } else if keys.is_empty() {
        println!("No Norted API keys exist.");
    } else {
        println!("{:<38} {:<24} {:<20} STATE", "KEY ID", "NAME", "PREFIX");
        for key in keys {
            println!(
                "{:<38} {:<24} {:<20} {}",
                key.key_id,
                key.name,
                format!("{}...", key.display_prefix),
                if key.active { "active" } else { "revoked" }
            );
        }
    }
    Ok(())
}

pub fn api_key_created(created: &CreatedApiKey, json_output: bool) -> Result<()> {
    let key = created.summary();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "key_id": key.key_id,
                "name": key.name,
                "display_prefix": key.display_prefix,
                "created_at": key.created_at,
                "api_key": created.secret(),
            }))?
        );
    } else {
        println!("Created Norted API key {} ({})", key.key_id, key.name);
        println!("The secret is shown once; store it securely:");
        println!("{}", created.secret());
    }
    Ok(())
}

pub fn api_key_revoked(key: &ApiKeySummary, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": "revoke",
                "key": key,
            }))?
        );
    } else {
        println!("Revoked Norted API key {} ({}).", key.key_id, key.name);
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

pub fn doctor(report: &DoctorReport, json_output: bool, verbose: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        for check in report
            .checks
            .iter()
            .filter(|check| verbose || check.status != DoctorStatus::Pass)
        {
            println!("{:<4}  {}", check.status.label(), check.message);
            if let Some(detail) = &check.detail {
                for line in detail.lines() {
                    println!("  {line}");
                }
            }
            for remediation in &check.remediation {
                for line in remediation.lines() {
                    println!("  {line}");
                }
            }
            println!();
        }
        match report.status {
            DoctorStatus::Pass => println!("PASS  No problems found."),
            DoctorStatus::Warning => println!(
                "WARN  Found {} potential problem{}.",
                report.summary.problems,
                if report.summary.problems == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            DoctorStatus::Fail => println!(
                "FAIL  Found {} problem{} ({} failure{}, {} warning{}).",
                report.summary.problems,
                if report.summary.problems == 1 {
                    ""
                } else {
                    "s"
                },
                report.summary.failures,
                if report.summary.failures == 1 {
                    ""
                } else {
                    "s"
                },
                report.summary.warnings,
                if report.summary.warnings == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        }
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
            "{:<52} {:<11} {:<13} {:<10} {:>20}",
            "RUNTIME ID", "ENGINE", "BACKEND", "VERSION", "ACQUISITION"
        );
        for result in &snapshot.results {
            let runtime = &result.entry.available;
            let installed = if result.installed { " installed" } else { "" };
            println!(
                "{:<52} {:<11} {:<13} {:<10} {:>20}{}",
                runtime.runtime_id,
                runtime.identity.engine_id,
                runtime.identity.accelerator,
                runtime.identity.version,
                runtime
                    .download_size_bytes()
                    .map(|bytes| format!("upstream binary {}", format_bytes(bytes)))
                    .unwrap_or_else(|| "source build".to_owned()),
                installed
            );
            println!(
                "  {} {} / {}  {:?}",
                runtime.identity.platform,
                runtime.identity.architecture,
                runtime.identity.variant,
                result.entry.compatibility
            );
            for note in &runtime.requirements.advisories {
                println!("  Note: {note}");
            }
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

pub fn model_profiles(
    operation: &str,
    state: &norted_core::ModelProfilesState,
    selected: Option<&norted_core::ModelProfileId>,
    models: &[norted_core::ModelArtifact],
    json_output: bool,
) -> Result<()> {
    if let Some(id) = selected {
        let profile = state
            .profiles
            .get(id)
            .ok_or_else(|| color_eyre::eyre::eyre!("Model Profile `{id}` does not exist"))?;
        let model = models.iter().find(|model| model.id == profile.model_id);
        let resolved = norted_core::ResolvedSettings::default();
        return model_profile(operation, profile, model, &resolved, json_output);
    }
    if json_output {
        let rows = state
            .profiles
            .values()
            .map(|profile| {
                json!({
                    "profile": profile,
                    "content_hash": profile.content_hash(),
                    "model_available": models.iter().any(|model| model.id == profile.model_id),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "state_version": state.version,
                "data": rows,
            }))?
        );
    } else if state.profiles.is_empty() {
        println!("No Model Profiles exist. Create one from a discovered model artifact.");
    } else {
        println!(
            "{:<30} {:<12} {:<36} STATE",
            "MODEL PROFILE", "ENGINE", "MODEL ID"
        );
        for profile in state.profiles.values() {
            let available = models.iter().any(|model| model.id == profile.model_id);
            println!(
                "{:<30} {:<12} {:<36} {}",
                profile.id,
                profile.engine_id,
                truncate(&profile.model_id.to_string(), 36),
                if available { "available" } else { "missing" }
            );
        }
    }
    Ok(())
}

pub fn model_profile(
    operation: &str,
    profile: &norted_core::ModelProfile,
    model: Option<&norted_core::ModelArtifact>,
    resolved: &norted_core::ResolvedSettings,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "profile": profile,
                "content_hash": profile.content_hash(),
                "model": model,
                "model_available": model.is_some(),
                "effective_settings": resolved,
            }))?
        );
        return Ok(());
    }
    println!("Model Profile: {} ({})", profile.display_name, profile.id);
    println!("  Model ID:       {}", profile.model_id);
    println!(
        "  Model state:    {}",
        if model.is_some() {
            "available"
        } else {
            "missing"
        }
    );
    if let Some(model) = model {
        println!("  Artifact:       {}", model.path.display());
        println!("  Format:         {}", model.format);
    }
    println!("  Engine:         {}", profile.engine_id);
    println!("  Content SHA:    {}", profile.content_hash());
    if profile.overrides.is_empty() {
        println!("  Overrides:      none (Global + engine defaults only)");
    } else {
        println!("  Overrides:");
        for (id, value) in profile.overrides.iter() {
            println!("    {id:<36} {value}");
        }
    }
    if !resolved.effective.is_empty() {
        println!("  Effective settings:");
        for (id, setting) in &resolved.effective {
            println!("    {id:<36} {:<18} {}", setting.value, setting.source);
        }
    }
    Ok(())
}

pub fn model_profile_deleted(
    profile_id: &norted_core::ModelProfileId,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": "delete",
                "model_profile_id": profile_id,
                "deleted": true,
            }))?
        );
    } else {
        println!("Deleted Model Profile `{profile_id}`.");
    }
    Ok(())
}

pub fn model_profile_compatibility(
    profile: &norted_core::ModelProfile,
    model: &norted_core::ModelArtifact,
    selection: &norted_core::RuntimeSelection,
    schema: &norted_core::SettingsSchema,
    resolved: &norted_core::ResolvedSettings,
    compatibility: &norted_core::RuntimeCompatibility,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "model_profile": profile,
                "model_profile_content_hash": profile.content_hash(),
                "artifact_model_id": model.id,
                "artifact_format": model.format,
                "runtime_id": selection.runtime.manifest.runtime_id,
                "settings_schema": schema,
                "effective_settings": resolved,
                "compatibility": compatibility,
            }))?
        );
    } else {
        println!("Model Profile compatibility");
        println!("  Profile:    {} ({})", profile.display_name, profile.id);
        println!("  Model:      {} ({})", model.display_name, model.format);
        println!("  Engine:     {}", profile.engine_id);
        println!("  Runtime:    {}", selection.runtime.manifest.runtime_id);
        println!("  Status:     {compatibility:?}");
    }
    Ok(())
}

pub fn settings_defaults(
    operation: &str,
    scope: &str,
    state: &norted_core::SettingsState,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "scope": scope,
                "state": state,
            }))?
        );
        return Ok(());
    }
    println!("Settings {operation}: {scope}");
    let patch = if scope == "global" {
        Some(&state.global_defaults)
    } else {
        scope
            .strip_prefix("engine:")
            .and_then(|engine| state.engine_defaults.get(engine))
    };
    match patch {
        Some(patch) if !patch.is_empty() => {
            for (id, value) in patch.iter() {
                println!("  {id:<38} {value}");
            }
        }
        _ => println!("  No configured defaults; values inherit the runtime/upstream default."),
    }
    println!(
        "Server operational settings apply to the running TUI; model settings apply on the next Model Profile load."
    );
    Ok(())
}
