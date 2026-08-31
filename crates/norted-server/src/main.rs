mod cli;
mod composition;
mod doctor;
mod output;

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{
    AuthCommand, AuthKeysCommand, Cli, Command, ConfigCommand, EnginesCommand, ModelsCommand,
    ProfilesCommand, RuntimesCommand, SettingsCommand, SettingsMutationArgs, SettingsUnsetArgs,
};
use color_eyre::Result;
use norted_core::{
    ApiKeyStore, AppPaths, ApplicationCore, LoadSettingId, LoadSettingScope, LoadSettingsError,
    LoadSettingsPatch, ModelId, PromptDelivery, PromptMode, PublicAuthStatus, ResponseFilter,
    RuntimeId, ServeProfile, ServeProfileName, ServeProfileSource, ServeProfilesStore,
};
use norted_engine::{ControlClient, ControlClientError, EngineRegistry};
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
    if matches!(&cli.command, Some(Command::Doctor)) {
        let checks = doctor::run();
        let has_fatal = checks.iter().any(|check| check.fatal);
        output::doctor(&checks, cli.json)?;
        return Ok(if has_fatal {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }
    let paths = AppPaths::discover()?;
    paths.ensure_required()?;
    let _log_guard = init_logging(&paths);
    let core = ApplicationCore::load().await?;

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => {
            run_tui(core, cli.json).await?;
        }
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
                println!("Norted API listening at http://{}", server.local_addr());
                println!("Press Ctrl+C to stop.");
            }
            server
                .run_while(async {
                    shutdown_signal().await;
                    Ok(())
                })
                .await?;
        }
        Command::Status => output::status(core, cli.json).await?,
        Command::Auth(args) => {
            handle_auth(&core, args.command, cli.json).await?;
        }
        Command::Load {
            model_id,
            runtime,
            profile,
            settings,
        } => {
            let registry = composition::engine_registry(&core)?;
            let settings = registry.parse_load_settings(&settings)?;
            let client = ControlClient::discover(&core.paths).await?;
            let runtime = runtime.map(RuntimeId::new).transpose()?;
            let profile = profile.map(ServeProfileName::new).transpose()?;
            let status = client
                .load_with_settings(ModelId(model_id), runtime, profile, settings)
                .await?;
            output::control_operation("load", &status, cli.json)?;
        }
        Command::Unload => {
            let client = ControlClient::discover(&core.paths).await?;
            let status = client.unload().await?;
            output::control_operation("unload", &status, cli.json)?;
        }
        Command::Models(args) => match args.command {
            ModelsCommand::List => output::models(core, cli.json).await?,
            ModelsCommand::Info { model_id } => {
                model_info(core, ModelId(model_id), cli.json).await?
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
                    let active = match ControlClient::discover(&core.paths).await {
                        Ok(client) => client.status().await?.backend.runtime_id,
                        Err(ControlClientError::Unavailable) => None,
                        Err(error) => return Err(error.into()),
                    };
                    packs.remove(&runtime_id, active.as_ref()).await?;
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
        Command::Profiles(args) => {
            handle_profiles(Arc::clone(&core), args.command, cli.json).await?;
        }
        Command::Settings(args) => {
            handle_settings(Arc::clone(&core), args.command, cli.json).await?;
        }
        Command::Config(args) => match args.command {
            ConfigCommand::Show => output::config(core, cli.json)?,
        },
        Command::Doctor => unreachable!("doctor is dispatched before application startup"),
    }
    Ok(ExitCode::SUCCESS)
}

async fn run_tui(core: Arc<ApplicationCore>, json_output: bool) -> Result<()> {
    let services = composition::ApplicationServices::new(&core)?;
    let load_setting_definitions = services.registry.load_setting_definitions()?;
    let startup_guard = composition::ServerStartupGuard::acquire(&core.paths).await?;

    if composition::discover_existing_control(&core.paths)
        .await?
        .is_some()
    {
        drop(startup_guard);
        let _ = core.refresh_server_state().await;
        return norted_tui::run(core, services.runtime_packs, load_setting_definitions).await;
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
                return norted_tui::run(core, services.runtime_packs, load_setting_definitions)
                    .await;
            }
            return Err(startup_error);
        }
    };
    drop(startup_guard);
    report_insecure_remote(server.auth_status(), json_output);
    server
        .run_while(norted_tui::run(
            core,
            services.runtime_packs,
            load_setting_definitions,
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
        Ok(client) => client.status().await?.backend.model_id,
        Err(ControlClientError::Unavailable) => None,
        Err(error) => return Err(error.into()),
    };
    let registry = composition::engine_registry(&core)?;
    let packs = composition::runtime_pack_manager(&core, registry)?;
    let profile_state = ServeProfilesStore::new(&core.paths).read().await?;
    let serve_profile = effective_serve_profile(&core, &profile_state, &model).await?;
    let capabilities = packs
        .model_serving_capabilities_with_profile(
            &model,
            active_model.as_ref(),
            serve_profile.as_ref(),
        )
        .await?;
    output::model_info(&capabilities, json_output)?;
    Ok(())
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

async fn handle_profiles(
    core: Arc<ApplicationCore>,
    command: ProfilesCommand,
    json_output: bool,
) -> Result<()> {
    let store = ServeProfilesStore::new(&core.paths);
    match command {
        ProfilesCommand::List => {
            core.ensure_model_discovery().await?;
            let state = profile_view(&core, store.read().await?).await?;
            output::profiles("list", &state, None, json_output)?;
        }
        ProfilesCommand::Show { name } => {
            let name = ServeProfileName::new(name)?;
            core.ensure_model_discovery().await?;
            let state = profile_view(&core, store.read().await?).await?;
            if !state.profiles.contains_key(&name) {
                return Err(LoadSettingsError::ProfileNotFound(name).into());
            }
            output::profiles("show", &state, Some(&name), json_output)?;
        }
        ProfilesCommand::Create { name } => {
            let name = ServeProfileName::new(name)?;
            core.ensure_model_discovery().await?;
            if discovered_builder_profiles(&core)
                .await?
                .contains_key(&name)
            {
                return Err(LoadSettingsError::ProfileAlreadyExists(name).into());
            }
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    state.create_profile(name)?;
                    Ok(state.clone())
                })
                .await?;
            output::profiles("create", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::Duplicate { source, name } => {
            core.ensure_model_discovery().await?;
            let source = ServeProfileName::new(source)?;
            let name = ServeProfileName::new(name)?;
            let view = profile_view(&core, store.read().await?).await?;
            if view.profiles.contains_key(&name) {
                return Err(LoadSettingsError::ProfileAlreadyExists(name).into());
            }
            let source_profile = view
                .profiles
                .get(&source)
                .ok_or_else(|| LoadSettingsError::ProfileNotFound(source.clone()))?
                .clone();
            let fork = source_profile.fork_local(name.as_str(), name.as_str());
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    state.create_profile(name.clone())?;
                    state.profiles.insert(name, fork);
                    Ok(state.clone())
                })
                .await?;
            output::profiles("duplicate", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::Delete { name } => {
            let name = ServeProfileName::new(name)?;
            let state = store
                .update(move |state| {
                    state.delete_profile(&name)?;
                    Ok(state.clone())
                })
                .await?;
            output::profiles("delete", &state, None, json_output)?;
        }
        ProfilesCommand::Set { name, settings } => {
            let name = ServeProfileName::new(name)?;
            let registry = composition::engine_registry(&core)?;
            let patch = registry.parse_load_settings(&settings)?;
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&name)
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?;
                    profile.load.settings.0.extend(patch.0);
                    Ok(state.clone())
                })
                .await?;
            output::profiles("set", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::Unset { name, settings } => {
            let name = ServeProfileName::new(name)?;
            let registry = composition::engine_registry(&core)?;
            let ids = parse_known_setting_ids(&registry, &settings)?;
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&name)
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?;
                    for id in &ids {
                        profile.load.settings.remove(id);
                    }
                    Ok(state.clone())
                })
                .await?;
            output::profiles("unset", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::SetGeneration { name, values } => {
            let name = ServeProfileName::new(name)?;
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&name)
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?;
                    ensure_mutable_profile(profile)?;
                    apply_generation_values(profile, &values)?;
                    Ok(state.clone())
                })
                .await?;
            output::profiles("set_generation", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::SetPromptRuntimeDefault { name } => {
            let name = ServeProfileName::new(name)?;
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&name)
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?;
                    ensure_mutable_profile(profile)?;
                    profile.prompt = norted_core::ServePromptProfile::default();
                    Ok(state.clone())
                })
                .await?;
            output::profiles("set_prompt", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::SetPromptExternal {
            name,
            template,
            template_id,
            template_sha256,
        } => {
            let name = ServeProfileName::new(name)?;
            let template = std::fs::canonicalize(&template).map_err(|error| {
                color_eyre::eyre::eyre!(
                    "could not resolve template {}: {error}",
                    template.display()
                )
            })?;
            let selected = name.clone();
            let state = store
                .update(move |state| {
                    let profile = state
                        .profiles
                        .get_mut(&name)
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?;
                    ensure_mutable_profile(profile)?;
                    profile.prompt = norted_core::ServePromptProfile {
                        mode: PromptMode::ExternalTemplate,
                        delivery: PromptDelivery::RawCompletions,
                        template: Some(norted_core::ExternalTemplateReference {
                            identity: template_id,
                            path: template,
                            sha256: template_sha256,
                        }),
                        render_generation_prompt: true,
                        thinking_enabled: profile.generation.thinking.default,
                        response_filter: ResponseFilter::None,
                    };
                    profile
                        .validate()
                        .map_err(LoadSettingsError::InvalidServeProfile)?;
                    Ok(state.clone())
                })
                .await?;
            output::profiles("set_prompt", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::Assign { model, name } => {
            core.ensure_model_discovery().await?;
            let model = ModelId(model);
            ensure_model(&core, &model).await?;
            let name = ServeProfileName::new(name)?;
            let selected = name.clone();
            let builder = discovered_builder_profiles(&core).await?;
            let state = store
                .update(move |state| {
                    if state.profiles.contains_key(&name) {
                        state.assign_profile(model, Some(name))?;
                    } else if builder.contains_key(&name) {
                        state.assign_builder_profile(model, name.to_string());
                    } else {
                        return Err(LoadSettingsError::ProfileNotFound(name));
                    }
                    Ok(state.clone())
                })
                .await?;
            let state = profile_view(&core, state).await?;
            output::profiles("assign", &state, Some(&selected), json_output)?;
        }
        ProfilesCommand::ClearAssignment { model } => {
            let model = ModelId(model);
            let state = store
                .update(move |state| {
                    state.assign_profile(model, None)?;
                    Ok(state.clone())
                })
                .await?;
            output::profiles("clear_assignment", &state, None, json_output)?;
        }
        ProfilesCommand::UseRecommended { model } => {
            let model = ModelId(model);
            ensure_model(&core, &model).await?;
            let state = store
                .update(move |state| {
                    state.inherit_recommended_profile(&model);
                    Ok(state.clone())
                })
                .await?;
            output::profiles("use_recommended", &state, None, json_output)?;
        }
        ProfilesCommand::Compatibility {
            name,
            model,
            runtime,
        } => {
            core.ensure_model_discovery().await?;
            let model_id = ModelId(model);
            let model = core
                .model(&model_id)
                .await
                .ok_or_else(|| color_eyre::eyre::eyre!("model `{model_id}` does not exist"))?;
            let name = ServeProfileName::new(name)?;
            let view = profile_view(&core, store.read().await?).await?;
            let profile = view
                .profiles
                .get(&name)
                .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?
                .clone();
            profile
                .basic_applicability(&model)
                .map_err(color_eyre::eyre::Report::msg)?;
            let registry = composition::engine_registry(&core)?;
            let packs = composition::runtime_pack_manager(&core, registry.clone())?;
            let runtime_id = runtime.map(RuntimeId::new).transpose()?;
            let selection = packs
                .resolve_with_profile(&model, runtime_id.as_ref(), Some(&profile))
                .await?;
            let host = packs.host_capabilities().await;
            let adapter = registry
                .get(&selection.runtime.manifest.identity.engine_id)
                .ok_or_else(|| color_eyre::eyre::eyre!("selected engine is unavailable"))?;
            let compatibility = adapter.runtime_model_compatibility(
                &selection.runtime,
                &model,
                &host,
                Some(&profile),
            );
            output::profile_compatibility(
                &profile,
                &model,
                &selection,
                &compatibility,
                json_output,
            )?;
        }
    }
    Ok(())
}

async fn discovered_builder_profiles(
    core: &ApplicationCore,
) -> Result<BTreeMap<ServeProfileName, ServeProfile>> {
    let mut profiles: BTreeMap<ServeProfileName, ServeProfile> = BTreeMap::new();
    for model in core.snapshot().await.models {
        let Some(profile) = model
            .norted_package
            .and_then(|package| package.recommended_serve_profile)
        else {
            continue;
        };
        let name = ServeProfileName::new(profile.id.clone())?;
        if let Some(existing) = profiles.get(&name)
            && existing.content_hash() != profile.content_hash()
        {
            return Err(color_eyre::eyre::eyre!(
                "discovered Builder Serve Profiles reuse `{name}` with different content"
            ));
        }
        profiles.entry(name).or_insert(profile);
    }
    Ok(profiles)
}

async fn effective_serve_profile(
    core: &ApplicationCore,
    state: &norted_core::ServeProfilesState,
    model: &norted_core::ModelArtifact,
) -> Result<Option<ServeProfile>> {
    if state.raw_profile_models.contains(&model.id) {
        return Ok(None);
    }
    if let Some(name) = state.model_assignments.get(&model.id) {
        return state
            .profiles
            .get(name)
            .map(|profile| Some(profile.clone()))
            .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()).into());
    }
    if let Some(profile_id) = state.builder_profile_assignments.get(&model.id) {
        let name = ServeProfileName::new(profile_id.clone())?;
        return discovered_builder_profiles(core)
            .await?
            .remove(&name)
            .map(Some)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "assigned Builder Serve Profile `{profile_id}` is unavailable; its source package may have moved or been removed"
                )
            });
    }
    Ok(model
        .norted_package
        .as_ref()
        .and_then(|package| package.recommended_serve_profile.clone()))
}

async fn profile_view(
    core: &ApplicationCore,
    mut state: norted_core::ServeProfilesState,
) -> Result<norted_core::ServeProfilesState> {
    for (name, profile) in discovered_builder_profiles(core).await? {
        if state.profiles.contains_key(&name) {
            return Err(color_eyre::eyre::eyre!(
                "local Serve Profile `{name}` conflicts with a discovered read-only Builder profile ID"
            ));
        }
        state.profiles.insert(name, profile);
    }
    Ok(state)
}

fn ensure_mutable_profile(profile: &ServeProfile) -> Result<(), LoadSettingsError> {
    if profile.read_only || profile.source != ServeProfileSource::UserLocal {
        return Err(LoadSettingsError::InvalidServeProfile(format!(
            "Serve Profile `{}` is read-only; duplicate/fork it before editing",
            profile.display_name
        )));
    }
    Ok(())
}

fn apply_generation_values(
    profile: &mut ServeProfile,
    values: &[String],
) -> Result<(), LoadSettingsError> {
    for raw in values {
        let (field, value) = raw.split_once('=').ok_or_else(|| {
            LoadSettingsError::InvalidServeProfile(format!(
                "generation edit `{raw}` must use FIELD=VALUE"
            ))
        })?;
        let number = |label: &str| {
            value.parse::<f64>().map_err(|_| {
                LoadSettingsError::InvalidServeProfile(format!(
                    "generation field `{label}` requires a number"
                ))
            })
        };
        match field {
            "temperature" => profile.generation.defaults.temperature = Some(number(field)?),
            "top_p" => profile.generation.defaults.top_p = Some(number(field)?),
            "top_k" => {
                profile.generation.defaults.top_k = Some(value.parse::<u64>().map_err(|_| {
                    LoadSettingsError::InvalidServeProfile(
                        "generation field `top_k` requires a non-negative integer".to_owned(),
                    )
                })?)
            }
            "min_p" => profile.generation.defaults.min_p = Some(number(field)?),
            "reasoning_effort" => {
                if value.is_empty() {
                    return Err(LoadSettingsError::InvalidServeProfile(
                        "reasoning_effort cannot be empty".to_owned(),
                    ));
                }
                profile.generation.defaults.reasoning_effort = Some(value.to_owned());
            }
            "thinking_default" => {
                profile.generation.thinking.default = parse_profile_bool(field, value)?
            }
            "thinking_required" => {
                profile.generation.thinking.required = parse_profile_bool(field, value)?
            }
            "allow_override" => {
                if !profile
                    .generation
                    .allowed_user_overrides
                    .iter()
                    .any(|candidate| candidate == value)
                {
                    profile
                        .generation
                        .allowed_user_overrides
                        .push(value.to_owned());
                }
            }
            "forbid_override" => profile
                .generation
                .allowed_user_overrides
                .retain(|candidate| candidate != value),
            _ => {
                return Err(LoadSettingsError::InvalidServeProfile(format!(
                    "unknown generation field `{field}`; expected temperature, top_p, top_k, min_p, reasoning_effort, thinking_default, thinking_required, allow_override, or forbid_override"
                )));
            }
        }
    }
    profile
        .validate()
        .map_err(LoadSettingsError::InvalidServeProfile)
}

fn parse_profile_bool(field: &str, value: &str) -> Result<bool, LoadSettingsError> {
    match value {
        "true" | "on" => Ok(true),
        "false" | "off" => Ok(false),
        _ => Err(LoadSettingsError::InvalidServeProfile(format!(
            "generation field `{field}` requires true/false"
        ))),
    }
}

async fn handle_settings(
    core: Arc<ApplicationCore>,
    command: SettingsCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        SettingsCommand::Show {
            model,
            runtime,
            profile,
        } => {
            let profile = profile.map(ServeProfileName::new).transpose()?;
            let (runtime_id, schema, resolved) =
                exact_settings_context(&core, ModelId(model), runtime, profile.as_ref()).await?;
            output::effective_settings(&runtime_id, &schema, &resolved, json_output)?;
        }
        SettingsCommand::Schema { model, runtime } => {
            let (_, schema, _) =
                exact_settings_context(&core, ModelId(model), runtime, None).await?;
            output::settings_schema(&schema, json_output)?;
        }
        SettingsCommand::Set(args) => {
            mutate_defaults(&core, args, json_output).await?;
        }
        SettingsCommand::Unset(args) => {
            unset_defaults(&core, args, json_output).await?;
        }
    }
    Ok(())
}

async fn exact_settings_context(
    core: &Arc<ApplicationCore>,
    model_id: ModelId,
    runtime: Option<String>,
    profile: Option<&ServeProfileName>,
) -> Result<(
    RuntimeId,
    norted_core::LoadSettingsSchema,
    norted_core::ResolvedLoadSettings,
)> {
    core.ensure_model_discovery().await?;
    let model = core.model(&model_id).await.ok_or_else(|| {
        color_eyre::eyre::eyre!("model `{model_id}` does not exist in the discovered registry")
    })?;
    let registry = composition::engine_registry(core)?;
    let packs = composition::runtime_pack_manager(core, registry.clone())?;
    packs.refresh_host_capabilities().await;
    let runtime = runtime.map(RuntimeId::new).transpose()?;
    let state = ServeProfilesStore::new(&core.paths).read().await?;
    let (serve_profile, local_profile) = if let Some(name) = profile {
        if name.as_str() == "none" {
            (None, None)
        } else if let Some(local) = state.profiles.get(name) {
            (Some(local.clone()), Some(name.clone()))
        } else {
            let builder = discovered_builder_profiles(core).await?;
            (
                Some(
                    builder
                        .get(name)
                        .cloned()
                        .ok_or_else(|| LoadSettingsError::ProfileNotFound(name.clone()))?,
                ),
                None,
            )
        }
    } else {
        (effective_serve_profile(core, &state, &model).await?, None)
    };
    let selection = packs
        .resolve_with_profile(&model, runtime.as_ref(), serve_profile.as_ref())
        .await
        .map_err(|error| {
            color_eyre::eyre::eyre!(
                "no compatible installed runtime is available to validate these settings: {error}"
            )
        })?;
    let runtime_id = selection.runtime.manifest.runtime_id.clone();
    let engine_id = selection.runtime.manifest.identity.engine_id.clone();
    let adapter = registry.get(&engine_id).ok_or_else(|| {
        color_eyre::eyre::eyre!("selected runtime uses unregistered engine `{engine_id}`")
    })?;
    let host = packs.host_capabilities().await;
    let schema = adapter
        .load_settings_schema(&selection.runtime, &model, &host, serve_profile.as_ref())
        .await?;
    let mut load_state = state;
    if profile.is_some() && local_profile.is_none() {
        load_state.model_assignments.remove(&model_id);
    }
    let mut resolved = load_state.resolve(
        &model_id,
        &engine_id,
        local_profile.as_ref(),
        &LoadSettingsPatch::default(),
        &core.paths.data_dir,
    )?;
    norted_core::apply_serve_profile_load_policy(serve_profile.as_ref(), &mut resolved)
        .map_err(color_eyre::eyre::Report::msg)?;
    Ok((runtime_id, schema, resolved))
}

#[derive(Clone)]
enum DefaultsScope {
    Global,
    Engine(String),
    Model(ModelId),
}

impl std::fmt::Display for DefaultsScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Global => formatter.write_str("global defaults"),
            Self::Engine(engine) => write!(formatter, "engine defaults:{engine}"),
            Self::Model(model) => write!(formatter, "model defaults:{model}"),
        }
    }
}

async fn mutate_defaults(
    core: &Arc<ApplicationCore>,
    args: SettingsMutationArgs,
    json_output: bool,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let patch = registry.parse_load_settings(&args.settings)?;
    let scope = defaults_scope(args.global, args.engine, args.model);
    validate_default_scope(core, &registry, &scope, &patch).await?;
    let label = scope.to_string();
    let state = ServeProfilesStore::new(&core.paths)
        .update(move |state| {
            default_patch_mut(state, &scope).0.extend(patch.0);
            Ok(state.clone())
        })
        .await?;
    output::settings_mutation("set", &label, &state, json_output)?;
    Ok(())
}

async fn unset_defaults(
    core: &Arc<ApplicationCore>,
    args: SettingsUnsetArgs,
    json_output: bool,
) -> Result<()> {
    let registry = composition::engine_registry(core)?;
    let ids = parse_known_setting_ids(&registry, &args.settings)?;
    let scope = defaults_scope(args.global, args.engine, args.model);
    let patch = LoadSettingsPatch(
        ids.iter()
            .cloned()
            .map(|id| (id, norted_core::LoadSettingValue::FlagEnabled))
            .collect(),
    );
    validate_default_scope(core, &registry, &scope, &patch).await?;
    let label = scope.to_string();
    let state = ServeProfilesStore::new(&core.paths)
        .update(move |state| {
            let target = default_patch_mut(state, &scope);
            for id in &ids {
                target.remove(id);
            }
            match &scope {
                DefaultsScope::Engine(engine)
                    if state
                        .engine_defaults
                        .get(engine)
                        .is_some_and(LoadSettingsPatch::is_empty) =>
                {
                    state.engine_defaults.remove(engine);
                }
                DefaultsScope::Model(model)
                    if state
                        .model_defaults
                        .get(model)
                        .is_some_and(LoadSettingsPatch::is_empty) =>
                {
                    state.model_defaults.remove(model);
                }
                _ => {}
            }
            Ok(state.clone())
        })
        .await?;
    output::settings_mutation("unset", &label, &state, json_output)?;
    Ok(())
}

fn defaults_scope(global: bool, engine: Option<String>, model: Option<String>) -> DefaultsScope {
    if global {
        DefaultsScope::Global
    } else if let Some(engine) = engine {
        DefaultsScope::Engine(engine)
    } else {
        DefaultsScope::Model(ModelId(model.expect("clap requires one settings scope")))
    }
}

fn default_patch_mut<'a>(
    state: &'a mut norted_core::ServeProfilesState,
    scope: &DefaultsScope,
) -> &'a mut LoadSettingsPatch {
    match scope {
        DefaultsScope::Global => &mut state.global_defaults,
        DefaultsScope::Engine(engine) => state.engine_defaults.entry(engine.clone()).or_default(),
        DefaultsScope::Model(model) => state.model_defaults.entry(model.clone()).or_default(),
    }
}

async fn validate_default_scope(
    core: &Arc<ApplicationCore>,
    registry: &EngineRegistry,
    scope: &DefaultsScope,
    patch: &LoadSettingsPatch,
) -> Result<()> {
    let definitions = registry
        .load_setting_definitions()?
        .into_iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect::<std::collections::BTreeMap<_, _>>();
    match scope {
        DefaultsScope::Global => {
            if let Some(id) = patch.0.keys().find(|id| id.namespace().is_some()) {
                return Err(LoadSettingsError::InvalidGlobalSetting(id.clone()).into());
            }
        }
        DefaultsScope::Engine(engine) => {
            if registry.get(engine).is_none() {
                return Err(color_eyre::eyre::eyre!("unknown engine `{engine}`"));
            }
            for id in patch.0.keys() {
                let definition = &definitions[id];
                if matches!(
                    &definition.scope,
                    LoadSettingScope::Engine { engine_id } if engine_id != engine
                ) {
                    return Err(LoadSettingsError::WrongEngineScope {
                        setting_id: id.clone(),
                        engine_id: engine.clone(),
                    }
                    .into());
                }
            }
        }
        DefaultsScope::Model(model) => {
            core.ensure_model_discovery().await?;
            ensure_model(core, model).await?;
        }
    }
    Ok(())
}

fn parse_known_setting_ids(
    registry: &EngineRegistry,
    values: &[String],
) -> Result<Vec<LoadSettingId>> {
    let known = registry
        .load_setting_definitions()?
        .into_iter()
        .map(|definition| definition.id)
        .collect::<std::collections::BTreeSet<_>>();
    values
        .iter()
        .map(|value| {
            let id = LoadSettingId::new(value.clone())?;
            if !known.contains(&id) {
                return Err(LoadSettingsError::UnknownSetting(id));
            }
            Ok(id)
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

async fn ensure_model(core: &ApplicationCore, model: &ModelId) -> Result<()> {
    if core.model(model).await.is_none() {
        return Err(color_eyre::eyre::eyre!(
            "model `{model}` does not exist in the discovered registry"
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
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "norted-server.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .init();
    guard
}
