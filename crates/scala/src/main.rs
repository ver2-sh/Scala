mod cli;
mod composition;
mod doctor;
mod output;
mod prune;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{
    AuthCommand, AuthKeysCommand, Cli, Command, ConfigCommand, EnginesCommand,
    ModelProfilesCommand, ModelsCommand, RuntimesCommand, SettingsCommand, SettingsMutationArgs,
    SettingsUnsetArgs,
};
use color_eyre::Result;
use scala_core::{
    ApiKeyStore, AppPaths, ApplicationCore, EngineId, ModelId, ModelProfileId, ModelProfilesStore,
    PublicAuthStatus, RuntimeId, SettingId, SettingsError, SettingsPatch, SettingsStore,
};
use scala_engine::{ControlClient, ControlClientError, EngineRegistry};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    color_eyre::install().ok();
    let cli = Cli::parse();
    let json_errors = cli.json;
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            if json_errors {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "error": {
                            "message": error.to_string(),
                        }
                    })
                );
            } else {
                eprintln!("Error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    if let Some(Command::Doctor(args)) = &cli.command {
        let report = doctor::run().await;
        let has_failures = report.has_failures();
        output::doctor(&report, cli.json, args.verbose)?;
        return Ok(if has_failures {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }
    let paths = AppPaths::discover()?;
    if let Some(Command::Prune(args)) = &cli.command {
        prune::run(&paths, args, cli.json).await?;
        return Ok(ExitCode::SUCCESS);
    }
    let core = ApplicationCore::load().await?;
    let _log_guard = init_logging(&paths);

    match cli.command.unwrap_or(Command::Tui) {
        Command::Prune(_) => unreachable!("prune dispatches before application initialization"),
        Command::Tui => {
            run_tui(core, cli.json).await?;
        }
        Command::Link { command } => match command {
            cli::LinkCommand::Status => {
                let observed = match ControlClient::discover(&core.paths).await {
                    Ok(client) => client.link_status().await.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "configured": core.config.link, "observed": observed,
                        "setup": "Link machines in Wayfinder, then use link enable and start Scala. Registration and reconnection are automatic."
                    }))?
                );
            }
            cli::LinkCommand::Enable => {
                let config = scala_core::LinkConfig { enabled: true };
                config
                    .save(&core.paths)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                println!("Scala Link enabled in configuration. Restart Scala to connect.");
            }
            cli::LinkCommand::Disable => {
                let mut config = core.config.link.clone();
                config.enabled = false;
                config
                    .save(&core.paths)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                println!("Scala Link disabled in configuration. Restart Scala to disconnect.");
            }
        },
        Command::Serve => {
            let services = composition::ApplicationServices::new(&core)?;
            let startup_guard = composition::ServerStartupGuard::acquire(&core.paths).await?;
            let server = services
                .start_server(core, composition::ModelDiscoveryReadiness::RequireReady)
                .await?;
            drop(startup_guard);
            report_insecure_remote(server.auth_status(), cli.json);
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "event": "listening",
                        "address": server.local_addr(),
                    }))?
                );
            } else {
                println!("Scala API listening at http://{}", server.local_addr());
                println!("Press Ctrl+C to stop.");
            }
            server
                .run_while(async {
                    shutdown_signal().await;
                    Ok(())
                })
                .await?;
        }
        Command::Benchmarks(args) => {
            let client = scala_engine::ControlClient::discover(&core.paths).await?;
            let response = client.benchmark(args.command.into()).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else if args.verbose {
                println!("{}", scala_engine::benchmark::inspection_text(&response));
            } else {
                println!("{}", scala_engine::benchmark::scorecard_text(&response));
            }
        }
        Command::Status => output::status(core, cli.json).await?,
        Command::Auth(args) => {
            handle_auth(&core, args.command, cli.json).await?;
        }
        Command::Load {
            model_profile_id,
            runtime,
            settings,
        } => {
            let registry = composition::engine_registry(&core)?;
            let mut settings = registry.parse_settings(&settings)?;
            record_q27_local_file_identity(&core, &mut settings).await?;
            record_llama_local_file_identities(&core, &mut settings).await?;
            let client = ControlClient::discover(&core.paths).await?;
            let runtime = runtime.map(RuntimeId::new).transpose()?;
            let status = client
                .load_with_settings(ModelProfileId::new(model_profile_id)?, runtime, settings)
                .await?;
            output::control_operation("load", &status, cli.json)?;
        }
        Command::Unload { model_profile_id } => {
            let client = ControlClient::discover(&core.paths).await?;
            let status = client
                .unload(ModelProfileId::new(model_profile_id)?)
                .await?;
            output::control_operation("unload", &status, cli.json)?;
        }
        Command::Models(args) => match args.command {
            ModelsCommand::List => output::models(core, cli.json).await?,
            ModelsCommand::Info { model_id } => {
                model_info(core, ModelId(model_id), cli.json).await?
            }
            ModelsCommand::Search { query, format } => {
                let library = build_model_library(&core);
                let search = library
                    .search(query.as_deref().unwrap_or(""), format)
                    .await?;
                output::models_search(&search, cli.json)?;
            }
            ModelsCommand::Download { model_ref } => {
                let library = build_model_library(&core);
                let download = download_model_with_progress(&library, &model_ref, cli.json).await?;
                core.refresh_models().await?;
                output::model_operation(
                    if download.already_installed {
                        "already_installed"
                    } else {
                        "download"
                    },
                    &download.artifact,
                    cli.json,
                )?;
            }
            ModelsCommand::Import { path } => {
                let library = build_model_library(&core);
                let artifact = library.import(&path).await?;
                core.refresh_models().await?;
                output::model_operation("import", &artifact, cli.json)?;
            }
            ModelsCommand::Remove { model_id } => {
                core.ensure_model_discovery().await?;
                let model_id = ModelId(model_id);
                let model = core.model(&model_id).await.ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "model `{model_id}` does not exist in the discovered registry"
                    )
                })?;
                let library = build_model_library(&core);
                let plan = library.plan_removal(&model)?;
                let active = match ControlClient::discover(&core.paths).await {
                    Ok(client) => client
                        .status()
                        .await?
                        .backends
                        .into_iter()
                        .map(|backend| backend.model_id)
                        .collect::<Vec<_>>(),
                    Err(ControlClientError::Unavailable) => Vec::new(),
                    Err(error) => return Err(error.into()),
                };
                if active
                    .iter()
                    .any(|active| plan.affected_model_ids.contains(active))
                {
                    return Err(color_eyre::eyre::eyre!(
                        "an active model belongs to this managed acquisition; unload it before removal"
                    ));
                }
                library.remove(&plan).await?;
                core.refresh_models().await?;
                output::model_removed(&model_id, cli.json)?;
            }
        },
        Command::Engines(args) => match args.command {
            EnginesCommand::List => output::engines(core, cli.json).await?,
        },
        Command::Runtimes(args) => {
            let registry = composition::engine_registry(&core)?;
            let packs = composition::runtime_pack_manager(&core, registry)?;
            match args.command {
                RuntimesCommand::List => {
                    packs.refresh_host_capabilities().await;
                    output::runtimes_list(&packs.list().await?, cli.json)?;
                }
                RuntimesCommand::Search { query, refresh } => {
                    packs.refresh_host_capabilities().await;
                    let snapshot = packs
                        .search(query.as_deref().unwrap_or(""), refresh)
                        .await?;
                    output::runtimes_search(&snapshot, cli.json)?;
                }
                RuntimesCommand::Info { runtime_ref } => {
                    packs.refresh_host_capabilities().await;
                    let runtime_id = RuntimeId::new(runtime_ref)?;
                    let list = packs.list().await?;
                    if let Some(installed) = list
                        .installed
                        .into_iter()
                        .find(|status| status.runtime.manifest.runtime_id == runtime_id)
                    {
                        output::runtime_info(
                            &serde_json::json!({
                                "state": "installed",
                                "runtime": installed,
                            }),
                            cli.json,
                        )?;
                    } else {
                        let search = packs.search(runtime_id.as_str(), true).await?;
                        let available = search
                            .results
                            .into_iter()
                            .find(|result| result.entry.available.runtime_id == runtime_id)
                            .ok_or_else(|| {
                                color_eyre::eyre::eyre!("runtime `{runtime_id}` was not found")
                            })?;
                        output::runtime_info(
                            &serde_json::json!({
                                "state": "available",
                                "runtime": available,
                            }),
                            cli.json,
                        )?;
                    }
                }
                RuntimesCommand::Install { runtime_ref } => {
                    packs.refresh_host_capabilities().await;
                    let runtime = packs.install(&RuntimeId::new(runtime_ref)?).await?;
                    output::runtime_operation("install", &runtime, cli.json)?;
                }
                RuntimesCommand::Remove { runtime_ref } => {
                    let runtime_id = RuntimeId::new(runtime_ref)?;
                    packs.remove(&runtime_id, None).await?;
                    output::runtime_removed(&runtime_id, cli.json)?;
                }
                RuntimesCommand::CheckUpdates => {
                    packs.refresh_host_capabilities().await;
                    output::runtime_updates(&packs.check_updates().await?, cli.json)?;
                }
                RuntimesCommand::Update { runtime_ref } => {
                    packs.refresh_host_capabilities().await;
                    let runtime = packs.update(&RuntimeId::new(runtime_ref)?).await?;
                    output::runtime_operation("update", &runtime, cli.json)?;
                }
                RuntimesCommand::Select {
                    format,
                    model,
                    track,
                    runtime_ref,
                } => {
                    let runtime_id = RuntimeId::new(runtime_ref)?;
                    let selections = if let Some(format) = format {
                        packs
                            .select_format_with_preference(format, runtime_id, track.into())
                            .await?
                    } else {
                        core.ensure_model_discovery().await?;
                        let model_id = ModelId(model.expect("clap requires a selection target"));
                        let model = core.model(&model_id).await.ok_or_else(|| {
                            color_eyre::eyre::eyre!(
                                "model `{model_id}` does not exist in the discovered registry"
                            )
                        })?;
                        packs
                            .select_model_with_preference(&model, runtime_id, track.into())
                            .await?
                    };
                    output::runtime_selections("set", &selections, cli.json)?;
                }
                RuntimesCommand::ClearSelection { format, model } => {
                    let selections = if let Some(format) = format {
                        packs.clear_format_selection(format).await?
                    } else {
                        packs
                            .clear_model_selection(&ModelId(
                                model.expect("clap requires a selection target"),
                            ))
                            .await?
                    };
                    output::runtime_selections("cleared", &selections, cli.json)?;
                }
            }
        }
        Command::ModelProfiles(args) => {
            handle_model_profiles(Arc::clone(&core), args.command, cli.json).await?;
        }
        Command::Settings(args) => {
            handle_settings(Arc::clone(&core), args.command, cli.json).await?;
        }
        Command::Config(args) => match args.command {
            ConfigCommand::Show => output::config(core, cli.json)?,
        },
        Command::Doctor(_) => unreachable!("doctor is dispatched before application startup"),
    }
    Ok(ExitCode::SUCCESS)
}

fn build_model_library(core: &ApplicationCore) -> scala_model_library::ModelLibrary {
    scala_model_library::ModelLibrary::new(
        &core.paths,
        &core
            .config
            .models
            .effective_downloads_path(&core.paths.data_dir),
    )
}

async fn run_tui(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    let services = composition::ApplicationServices::new(&core)?;
    let setting_definitions = server_setting_definitions(&services.registry)?;
    let startup_guard = composition::ServerStartupGuard::acquire(&core.paths).await?;

    if composition::discover_existing_control(&core.paths)
        .await?
        .is_some()
    {
        drop(startup_guard);
        let _ = core.refresh_server_state().await;
        let model_library = Arc::new(build_model_library(&core));
        return scala_tui::run(
            core,
            services.runtime_packs,
            model_library,
            setting_definitions,
        )
        .await;
    }

    let server = match services
        .start_server(
            Arc::clone(&core),
            composition::ModelDiscoveryReadiness::AllowPending,
        )
        .await
    {
        Ok(server) => server,
        Err(startup_error) => {
            if composition::discover_existing_control(&core.paths)
                .await?
                .is_some()
            {
                drop(startup_guard);
                let _ = core.refresh_server_state().await;
                let model_library = Arc::new(build_model_library(&core));
                return scala_tui::run(
                    core,
                    services.runtime_packs,
                    model_library,
                    setting_definitions,
                )
                .await;
            }
            return Err(startup_error);
        }
    };
    drop(startup_guard);
    report_insecure_remote(server.auth_status(), json_output);
    let model_library = Arc::new(build_model_library(&core));
    server
        .run_while(scala_tui::run(
            core,
            services.runtime_packs,
            model_library,
            setting_definitions,
        ))
        .await
}

fn report_insecure_remote(status: &PublicAuthStatus, json_output: bool) {
    if !status.insecure_remote {
        return;
    }
    tracing::warn!(
        bind = %status.bind,
        "INSECURE REMOTE SERVING: public authentication is explicitly disabled and HTTP traffic is not encrypted"
    );
    if json_output {
        eprintln!(
            "{}",
            serde_json::json!({
                "warning": {
                    "code": "insecure_remote_serving",
                    "bind": status.bind,
                    "message": "public authentication is disabled; plain HTTP does not protect prompts, outputs, or credentials",
                }
            })
        );
    } else {
        eprintln!(
            "WARNING: INSECURE REMOTE SERVING on {}: public authentication is disabled and plain HTTP is not encrypted.",
            status.bind
        );
    }
}

async fn model_info(
    core: Arc<ApplicationCore>,
    model_id: ModelId,
    json_output: bool,
) -> Result<()> {
    core.ensure_model_discovery().await?;
    let model = core.model(&model_id).await.ok_or_else(|| {
        color_eyre::eyre::eyre!("model `{model_id}` does not exist in the discovered registry")
    })?;
    let active_model = match ControlClient::discover(&core.paths).await {
        Ok(client) => client
            .status()
            .await?
            .backends
            .into_iter()
            .find(|backend| backend.model_id == model_id)
            .map(|backend| backend.model_id),
        Err(ControlClientError::Unavailable) => None,
        Err(error) => return Err(error.into()),
    };
    let registry = composition::engine_registry(&core)?;
    let packs = composition::runtime_pack_manager(&core, registry)?;
    let capabilities = packs
        .model_serving_capabilities(&model, active_model.as_ref())
        .await?;
    output::model_info(&capabilities, json_output)?;
    Ok(())
}

async fn download_model_with_progress(
    library: &scala_model_library::ModelLibrary,
    model_ref: &str,
    json_output: bool,
) -> Result<scala_model_library::ModelDownloadResult> {
    if json_output {
        return Ok(library.download_with_status(model_ref).await?);
    }
    let mut progress = library.subscribe();
    let download = library.download_with_status(model_ref);
    tokio::pin!(download);
    let mut last_update = std::time::Instant::now() - std::time::Duration::from_secs(1);
    loop {
        tokio::select! {
            result = &mut download => {
                eprintln!();
                return Ok(result?);
            }
            event = progress.recv() => {
                let Ok(event) = event else { continue; };
                if last_update.elapsed() < std::time::Duration::from_millis(150)
                    && event.phase == scala_model_library::ModelOperationPhase::Downloading
                {
                    continue;
                }
                last_update = std::time::Instant::now();
                let bytes = event.total_bytes.map_or_else(
                    || format!("{} bytes", event.downloaded_bytes),
                    |total| format!("{} / {} bytes", event.downloaded_bytes, total),
                );
                eprint!("\r{: <12} {: <25} {bytes}", format!("{:?}", event.phase), event.filename);
                let _ = std::io::stderr().flush();
            }
        }
    }
}

async fn handle_auth(
    core: &ApplicationCore,
    command: AuthCommand,
    json_output: bool,
) -> Result<()> {
    let store = ApiKeyStore::new(&core.paths);
    match command {
        AuthCommand::Status => {
            let active_key_count = store.active_count().await?;
            let status = core.config.server.public_auth_status(active_key_count)?;
            output::auth_status(&status, json_output)?;
        }
        AuthCommand::Keys(args) => match args.command {
            AuthKeysCommand::List => output::api_keys(&store.list().await?, json_output)?,
            AuthKeysCommand::Create { name } => {
                let created = store.create(name).await?;
                output::api_key_created(&created, json_output)?;
            }
            AuthKeysCommand::Revoke { key_id } => {
                let revoked = store.revoke(key_id).await?;
                output::api_key_revoked(&revoked, json_output)?;
            }
        },
    }
    Ok(())
}

async fn handle_model_profiles(
    core: Arc<ApplicationCore>,
    command: ModelProfilesCommand,
    json_output: bool,
) -> Result<()> {
    let store = ModelProfilesStore::new(&core.paths);
    match command {
        ModelProfilesCommand::List => {
            core.ensure_model_discovery().await?;
            let state = store.read().await?;
            output::model_profiles(
                "list",
                &state,
                None,
                &core.snapshot().await.models,
                json_output,
            )?;
        }
        ModelProfilesCommand::Show { profile } => {
            let profile_id = ModelProfileId::new(profile)?;
            let (profile, model, _, _, resolved, _) =
                exact_model_profile_context(&core, &profile_id, None).await?;
            output::model_profile("show", &profile, Some(&model), &resolved, json_output)?;
        }
        ModelProfilesCommand::Create {
            profile,
            model,
            engine,
            role,
        } => {
            let profile_id = ModelProfileId::new(profile)?;
            let model_id = ModelId(model);
            let engine_id = EngineId::new(engine)?;
            core.ensure_model_discovery().await?;
            let model = require_model(&core, &model_id).await?;
            let registry = composition::engine_registry(&core)?;
            require_compatible_engine(&registry, &model, engine_id.as_str())?;
            let selected = profile_id.clone();
            let state = store
                .update(move |state| {
                    state.create(profile_id.clone(), profile_id.as_str(), model_id, engine_id)?;
                    state
                        .profiles
                        .get_mut(&profile_id)
                        .expect("new profile")
                        .role = role.into();
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "create", &state, &selected, json_output).await?;
        }
        ModelProfilesCommand::Duplicate { source, profile } => {
            let source = ModelProfileId::new(source)?;
            let profile = ModelProfileId::new(profile)?;
            let selected = profile.clone();
            let state = store
                .update(move |state| {
                    state.duplicate(&source, profile.clone(), profile.as_str())?;
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "duplicate", &state, &selected, json_output)
                .await?;
        }
        ModelProfilesCommand::Delete { profile } => {
            let profile_id = ModelProfileId::new(profile)?;
            if let Ok(client) = ControlClient::discover(&core.paths).await
                && client.status().await?.backend(&profile_id).is_some()
            {
                return Err(color_eyre::eyre::eyre!(
                    "cannot delete active Model Profile `{profile_id}`; unload it first"
                ));
            }
            let deleted = store.update(move |state| state.delete(&profile_id)).await?;
            output::model_profile_deleted(&deleted.id, json_output)?;
        }
        ModelProfilesCommand::SetModel { profile, model } => {
            let profile_id = ModelProfileId::new(profile)?;
            let model_id = ModelId(model);
            core.ensure_model_discovery().await?;
            let model = require_model(&core, &model_id).await?;
            let registry = composition::engine_registry(&core)?;
            let current = store.read().await?;
            let existing = current
                .profiles
                .get(&profile_id)
                .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
            require_compatible_engine(&registry, &model, existing.engine_id.as_str())?;
            validate_patch_for_model(
                &registry,
                &model,
                existing.engine_id.as_str(),
                &existing.overrides,
            )?;
            let selected = profile_id.clone();
            let state = store
                .update(move |state| {
                    state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?
                        .model_id = model_id;
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "set-model", &state, &selected, json_output)
                .await?;
        }
        ModelProfilesCommand::SetEngine { profile, engine } => {
            let profile_id = ModelProfileId::new(profile)?;
            let engine_id = EngineId::new(engine)?;
            core.ensure_model_discovery().await?;
            let current = store.read().await?;
            let existing = current
                .profiles
                .get(&profile_id)
                .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
            let model = require_model(&core, &existing.model_id).await?;
            let registry = composition::engine_registry(&core)?;
            require_compatible_engine(&registry, &model, engine_id.as_str())?;
            let settings = SettingsStore::new(&core.paths).read().await?;
            let packs = composition::runtime_pack_manager(&core, registry)?;
            let candidate = packs
                .model_profile_engine_switch_candidate(
                    &settings,
                    existing,
                    &model,
                    engine_id.as_str(),
                    &core.paths.data_dir,
                )
                .await?;
            let original_hash = existing.content_hash();
            let overrides = candidate.overrides.clone();
            let update_profile_id = profile_id.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&update_profile_id)
                        .ok_or_else(|| {
                            SettingsError::ModelProfileNotFound(update_profile_id.clone())
                        })?;
                    if profile.content_hash() != original_hash {
                        return Err(SettingsError::InvalidModelProfile(format!(
                            "Model Profile `{update_profile_id}` changed during engine-switch preflight"
                        )));
                    }
                    profile.engine_id = engine_id;
                    profile.overrides = overrides;
                    Ok(state.clone())
                })
                .await?;
            if !candidate.removed.is_empty() {
                eprintln!(
                    "Notice: removed incompatible Model Profile overrides: {}",
                    candidate
                        .removed
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let profile = state
                .profiles
                .get(&profile_id)
                .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
            output::model_profile(
                "set-engine",
                profile,
                Some(&model),
                &candidate.resolved,
                json_output,
            )?;
        }
        ModelProfilesCommand::SetRole { profile, role } => {
            let profile_id = ModelProfileId::new(profile)?;
            let selected = profile_id.clone();
            let state = store
                .update(move |state| {
                    state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?
                        .role = role.into();
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "set-role", &state, &selected, json_output)
                .await?;
        }
        ModelProfilesCommand::Set { profile, settings } => {
            let profile_id = ModelProfileId::new(profile)?;
            core.ensure_model_discovery().await?;
            let registry = composition::engine_registry(&core)?;
            let mut patch = registry.parse_settings(&settings)?;
            scala_engine::record_local_file_setting_identity(
                &mut patch,
                "q27.template_path",
                "q27.template_sha256",
                &core.paths.data_dir,
                4 * 1024 * 1024,
            )
            .await?;
            record_llama_local_file_identities(&core, &mut patch).await?;
            let current = store.read().await?;
            let existing = current
                .profiles
                .get(&profile_id)
                .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
            validate_patch_for_engine(&patch, existing.engine_id.as_str())?;
            let model = require_model(&core, &existing.model_id).await?;
            let mut candidate = existing.overrides.clone();
            candidate.0.extend(patch.0.clone());
            let packs = composition::runtime_pack_manager(&core, registry.clone())?;
            let settings_state = SettingsStore::new(&core.paths).read().await?;
            packs
                .validate_profile_settings(
                    &settings_state,
                    existing,
                    &model,
                    &candidate,
                    &core.paths.data_dir,
                )
                .await?;
            let selected = profile_id.clone();
            let state = store
                .update(move |state| {
                    state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?
                        .overrides
                        .0
                        .extend(patch.0);
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "set", &state, &selected, json_output).await?;
        }
        ModelProfilesCommand::Unset { profile, settings } => {
            let profile_id = ModelProfileId::new(profile)?;
            let registry = composition::engine_registry(&core)?;
            let mut ids = parse_known_setting_ids(&registry, &settings)?;
            if ids.iter().any(|id| id.as_str() == "q27.template_path") {
                ids.push(SettingId::new("q27.template_sha256")?);
            }
            add_llama_bound_identity_ids(&mut ids)?;
            let selected = profile_id.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&profile_id)
                        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
                    for id in &ids {
                        profile.overrides.remove(id);
                    }
                    Ok(state.clone())
                })
                .await?;
            output_model_profile_mutation(&core, "unset", &state, &selected, json_output).await?;
        }
        ModelProfilesCommand::Load {
            profile,
            runtime,
            settings,
        } => {
            let profile_id = ModelProfileId::new(profile)?;
            let registry = composition::engine_registry(&core)?;
            let mut settings = registry.parse_settings(&settings)?;
            record_q27_local_file_identity(&core, &mut settings).await?;
            record_llama_local_file_identities(&core, &mut settings).await?;
            let runtime = runtime.map(RuntimeId::new).transpose()?;
            let status = ControlClient::discover(&core.paths)
                .await?
                .load_with_settings(profile_id, runtime, settings)
                .await?;
            output::control_operation("load", &status, json_output)?;
        }
        ModelProfilesCommand::Compatibility { profile, runtime } => {
            let profile_id = ModelProfileId::new(profile)?;
            let (profile, model, selection, schema, resolved, compatibility) =
                exact_model_profile_context(&core, &profile_id, runtime).await?;
            output::model_profile_compatibility(
                &profile,
                &model,
                &selection,
                &schema,
                &resolved,
                &compatibility,
                json_output,
            )?;
        }
    }
    Ok(())
}

async fn output_model_profile_mutation(
    core: &Arc<ApplicationCore>,
    operation: &str,
    _state: &scala_core::ModelProfilesState,
    selected: &ModelProfileId,
    json_output: bool,
) -> Result<()> {
    let (profile, model, _, _, resolved, _) =
        exact_model_profile_context(core, selected, None).await?;
    output::model_profile(operation, &profile, Some(&model), &resolved, json_output)
}

async fn exact_model_profile_context(
    core: &Arc<ApplicationCore>,
    profile_id: &ModelProfileId,
    runtime: Option<String>,
) -> Result<(
    scala_core::ModelProfile,
    scala_core::ModelArtifact,
    scala_core::RuntimeSelection,
    scala_core::SettingsSchema,
    scala_core::ResolvedSettings,
    scala_core::RuntimeCompatibility,
)> {
    core.ensure_model_discovery().await?;
    let profile = ModelProfilesStore::new(&core.paths)
        .read()
        .await?
        .profiles
        .get(profile_id)
        .cloned()
        .ok_or_else(|| SettingsError::ModelProfileNotFound(profile_id.clone()))?;
    let model = require_model(core, &profile.model_id).await.map_err(|_| {
        color_eyre::eyre::eyre!(
            "Model Profile `{}` is unavailable because bound model `{}` is missing",
            profile.id,
            profile.model_id
        )
    })?;
    let registry = composition::engine_registry(core)?;
    require_compatible_engine(&registry, &model, profile.engine_id.as_str())?;
    let settings = SettingsStore::new(&core.paths).read().await?;
    let mut resolved = settings.resolve(
        &profile.id,
        profile.engine_id.as_str(),
        &profile.overrides,
        &SettingsPatch::default(),
        &core.paths.data_dir,
    )?;
    let adapter = registry.get(profile.engine_id.as_str()).ok_or_else(|| {
        color_eyre::eyre::eyre!("bound engine `{}` is not registered", profile.engine_id)
    })?;
    adapter.normalize_settings(&mut resolved)?;
    let packs = composition::runtime_pack_manager(core, registry.clone())?;
    let runtime = runtime.map(RuntimeId::new).transpose()?;
    let (selection, schema) = packs
        .settings_schema_for_model_for_engine_with_settings(
            &model,
            profile.engine_id.as_str(),
            runtime.as_ref(),
            Some(&resolved),
        )
        .await?;
    let host = packs.host_capabilities().await;
    schema.materialize_runtime_configuration(&mut resolved)?;
    schema.validate(&resolved)?;
    schema.materialize_effective(&mut resolved)?;
    adapter.validate_configuration(&selection.runtime, Some(&model), &host, &resolved)?;
    let compatibility =
        adapter.runtime_model_compatibility(&selection.runtime, &model, &host, Some(&resolved));
    Ok((profile, model, selection, schema, resolved, compatibility))
}

async fn handle_settings(
    core: Arc<ApplicationCore>,
    command: SettingsCommand,
    json_output: bool,
) -> Result<()> {
    let store = SettingsStore::new(&core.paths);
    match command {
        SettingsCommand::Show { server, runtime } => {
            let scope = defaults_scope(server, runtime);
            let state = store.read().await?;
            output_settings_scope(&core, "show", &scope, &state, json_output).await?;
        }
        SettingsCommand::Set(args) => mutate_defaults(&core, args, json_output).await?,
        SettingsCommand::Unset(args) => unset_defaults(&core, args, json_output).await?,
    }
    Ok(())
}

async fn output_settings_scope(
    core: &Arc<ApplicationCore>,
    operation: &str,
    scope: &DefaultsScope,
    state: &scala_core::SettingsState,
    json_output: bool,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let mut runtime_id = None;
    let definitions = match scope {
        DefaultsScope::Server => server_setting_definitions(&registry)?
            .into_iter()
            .filter(|definition| definition.scope == scala_core::SettingScope::Server)
            .collect(),
        DefaultsScope::Runtime(engine_id) => {
            let packs = composition::runtime_pack_manager(core, registry)?;
            let (mut schemas, warnings) = packs
                .selected_runtime_settings_schemas(state, &core.paths.data_dir)
                .await?;
            let schema = schemas.remove(engine_id).ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "selected runtime defaults for `{engine_id}` are unavailable{}",
                    if warnings.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", warnings.join("; "))
                    }
                )
            })?;
            runtime_id = schema.runtime_id;
            schema.definitions
        }
    };
    output::settings_defaults(
        operation,
        &scope.to_string(),
        state,
        &definitions,
        runtime_id.as_ref(),
        json_output,
    )
}

#[derive(Clone)]
enum DefaultsScope {
    Server,
    Runtime(String),
}

impl std::fmt::Display for DefaultsScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server => formatter.write_str("server"),
            Self::Runtime(engine) => write!(formatter, "runtime:{engine}"),
        }
    }
}

async fn mutate_defaults(
    core: &Arc<ApplicationCore>,
    args: SettingsMutationArgs,
    json_output: bool,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let mut patch = parse_server_settings(&registry, &args.settings)?;
    scala_engine::record_local_file_setting_identity(
        &mut patch,
        "q27.template_path",
        "q27.template_sha256",
        &core.paths.data_dir,
        4 * 1024 * 1024,
    )
    .await?;
    record_llama_local_file_identities(core, &mut patch).await?;
    let scope = defaults_scope(args.server, args.runtime);
    validate_default_scope(&registry, &scope, &patch)?;
    let store = SettingsStore::new(&core.paths);
    let current = store.read().await?;
    if let DefaultsScope::Runtime(engine) = &scope {
        validate_runtime_default_candidate(core, &current, engine, &patch).await?;
    }
    let update_scope = scope.clone();
    let state = store
        .update(move |state| {
            default_patch_mut(state, &update_scope).0.extend(patch.0);
            Ok(state.clone())
        })
        .await?;
    output_settings_scope(core, "set", &scope, &state, json_output).await?;
    Ok(())
}

async fn validate_runtime_default_candidate(
    core: &Arc<ApplicationCore>,
    state: &scala_core::SettingsState,
    engine: &str,
    patch: &SettingsPatch,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let packs = composition::runtime_pack_manager(core, registry)?;
    let (mut schemas, warnings) = packs
        .selected_runtime_settings_schemas(state, &core.paths.data_dir)
        .await?;
    let schema = schemas.remove(engine).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "selected runtime schema for `{engine}` is unavailable{}",
            if warnings.is_empty() {
                String::new()
            } else {
                format!(": {}", warnings.join("; "))
            }
        )
    })?;
    let mut candidate = state.clone();
    candidate
        .runtime_defaults
        .entry(engine.to_owned())
        .or_default()
        .0
        .extend(patch.0.clone());
    let resolved = candidate.resolve_runtime_defaults(engine, &core.paths.data_dir)?;
    packs.validate_runtime_settings(&schema, &resolved).await?;
    Ok(())
}

async fn record_llama_local_file_identities(
    core: &ApplicationCore,
    patch: &mut SettingsPatch,
) -> Result<()> {
    scala_engine::record_local_file_setting_identity(
        patch,
        "llama.cpp.chat_template_file",
        "llama.cpp.chat_template_sha256",
        &core.paths.data_dir,
        4 * 1024 * 1024,
    )
    .await?;
    scala_engine::record_local_file_setting_identity(
        patch,
        "llama.cpp.speculative_draft_model",
        "llama.cpp.speculative_draft_sha256",
        &core.paths.data_dir,
        1024_u64 * 1024 * 1024 * 1024,
    )
    .await?;
    Ok(())
}

async fn record_q27_local_file_identity(
    core: &ApplicationCore,
    patch: &mut SettingsPatch,
) -> Result<()> {
    scala_engine::record_local_file_setting_identity(
        patch,
        "q27.template_path",
        "q27.template_sha256",
        &core.paths.data_dir,
        4 * 1024 * 1024,
    )
    .await?;
    Ok(())
}

fn add_llama_bound_identity_ids(ids: &mut Vec<SettingId>) -> Result<()> {
    for (path, sha256) in [
        (
            "llama.cpp.chat_template_file",
            "llama.cpp.chat_template_sha256",
        ),
        (
            "llama.cpp.speculative_draft_model",
            "llama.cpp.speculative_draft_sha256",
        ),
    ] {
        if ids.iter().any(|id| id.as_str() == path) {
            ids.push(SettingId::new(sha256)?);
        }
    }
    Ok(())
}

async fn unset_defaults(
    core: &Arc<ApplicationCore>,
    args: SettingsUnsetArgs,
    json_output: bool,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let mut ids = parse_known_setting_ids(&registry, &args.settings)?;
    if ids.iter().any(|id| id.as_str() == "q27.template_path") {
        ids.push(SettingId::new("q27.template_sha256")?);
    }
    add_llama_bound_identity_ids(&mut ids)?;
    let scope = defaults_scope(args.server, args.runtime);
    let patch = SettingsPatch(
        ids.iter()
            .cloned()
            .map(|id| (id, scala_core::SettingValue::FlagEnabled))
            .collect(),
    );
    validate_default_scope(&registry, &scope, &patch)?;
    let update_scope = scope.clone();
    let state = SettingsStore::new(&core.paths)
        .update(move |state| {
            let target = default_patch_mut(state, &update_scope);
            for id in &ids {
                target.remove(id);
            }
            if let DefaultsScope::Runtime(engine) = &update_scope
                && state
                    .runtime_defaults
                    .get(engine)
                    .is_some_and(SettingsPatch::is_empty)
            {
                state.runtime_defaults.remove(engine);
            }
            Ok(state.clone())
        })
        .await?;
    output_settings_scope(core, "unset", &scope, &state, json_output).await?;
    Ok(())
}

fn defaults_scope(server: bool, runtime: Option<String>) -> DefaultsScope {
    if server {
        DefaultsScope::Server
    } else {
        DefaultsScope::Runtime(runtime.expect("clap requires one settings scope"))
    }
}

fn default_patch_mut<'a>(
    state: &'a mut scala_core::SettingsState,
    scope: &DefaultsScope,
) -> &'a mut SettingsPatch {
    match scope {
        DefaultsScope::Server => &mut state.server_settings,
        DefaultsScope::Runtime(engine) => state.runtime_defaults.entry(engine.clone()).or_default(),
    }
}

fn validate_default_scope(
    registry: &EngineRegistry,
    scope: &DefaultsScope,
    patch: &SettingsPatch,
) -> Result<()> {
    match scope {
        DefaultsScope::Server => {
            if let Some(id) = patch.0.keys().find(|id| id.namespace() != Some("server")) {
                return Err(SettingsError::InvalidServerSetting(id.clone()).into());
            }
        }
        DefaultsScope::Runtime(engine) => {
            if registry.get(engine).is_none() {
                return Err(color_eyre::eyre::eyre!("unknown engine `{engine}`"));
            }
            let definitions = server_setting_definitions(registry)?
                .into_iter()
                .map(|definition| (definition.id.clone(), definition))
                .collect::<std::collections::BTreeMap<_, _>>();
            if let Some(id) = patch.0.keys().find(|id| {
                definitions
                    .get(*id)
                    .is_some_and(|definition| definition.scope == scala_core::SettingScope::Server)
            }) {
                return Err(color_eyre::eyre::eyre!(
                    "setting `{id}` is available only in Server Settings"
                ));
            }
            validate_patch_for_engine(patch, engine)?;
        }
    }
    Ok(())
}

fn validate_patch_for_engine(patch: &SettingsPatch, engine: &str) -> Result<()> {
    for id in patch.0.keys() {
        if !id.applies_to_engine(engine) {
            return Err(SettingsError::WrongEngineScope {
                setting_id: id.clone(),
                engine_id: engine.to_owned(),
            }
            .into());
        }
    }
    Ok(())
}

fn validate_patch_for_model(
    registry: &EngineRegistry,
    model: &scala_core::ModelArtifact,
    engine: &str,
    patch: &SettingsPatch,
) -> Result<()> {
    let schema = model_schema_for_engine(registry, model, engine)?;
    let resolved = scala_core::ResolvedSettings {
        engine_id: engine.to_owned(),
        model_profile_id: None,
        configured: patch
            .0
            .iter()
            .map(|(id, value)| {
                (
                    id.clone(),
                    scala_core::ResolvedSetting {
                        value: value.clone(),
                        source: scala_core::SettingSource::ModelProfile {
                            model_profile_id: ModelProfileId::new("model-validation")
                                .expect("static Model Profile ID"),
                        },
                    },
                )
            })
            .collect(),
        effective: Default::default(),
    };
    schema.validate(&resolved)?;
    Ok(())
}

fn model_schema_for_engine(
    registry: &EngineRegistry,
    model: &scala_core::ModelArtifact,
    engine: &str,
) -> Result<scala_core::SettingsSchema> {
    let adapter = registry
        .get(engine)
        .ok_or_else(|| color_eyre::eyre::eyre!("unknown engine `{engine}`"))?;
    Ok(scala_core::SettingsSchema {
        engine_id: engine.to_owned(),
        runtime_id: None,
        definitions: adapter.model_setting_definitions(model)?,
    })
}

fn parse_known_setting_ids(
    _registry: &EngineRegistry,
    values: &[String],
) -> Result<Vec<SettingId>> {
    // Removal must work for obsolete and unsupported keys, without runtime discovery.
    values
        .iter()
        .map(|value| SettingId::new(value.clone()).map_err(Into::into))
        .collect()
}

fn server_setting_definitions(
    registry: &EngineRegistry,
) -> Result<Vec<scala_core::SettingDefinition>> {
    let mut definitions = registry.setting_definitions()?;
    definitions.push(scala_model_library::setting_definition());
    definitions.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(definitions)
}

fn parse_server_settings(
    registry: &EngineRegistry,
    assignments: &[String],
) -> Result<SettingsPatch> {
    let definitions = server_setting_definitions(registry)?
        .into_iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut patch = SettingsPatch::default();
    for assignment in assignments {
        let (raw_id, raw_value) = assignment.split_once('=').ok_or_else(|| {
            color_eyre::eyre::eyre!("setting `{assignment}` must use SETTING_ID=VALUE")
        })?;
        let id = SettingId::new(raw_id.to_owned())?;
        let definition = definitions
            .get(&id)
            .ok_or_else(|| SettingsError::UnknownSetting(id.clone()))?;
        patch.insert(id, definition.parse(raw_value)?);
    }
    Ok(patch)
}

async fn require_model(
    core: &ApplicationCore,
    model_id: &ModelId,
) -> Result<scala_core::ModelArtifact> {
    core.model(model_id).await.ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "model `{model_id}` does not exist in the discovered artifact registry"
        )
    })
}

fn require_compatible_engine(
    registry: &EngineRegistry,
    model: &scala_core::ModelArtifact,
    engine_id: &str,
) -> Result<()> {
    let adapter = registry
        .get(engine_id)
        .ok_or_else(|| color_eyre::eyre::eyre!("unknown engine `{engine_id}`"))?;
    if !adapter.compatibility(model).is_supported() {
        return Err(color_eyre::eyre::eyre!(
            "engine `{engine_id}` is incompatible with model `{}` ({})",
            model.id,
            model.format
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).ok();
    let mut hangup = signal(SignalKind::hangup()).ok();
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                tracing::error!(%error, "could not install Ctrl+C handler");
            }
        }
        _ = async {
            if let Some(signal) = &mut terminate {
                signal.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {}
        _ = async {
            if let Some(signal) = &mut hangup {
                signal.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not install Ctrl+C handler");
    }
}

fn init_logging(paths: &AppPaths) -> tracing_appender::non_blocking::WorkerGuard {
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "scala.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .init();
    guard
}
