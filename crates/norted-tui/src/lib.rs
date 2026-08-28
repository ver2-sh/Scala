//! Interactive terminal interface for Norted Server.

mod app;
mod commands;
mod terminal;
mod theme;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use norted_core::{AppPaths, ApplicationCore};
use norted_engine::{ControlClient, ControlStatus};

use app::{App, ControlAction, Update};
use terminal::TerminalSession;
use ui::layout::UiLayout;

pub async fn run(core: Arc<ApplicationCore>) -> Result<()> {
    let initial_control = observe_control(&core.paths).await;
    let mut terminal = TerminalSession::enter()?;
    let mut core_events = core.subscribe();
    core.start_model_discovery().await;
    let snapshot = core.snapshot().await;
    let server_address = format!("{}:{}", core.config.server.host, core.config.server.port);
    let config_path = core.config_path.display().to_string();
    let model_paths = core
        .config
        .models
        .paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let mut app = App::new(
        snapshot,
        core.config.tui.no_color,
        core.config.tui.unicode,
        server_address,
        config_path,
        model_paths,
    );
    app.replace_control(initial_control.status, initial_control.error);
    let mut terminal_events = EventStream::new();
    let (control_updates, mut control_update_receiver) = tokio::sync::mpsc::channel(2);
    let (control_results, mut control_result_receiver) = tokio::sync::mpsc::channel(2);
    let observer_core = Arc::clone(&core);
    let observer_paths = core.paths.clone();
    let runtime_observer = tokio::spawn(async move {
        let mut refresh = tokio::time::interval(Duration::from_secs(2));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            refresh.tick().await;
            let _ = observer_core.refresh_server_state().await;
            if control_updates
                .send(observe_control(&observer_paths).await)
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let mut render = true;
    let mut layout = UiLayout::default();

    loop {
        if render {
            terminal.draw(|frame| layout = ui::render(frame, &app))?;
        }
        let update = tokio::select! {
            event = terminal_events.next() => match event {
                Some(Ok(Event::Key(key))) => app.handle_key(key, &layout),
                Some(Ok(Event::Mouse(mouse))) => app.handle_mouse(mouse, &layout),
                Some(Ok(Event::Paste(text))) => app.handle_paste(&text),
                Some(Ok(Event::Resize(_, _))) => {
                    app.clear_hover();
                    Update::Render
                },
                Some(Ok(_)) => Update::None,
                Some(Err(error)) => return Err(error.into()),
                None => Update::Quit,
            },
            event = core_events.recv() => match event {
                Ok(event) => {
                    app.replace_snapshot(core.snapshot().await);
                    app.handle_core_event(event);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    app.replace_snapshot(core.snapshot().await);
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            observation = control_update_receiver.recv() => match observation {
                Some(observation) => {
                    app.replace_control(observation.status, observation.error);
                    Update::Render
                }
                None => Update::None,
            },
            result = control_result_receiver.recv() => match result {
                Some(result) => {
                    app.handle_control_result(result);
                    Update::Render
                }
                None => Update::None,
            },
        };
        if update == Update::Quit {
            break;
        }
        if let Some(action) = app.take_control_action() {
            let paths = core.paths.clone();
            let results = control_results.clone();
            tokio::spawn(async move {
                let result = execute_control(&paths, action).await;
                let _ = results.send(result).await;
            });
        }
        render = update == Update::Render;
    }
    runtime_observer.abort();
    terminal.leave()?;
    Ok(())
}

struct ControlObservation {
    status: Option<ControlStatus>,
    error: Option<String>,
}

async fn observe_control(paths: &AppPaths) -> ControlObservation {
    match ControlClient::discover(paths).await {
        Ok(client) => match client.status().await {
            Ok(status) => ControlObservation {
                status: Some(status),
                error: None,
            },
            Err(error) => ControlObservation {
                status: None,
                error: Some(error.to_string()),
            },
        },
        Err(error) => ControlObservation {
            status: None,
            error: Some(error.to_string()),
        },
    }
}

async fn execute_control(
    paths: &AppPaths,
    action: ControlAction,
) -> std::result::Result<ControlStatus, String> {
    let client = ControlClient::discover(paths)
        .await
        .map_err(|error| error.to_string())?;
    match action {
        ControlAction::Load(model_id) => client.load(model_id).await,
        ControlAction::Unload => client.unload().await,
    }
    .map_err(|error| error.to_string())
}
