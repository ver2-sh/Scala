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
use norted_core::ApplicationCore;

use app::{App, Update};
use terminal::TerminalSession;

pub async fn run(core: Arc<ApplicationCore>) -> Result<()> {
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
    let mut terminal = TerminalSession::enter()?;
    let mut terminal_events = EventStream::new();
    let mut core_events = core.subscribe();
    let mut runtime_refresh = tokio::time::interval(Duration::from_secs(2));
    runtime_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut render = true;

    loop {
        if render {
            terminal.draw(|frame| ui::render(frame, &app))?;
        }
        let update = tokio::select! {
            event = terminal_events.next() => match event {
                Some(Ok(Event::Key(key))) => app.handle_key(key),
                Some(Ok(Event::Paste(text))) => app.handle_paste(&text),
                Some(Ok(Event::Resize(_, _))) => Update::Render,
                Some(Ok(_)) => Update::None,
                Some(Err(error)) => return Err(error.into()),
                None => Update::Quit,
            },
            event = core_events.recv() => match event {
                Ok(event) => {
                    app.handle_core_event(event);
                    app.snapshot = core.snapshot().await;
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    app.snapshot = core.snapshot().await;
                    Update::Render
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => Update::None,
            },
            _ = runtime_refresh.tick() => {
                let previous = app.snapshot.server.clone();
                core.refresh_server_state().await;
                app.snapshot = core.snapshot().await;
                if app.snapshot.server == previous { Update::None } else { Update::Render }
            },
        };
        if update == Update::Quit {
            break;
        }
        render = update == Update::Render;
    }
    terminal.leave()?;
    Ok(())
}
