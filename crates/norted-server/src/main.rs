mod cli;
mod composition;
mod doctor;
mod output;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Command, ConfigCommand, EnginesCommand, ModelsCommand};
use color_eyre::Result;
use norted_api::ApiServer;
use norted_core::{AppPaths, ApplicationCore, ModelId};
use norted_engine::ControlClient;
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
            norted_tui::run(core).await?;
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
        Command::Load { model_id } => {
            let client = ControlClient::discover(&core.paths).await?;
            let status = client.load(ModelId(model_id)).await?;
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
