mod cli;
mod composition;
mod doctor;
mod output;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Command, ConfigCommand, EnginesCommand, ModelsCommand, RuntimesCommand};
use color_eyre::Result;
use norted_api::ApiServer;
use norted_core::{AppPaths, ApplicationCore, ModelId, RuntimeId};
use norted_engine::{ControlClient, ControlClientError};
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
            let registry = composition::engine_registry(&core)?;
            let packs = composition::runtime_pack_manager(&core, registry)?;
            norted_tui::run(core, packs).await?;
        }
        Command::Serve => {
            core.ensure_model_discovery().await?;
            let runtime = composition::runtime_manager(Arc::clone(&core)).await?;
            let server = ApiServer::bind(core, runtime).await?;
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
            server.run(shutdown_signal()).await?;
        }
        Command::Status => output::status(core, cli.json).await?,
        Command::Load { model_id, runtime } => {
            let client = ControlClient::discover(&core.paths).await?;
            let runtime = runtime.map(RuntimeId::new).transpose()?;
            let status = client.load_with_runtime(ModelId(model_id), runtime).await?;
            output::control_operation("load", &status, cli.json)?;
        }
        Command::Unload => {
            let client = ControlClient::discover(&core.paths).await?;
            let status = client.unload().await?;
            output::control_operation("unload", &status, cli.json)?;
        }
        Command::Models(args) => match args.command {
            ModelsCommand::List => output::models(core, cli.json).await?,
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
        Command::Config(args) => match args.command {
            ConfigCommand::Show => output::config(core, cli.json)?,
        },
        Command::Doctor => unreachable!("doctor is dispatched before application startup"),
    }
    Ok(ExitCode::SUCCESS)
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
