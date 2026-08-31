use std::sync::Arc;

use color_eyre::Result;
use norted_core::{
    ApiKeyStore, ApiKeySummary, ApplicationCore, ConfigSource, CreatedApiKey, LoadedConfig,
    PublicAuthStatus, RegistryState,
};
use norted_engine::{ControlClient, ControlStatus, InstallationState, ModelServingCapabilities};
use serde_json::json;

use crate::doctor::DoctorCheck;

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
            println!("Package validation:   {:?}", package.validation_status);
            println!(
                "Package policy:       {}",
                package.runtime_policy.as_deref().unwrap_or("n/a")
            );
            println!(
                "Package runtime:      {}",
                package
                    .runtime_package_capability
                    .as_ref()
                    .map(|state| format!("{state:?}"))
                    .as_deref()
                    .unwrap_or("unknown")
            );
            println!(
                "Sharp:                required={} validated={} application={}",
                yes_no(package.sharp_required),
                yes_no(package.sharp_validated),
                package
                    .sharp_application_capability
                    .as_ref()
                    .map(|state| format!("{state:?}"))
                    .as_deref()
                    .unwrap_or("unknown")
            );
            if let Some(lineage) = &package.canonical_lineage_key_short {
                println!("Package lineage:      {lineage}");
            }
            if !package.ninfer_benchmark_profiles.is_empty() {
                println!(
                    "Package profiles:     {}",
                    comma_list(&package.ninfer_benchmark_profiles)
                );
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

pub fn profiles(
    operation: &str,
    state: &norted_core::LoadProfilesState,
    selected: Option<&norted_core::LoadProfileName>,
    json_output: bool,
) -> Result<()> {
    if json_output {
        let profile = selected.and_then(|name| {
            state.profiles.get(name).map(|value| {
                let serve = value.effective_serve_profile(name);
                let effective_requirements = serve.effective_requirements();
                json!({
                    "name": name,
                    "serve_profile": serve,
                    "effective_requirements": effective_requirements,
                })
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "state_version": state.version,
                "profile": profile,
                "profiles": state.profiles,
                "model_assignments": state.model_assignments,
                "builder_profile_assignments": state.builder_profile_assignments,
                "raw_profile_models": state.raw_profile_models,
            }))?
        );
        return Ok(());
    }
    if let Some(name) = selected {
        let profile = &state.profiles[name];
        let serve = profile.effective_serve_profile(name);
        println!("Serve Profile {} ({})", serve.display_name, serve.id);
        println!("  Source:             {}", serve.source);
        println!(
            "  Access:             {}",
            if serve.read_only {
                "read-only"
            } else {
                "mutable"
            }
        );
        if let Some(description) = &serve.description {
            println!("  Description:        {description}");
        }
        println!(
            "  Applicability:      {}{}{}",
            if serve.applicability.artifact_formats.is_empty() {
                "any format".to_owned()
            } else {
                serve
                    .applicability
                    .artifact_formats
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            serve
                .applicability
                .architecture
                .as_ref()
                .map(|value| format!(" · architecture {value}"))
                .unwrap_or_default(),
            serve
                .applicability
                .family
                .as_ref()
                .map(|value| format!(" · family {value}"))
                .unwrap_or_default(),
        );
        println!(
            "  Prompt:             {:?} · {:?} · template {} · generation-prompt={} · filter={:?}",
            serve.prompt.mode,
            serve.prompt.delivery,
            serve
                .prompt
                .template
                .as_ref()
                .map(|template| format!("{} ({})", template.identity, template.path.display()))
                .unwrap_or_else(|| "runtime/upstream default".to_owned()),
            serve.prompt.render_generation_prompt,
            serve.prompt.response_filter,
        );
        println!(
            "  Generation:         temperature={} top_p={} top_k={} min_p={} reasoning={} thinking={}{}",
            optional_value(serve.generation.defaults.temperature),
            optional_value(serve.generation.defaults.top_p),
            optional_value(serve.generation.defaults.top_k),
            optional_value(serve.generation.defaults.min_p),
            serve
                .generation
                .defaults
                .reasoning_effort
                .as_deref()
                .unwrap_or("runtime default"),
            serve.generation.thinking.default,
            if serve.generation.thinking.required {
                " (required)"
            } else {
                ""
            },
        );
        println!(
            "  Context:            preferred={} minimum={}",
            optional_value(serve.load.context.preferred),
            optional_value(serve.load.context.minimum),
        );
        if let Some(q27) = &serve.engine.q27 {
            println!(
                "  q27 strategy:       KV {} · MTP {} / pmin {} · suffix={} W_MAX={} · fast-head={} override={}",
                q27.kv_quality_order.join(" -> "),
                q27.mtp.maximum_depth,
                q27.mtp.minimum_probability,
                q27.suffix_drafting,
                q27.suffix_width_from_runtime_w_max,
                q27.fast_head.default,
                q27.fast_head.user_override_allowed,
            );
        }
        if let Some(ninfer) = &serve.engine.ninfer {
            println!(
                "  NInfer strategy:    KV {}/{} · CUDA graph={} · prefix reuse={} · default strategy={} · speculative profiles={}",
                ninfer.kv_cache,
                ninfer.kv_dtype,
                ninfer.cuda_graph_required,
                ninfer.prefix_reuse_required,
                ninfer.default_speculative_profile,
                ninfer
                    .speculative_profiles
                    .iter()
                    .map(|(name, profile)| format!(
                        "{name}={}",
                        if profile.speculative_decoding {
                            format!(
                                "{}/{}{}",
                                profile.backend.as_deref().unwrap_or("missing-backend"),
                                profile
                                    .draft_tokens
                                    .map(|value| value.to_string())
                                    .unwrap_or_else(|| "missing-drafts".to_owned()),
                                if profile.optimized_proposal_head == Some(true) {
                                    "/lm-head-draft"
                                } else {
                                    ""
                                }
                            )
                        } else {
                            "off".to_owned()
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        let requirements = serve.effective_requirements();
        println!(
            "  Requirements:       {}",
            if requirements.is_empty() {
                "none".to_owned()
            } else {
                requirements
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        println!(
            "  Allowed overrides:  {}",
            if serve.generation.allowed_user_overrides.is_empty() {
                "none".to_owned()
            } else {
                serve.generation.allowed_user_overrides.join(", ")
            }
        );
        if profile.settings.is_empty() {
            println!("  No overrides; all values inherit.");
        } else {
            for (id, value) in profile.settings.iter() {
                println!("  {id:<38} {value}");
            }
        }
        let mut models = state
            .model_assignments
            .iter()
            .filter_map(|(model, assigned)| (assigned == name).then_some(model))
            .collect::<Vec<_>>();
        models.extend(
            state
                .builder_profile_assignments
                .iter()
                .filter_map(|(model, assigned)| (assigned == name.as_str()).then_some(model)),
        );
        models.sort();
        models.dedup();
        if !models.is_empty() {
            println!(
                "  Assigned models: {}",
                models
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else if state.profiles.is_empty() {
        println!("No Serve Profiles exist.");
    } else {
        println!(
            "{:<32} {:<18} {:<10} ASSIGNED MODELS",
            "SERVE PROFILE", "SOURCE", "ACCESS"
        );
        for (name, profile) in &state.profiles {
            let assigned = state
                .model_assignments
                .values()
                .filter(|candidate| *candidate == name)
                .count()
                + state
                    .builder_profile_assignments
                    .values()
                    .filter(|candidate| candidate.as_str() == name.as_str())
                    .count();
            let serve = profile.effective_serve_profile(name);
            println!(
                "{name:<32} {:<18} {:<10} {assigned}",
                serve.source,
                if serve.read_only {
                    "read-only"
                } else {
                    "mutable"
                },
            );
        }
        if !state.raw_profile_models.is_empty() {
            println!(
                "None / Raw runtime defaults: {} model(s)",
                state.raw_profile_models.len()
            );
        }
    }
    Ok(())
}

fn optional_value<T: ToString>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "runtime default".to_owned())
}

pub fn profile_compatibility(
    profile: &norted_core::ServeProfile,
    model: &norted_core::ModelArtifact,
    selection: &norted_core::RuntimeSelection,
    compatibility: &norted_core::RuntimeCompatibility,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "model_id": model.id,
                "artifact_format": model.format,
                "profile": profile,
                "runtime_id": selection.runtime.manifest.runtime_id,
                "compatibility": compatibility,
            }))?
        );
    } else {
        println!("Serve Profile compatibility");
        println!("  Model:       {} ({})", model.display_name, model.format);
        println!("  Profile:     {} ({})", profile.display_name, profile.id);
        println!("  Runtime:     {}", selection.runtime.manifest.runtime_id);
        println!("  Status:      {compatibility:?}");
    }
    Ok(())
}

pub fn settings_schema(schema: &norted_core::LoadSettingsSchema, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(schema)?);
    } else {
        println!(
            "Load settings for {} / {}",
            schema.engine_id,
            schema
                .runtime_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
                .unwrap_or("no exact runtime")
        );
        for definition in &schema.definitions {
            let support = if definition.supported {
                "supported".to_owned()
            } else {
                format!(
                    "unsupported: {}",
                    definition
                        .unsupported_reason
                        .as_deref()
                        .unwrap_or("unknown reason")
                )
            };
            println!(
                "  {:<38} {:<11} {}",
                definition.id, support, definition.label
            );
        }
    }
    Ok(())
}

pub fn effective_settings(
    runtime_id: &norted_core::RuntimeId,
    schema: &norted_core::LoadSettingsSchema,
    resolved: &norted_core::ResolvedLoadSettings,
    json_output: bool,
) -> Result<()> {
    let rows = schema
        .definitions
        .iter()
        .map(|definition| {
            let effective = resolved.effective.get(&definition.id);
            json!({
                "id": definition.id,
                "label": definition.label,
                "value": effective.map(|setting| &setting.value),
                "source": effective.map(|setting| &setting.source),
                "state": if effective.is_some() { "configured" } else { "upstream_default" },
                "supported": definition.supported,
                "unsupported_reason": definition.unsupported_reason,
            })
        })
        .collect::<Vec<_>>();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "runtime_id": runtime_id,
                "engine_id": schema.engine_id,
                "selected_profile": resolved.selected_profile,
                "settings": rows,
            }))?
        );
    } else {
        println!("Runtime: {runtime_id}");
        println!("Engine:  {}", schema.engine_id);
        println!(
            "Profile: {}",
            resolved
                .selected_profile
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
                .unwrap_or("none")
        );
        println!("{:<38} {:<20} SOURCE", "SETTING", "VALUE");
        for definition in &schema.definitions {
            if let Some(setting) = resolved.effective.get(&definition.id) {
                let suffix = if definition.supported {
                    String::new()
                } else {
                    format!(
                        " [incompatible: {}]",
                        definition
                            .unsupported_reason
                            .as_deref()
                            .unwrap_or("unsupported")
                    )
                };
                println!(
                    "{:<38} {:<20} {}{}",
                    definition.id, setting.value, setting.source, suffix
                );
            } else {
                println!(
                    "{:<38} {:<20} upstream",
                    definition.id, "<upstream default>"
                );
            }
        }
    }
    Ok(())
}

pub fn settings_mutation(
    operation: &str,
    scope: &str,
    state: &norted_core::LoadProfilesState,
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
    } else {
        println!("Load settings {operation}: {scope}");
        println!("Changes apply on the next model load.");
    }
    Ok(())
}
