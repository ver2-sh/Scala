use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use norted_core::{AppEvent, AppSnapshot, LogLevel};

use crate::commands::{self, CommandAction};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Screen {
    Overview,
    Models,
    Engines,
    Server,
    Logs,
    Settings,
    Help,
}

impl Screen {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Models,
        Self::Engines,
        Self::Server,
        Self::Logs,
        Self::Settings,
        Self::Help,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Models => "Models",
            Self::Engines => "Engines",
            Self::Server => "Server",
            Self::Logs => "Logs",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Overlay {
    Help,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Update {
    None,
    Render,
    Quit,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

pub struct App {
    pub snapshot: AppSnapshot,
    pub no_color: bool,
    pub server_address: String,
    pub config_path: String,
    pub model_paths: Vec<String>,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub command_active: bool,
    pub command_input: String,
    pub command_cursor: usize,
    pub suggestion_index: usize,
    pub notice: Option<String>,
    pub logs: Vec<LogEntry>,
}

impl App {
    pub fn new(
        snapshot: AppSnapshot,
        no_color: bool,
        server_address: String,
        config_path: String,
        model_paths: Vec<String>,
    ) -> Self {
        Self {
            snapshot,
            no_color,
            server_address,
            config_path,
            model_paths,
            screen: Screen::Overview,
            overlay: None,
            command_active: false,
            command_input: String::new(),
            command_cursor: 0,
            suggestion_index: 0,
            notice: None,
            logs: vec![LogEntry {
                level: LogLevel::Info,
                message: "Control core initialized".into(),
            }],
        }
    }

    pub fn suggestions(&self) -> Vec<&'static commands::SlashCommand> {
        commands::suggestions(&self.command_input)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Update {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Update::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Update::Quit;
        }
        if self.overlay.is_some() {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => {
                    self.overlay = None;
                    Update::Render
                }
                _ => Update::None,
            };
        }
        if self.command_active {
            return self.handle_command_key(key);
        }
        match key.code {
            KeyCode::Char('/') => {
                self.command_active = true;
                self.command_input = "/".into();
                self.command_cursor = 1;
                self.suggestion_index = 0;
                Update::Render
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            KeyCode::Char('q') => Update::Quit,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.move_screen(1);
                Update::Render
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.move_screen(-1);
                Update::Render
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_screen(1);
                Update::Render
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_screen(-1);
                Update::Render
            }
            _ => Update::None,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> Update {
        if !self.command_active {
            return Update::None;
        }
        let normalized = text.replace(['\r', '\n', '\t'], " ");
        self.insert_text(&normalized);
        Update::Render
    }

    pub fn handle_core_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Log { level, message } => self.push_log(level, message),
            AppEvent::RegistryRefreshed { model_count } => self.push_log(
                LogLevel::Info,
                format!("Model registry refreshed: {model_count} discovered"),
            ),
            AppEvent::ServerChanged(state) => self.push_log(
                LogLevel::Info,
                format!("API server state changed to {}", state.label()),
            ),
            AppEvent::ModelDiscovered(_) => {}
        }
    }

    fn push_log(&mut self, level: LogLevel, message: String) {
        self.logs.push(LogEntry { level, message });
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.command_active = false;
                self.command_input.clear();
                self.command_cursor = 0;
                Update::Render
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Up => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = self.suggestion_index.saturating_sub(1);
                }
                Update::Render
            }
            KeyCode::Down | KeyCode::Tab => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = (self.suggestion_index + 1).min(len - 1);
                }
                Update::Render
            }
            KeyCode::BackTab => {
                self.suggestion_index = self.suggestion_index.saturating_sub(1);
                Update::Render
            }
            KeyCode::Backspace => {
                if self.command_cursor > 0 {
                    self.command_cursor -= 1;
                    let start = byte_index(&self.command_input, self.command_cursor);
                    let end = byte_index(&self.command_input, self.command_cursor + 1);
                    self.command_input.replace_range(start..end, "");
                }
                self.suggestion_index = 0;
                Update::Render
            }
            KeyCode::Delete => {
                if self.command_cursor < self.command_input.chars().count() {
                    let start = byte_index(&self.command_input, self.command_cursor);
                    let end = byte_index(&self.command_input, self.command_cursor + 1);
                    self.command_input.replace_range(start..end, "");
                }
                Update::Render
            }
            KeyCode::Left => {
                self.command_cursor = self.command_cursor.saturating_sub(1);
                Update::Render
            }
            KeyCode::Right => {
                self.command_cursor =
                    (self.command_cursor + 1).min(self.command_input.chars().count());
                Update::Render
            }
            KeyCode::Home => {
                self.command_cursor = 0;
                Update::Render
            }
            KeyCode::End => {
                self.command_cursor = self.command_input.chars().count();
                Update::Render
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_text(&character.to_string());
                self.suggestion_index = 0;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn insert_text(&mut self, text: &str) {
        let index = byte_index(&self.command_input, self.command_cursor);
        self.command_input.insert_str(index, text);
        self.command_cursor += text.chars().count();
    }

    fn submit_command(&mut self) -> Update {
        let selected = self
            .suggestions()
            .get(self.suggestion_index)
            .copied()
            .or_else(|| commands::exact(&self.command_input));
        let Some(command) = selected else {
            self.notice = Some(format!("Unknown command: {}", self.command_input.trim()));
            self.command_active = false;
            self.command_input.clear();
            self.command_cursor = 0;
            return Update::Render;
        };
        self.command_active = false;
        self.command_input.clear();
        self.command_cursor = 0;
        self.suggestion_index = 0;
        self.notice = None;
        match command.action {
            CommandAction::Navigate(screen) => {
                self.screen = screen;
                Update::Render
            }
            CommandAction::ShowHelp => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            CommandAction::Quit => Update::Quit,
        }
    }

    fn move_screen(&mut self, direction: isize) {
        let current = Screen::ALL
            .iter()
            .position(|screen| *screen == self.screen)
            .unwrap_or_default() as isize;
        let count = Screen::ALL.len() as isize;
        let next = (current + direction).rem_euclid(count) as usize;
        self.screen = Screen::ALL[next];
        self.notice = None;
    }
}

fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}
