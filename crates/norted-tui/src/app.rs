use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use norted_core::{AppEvent, AppSnapshot, LogLevel, RegistryState};
use ratatui::layout::Position;

use crate::commands::{self, CommandAction};
use crate::ui::layout::{HoverTarget, UiLayout};

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
pub enum FocusArea {
    Navigation,
    Content,
    Command,
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
    pub unicode: bool,
    pub server_address: String,
    pub config_path: String,
    pub model_paths: Vec<String>,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub command_active: bool,
    pub command_input: String,
    pub command_cursor: usize,
    pub suggestion_index: usize,
    pub suggestion_scroll: usize,
    pub notice: Option<String>,
    pub logs: Vec<LogEntry>,
    pub focus: FocusArea,
    pub nav_focus: Screen,
    pub hover: Option<HoverTarget>,
    pub selected_model: Option<usize>,
    pub model_scroll: usize,
    pub log_scroll: usize,
    focus_before_command: FocusArea,
}

impl App {
    pub fn new(
        snapshot: AppSnapshot,
        no_color: bool,
        unicode: bool,
        server_address: String,
        config_path: String,
        model_paths: Vec<String>,
    ) -> Self {
        let mut logs = vec![LogEntry {
            level: LogLevel::Info,
            message: "Control core initialized".into(),
        }];
        logs.extend(
            snapshot
                .registry_warnings
                .iter()
                .cloned()
                .map(|message| LogEntry {
                    level: LogLevel::Warning,
                    message,
                }),
        );
        Self {
            snapshot,
            no_color,
            unicode,
            server_address,
            config_path,
            model_paths,
            screen: Screen::Overview,
            overlay: None,
            command_active: false,
            command_input: String::new(),
            command_cursor: 0,
            suggestion_index: 0,
            suggestion_scroll: 0,
            notice: None,
            logs,
            focus: FocusArea::Content,
            nav_focus: Screen::Overview,
            hover: None,
            selected_model: None,
            model_scroll: 0,
            log_scroll: 0,
            focus_before_command: FocusArea::Content,
        }
    }

    pub fn suggestions(&self) -> Vec<&'static commands::SlashCommand> {
        commands::suggestions(&self.command_input)
    }

    pub fn handle_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
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
                self.open_command(true);
                self.command_input = "/".into();
                self.command_cursor = 1;
                Update::Render
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            KeyCode::Char('q') => Update::Quit,
            KeyCode::Tab => {
                self.cycle_focus(1);
                Update::Render
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                Update::Render
            }
            _ => match self.focus {
                FocusArea::Navigation => self.handle_navigation_key(key),
                FocusArea::Content => self.handle_content_key(key, layout),
                FocusArea::Command => {
                    if key.code == KeyCode::Enter {
                        self.open_command(false);
                        Update::Render
                    } else {
                        Update::None
                    }
                }
            },
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, layout: &UiLayout) -> Update {
        if self.overlay.is_some() {
            if self.hover.take().is_some() {
                return Update::Render;
            }
            return Update::None;
        }
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved => {
                let hover = layout.hit_test(position);
                if hover == self.hover {
                    Update::None
                } else {
                    self.hover = hover;
                    Update::Render
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.handle_click(position, layout),
            MouseEventKind::ScrollUp => self.handle_wheel(position, layout, -1),
            MouseEventKind::ScrollDown => self.handle_wheel(position, layout, 1),
            _ => Update::None,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> Update {
        if !self.command_active {
            return Update::None;
        }
        let normalized = text.replace(['\r', '\n', '\t'], " ");
        self.insert_text(&normalized);
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        Update::Render
    }

    pub fn handle_core_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Log { level, message } => self.push_log(level, message),
            AppEvent::RegistryChanged(state) => match state {
                RegistryState::NotScanned => {}
                RegistryState::Scanning => {
                    self.push_log(LogLevel::Info, "Model registry scan started".into())
                }
                RegistryState::Ready => self.push_log(
                    LogLevel::Info,
                    format!(
                        "Model registry ready: {} discovered",
                        self.snapshot.models.len()
                    ),
                ),
                RegistryState::ReadyWithWarnings { warning_count } => {
                    self.push_log(
                        LogLevel::Warning,
                        format!("Model registry ready with {warning_count} warning(s)"),
                    );
                    for warning in self.snapshot.registry_warnings.clone() {
                        self.push_log(LogLevel::Warning, warning);
                    }
                }
                RegistryState::Failed { message } => self.push_log(
                    LogLevel::Error,
                    format!("Model registry scan failed: {message}"),
                ),
            },
            AppEvent::ServerChanged(state) => self.push_log(
                LogLevel::Info,
                format!("API server state changed to {}", state.label()),
            ),
        }
        self.reconcile_models();
    }

    pub fn replace_snapshot(&mut self, snapshot: AppSnapshot) {
        self.snapshot = snapshot;
        self.reconcile_models();
    }

    pub fn clear_hover(&mut self) {
        self.hover = None;
    }

    fn push_log(&mut self, level: LogLevel, message: String) {
        if self.log_scroll > 0 {
            self.log_scroll = self.log_scroll.saturating_add(1);
        }
        self.logs.push(LogEntry { level, message });
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Esc => {
                self.close_command();
                Update::Render
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Up => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = self.suggestion_index.saturating_sub(1);
                    self.ensure_suggestion_visible();
                }
                Update::Render
            }
            KeyCode::Down | KeyCode::Tab => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion_index = (self.suggestion_index + 1).min(len - 1);
                    self.ensure_suggestion_visible();
                }
                Update::Render
            }
            KeyCode::BackTab => {
                self.suggestion_index = self.suggestion_index.saturating_sub(1);
                self.ensure_suggestion_visible();
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
                self.suggestion_scroll = 0;
                Update::Render
            }
            KeyCode::Delete => {
                if self.command_cursor < self.command_input.chars().count() {
                    let start = byte_index(&self.command_input, self.command_cursor);
                    let end = byte_index(&self.command_input, self.command_cursor + 1);
                    self.command_input.replace_range(start..end, "");
                }
                self.suggestion_index = 0;
                self.suggestion_scroll = 0;
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
                self.suggestion_scroll = 0;
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
            self.close_command();
            return Update::Render;
        };
        self.close_command();
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        self.notice = None;
        match command.action {
            CommandAction::Navigate(screen) => {
                self.screen = screen;
                self.nav_focus = screen;
                self.focus = FocusArea::Content;
                self.hover = None;
                Update::Render
            }
            CommandAction::ShowHelp => {
                self.overlay = Some(Overlay::Help);
                Update::Render
            }
            CommandAction::Quit => Update::Quit,
        }
    }

    fn move_nav_focus(&mut self, direction: isize) {
        let current = Screen::ALL
            .iter()
            .position(|screen| *screen == self.nav_focus)
            .unwrap_or_default() as isize;
        let count = Screen::ALL.len() as isize;
        let next = (current + direction).rem_euclid(count) as usize;
        self.nav_focus = Screen::ALL[next];
    }

    fn ensure_suggestion_visible(&mut self) {
        const VISIBLE_SUGGESTIONS: usize = 8;
        if self.suggestion_index < self.suggestion_scroll {
            self.suggestion_scroll = self.suggestion_index;
        } else if self.suggestion_index >= self.suggestion_scroll + VISIBLE_SUGGESTIONS {
            self.suggestion_scroll = self.suggestion_index + 1 - VISIBLE_SUGGESTIONS;
        }
    }

    fn open_command(&mut self, reset: bool) {
        if !self.command_active {
            self.focus_before_command = self.focus;
        }
        self.command_active = true;
        self.focus = FocusArea::Command;
        if reset {
            self.command_input.clear();
            self.command_cursor = 0;
        }
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
    }

    fn close_command(&mut self) {
        self.command_active = false;
        self.command_input.clear();
        self.command_cursor = 0;
        self.suggestion_index = 0;
        self.suggestion_scroll = 0;
        self.focus = self.focus_before_command;
    }

    fn cycle_focus(&mut self, direction: isize) {
        let current = match self.focus {
            FocusArea::Navigation => 0_isize,
            FocusArea::Content => 1,
            FocusArea::Command => 2,
        };
        self.focus = match (current + direction).rem_euclid(3) {
            0 => FocusArea::Navigation,
            1 => FocusArea::Content,
            _ => FocusArea::Command,
        };
    }

    fn handle_navigation_key(&mut self, key: KeyEvent) -> Update {
        match key.code {
            KeyCode::Left => {
                self.move_nav_focus(-1);
                Update::Render
            }
            KeyCode::Right => {
                self.move_nav_focus(1);
                Update::Render
            }
            KeyCode::Enter => {
                self.screen = self.nav_focus;
                self.notice = None;
                Update::Render
            }
            _ => Update::None,
        }
    }

    fn handle_content_key(&mut self, key: KeyEvent, layout: &UiLayout) -> Update {
        match self.screen {
            Screen::Models => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_model_selection(-1, layout),
                KeyCode::Down | KeyCode::Char('j') => self.move_model_selection(1, layout),
                KeyCode::PageUp => self.scroll_models(-(layout.model_capacity() as isize), layout),
                KeyCode::PageDown => self.scroll_models(layout.model_capacity() as isize, layout),
                KeyCode::Home => self.select_model(0, layout),
                KeyCode::End if !self.snapshot.models.is_empty() => {
                    self.select_model(self.snapshot.models.len() - 1, layout)
                }
                _ => Update::None,
            },
            Screen::Logs => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_logs(1, layout),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_logs(-1, layout),
                KeyCode::PageUp => self.scroll_logs(layout.log_capacity() as isize, layout),
                KeyCode::PageDown => self.scroll_logs(-(layout.log_capacity() as isize), layout),
                KeyCode::Home => {
                    self.log_scroll = self.logs.len().saturating_sub(layout.log_capacity());
                    Update::Render
                }
                KeyCode::End => {
                    self.log_scroll = 0;
                    Update::Render
                }
                _ => Update::None,
            },
            _ => Update::None,
        }
    }

    fn handle_click(&mut self, position: Position, layout: &UiLayout) -> Update {
        match layout.hit_test(position) {
            Some(HoverTarget::Navigation(screen)) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Navigation;
                self.nav_focus = screen;
                self.screen = screen;
                self.notice = None;
                Update::Render
            }
            Some(HoverTarget::Model(index)) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Content;
                self.selected_model = Some(index);
                Update::Render
            }
            Some(HoverTarget::CommandSuggestion(index)) => {
                self.suggestion_index = index;
                self.submit_command()
            }
            Some(HoverTarget::CommandBar) => {
                self.open_command(false);
                Update::Render
            }
            None if layout.contains_content(position) => {
                if self.command_active {
                    self.close_command();
                }
                self.focus = FocusArea::Content;
                Update::Render
            }
            None => Update::None,
        }
    }

    fn handle_wheel(&mut self, position: Position, layout: &UiLayout, direction: isize) -> Update {
        if self.command_active && layout.contains_suggestions(position) {
            let len = self.suggestions().len();
            if len == 0 {
                return Update::None;
            }
            self.suggestion_index = if direction < 0 {
                self.suggestion_index.saturating_sub(1)
            } else {
                (self.suggestion_index + 1).min(len - 1)
            };
            self.ensure_suggestion_visible();
            return Update::Render;
        }
        if !layout.contains_content(position) {
            return Update::None;
        }
        self.focus = FocusArea::Content;
        match self.screen {
            Screen::Models => self.scroll_models(direction * 3, layout),
            Screen::Logs => self.scroll_logs(direction * -3, layout),
            _ => Update::None,
        }
    }

    fn move_model_selection(&mut self, direction: isize, layout: &UiLayout) -> Update {
        if self.snapshot.models.is_empty() {
            return Update::None;
        }
        let next = self.selected_model.map_or(0, |current| {
            (current as isize + direction).clamp(0, self.snapshot.models.len() as isize - 1)
                as usize
        });
        self.select_model(next, layout)
    }

    fn select_model(&mut self, index: usize, layout: &UiLayout) -> Update {
        if self.snapshot.models.is_empty() {
            return Update::None;
        }
        let index = index.min(self.snapshot.models.len() - 1);
        self.selected_model = Some(index);
        let capacity = layout.model_capacity().max(1);
        if index < self.model_scroll {
            self.model_scroll = index;
        } else if index >= self.model_scroll + capacity {
            self.model_scroll = index + 1 - capacity;
        }
        Update::Render
    }

    fn scroll_models(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let capacity = layout.model_capacity().max(1);
        let max_scroll = self.snapshot.models.len().saturating_sub(capacity);
        self.model_scroll = if amount < 0 {
            self.model_scroll.saturating_sub(amount.unsigned_abs())
        } else {
            self.model_scroll
                .saturating_add(amount as usize)
                .min(max_scroll)
        };
        Update::Render
    }

    fn scroll_logs(&mut self, amount: isize, layout: &UiLayout) -> Update {
        let max_scroll = self.logs.len().saturating_sub(layout.log_capacity());
        let current = self.log_scroll.min(max_scroll);
        self.log_scroll = if amount < 0 {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize).min(max_scroll)
        };
        Update::Render
    }

    fn reconcile_models(&mut self) {
        let len = self.snapshot.models.len();
        self.selected_model = self
            .selected_model
            .and_then(|index| (len > 0).then(|| index.min(len - 1)));
        self.model_scroll = self.model_scroll.min(len.saturating_sub(1));
    }
}

fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}
