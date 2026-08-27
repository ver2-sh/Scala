use std::io::{self, IsTerminal, Stdout, Write};
use std::panic;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    enhanced_keys: bool,
    active: bool,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(
                "the TUI requires an interactive terminal on stdin and stdout",
            ));
        }
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let enhanced_keys = execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
            )
        )
        .is_ok();
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore_terminal(enhanced_keys);
                return Err(error);
            }
        };
        install_panic_restore(enhanced_keys);
        Ok(Self {
            terminal,
            enhanced_keys,
            active: true,
        })
    }

    pub fn draw(&mut self, draw: impl FnOnce(&mut ratatui::Frame<'_>)) -> io::Result<()> {
        self.terminal.draw(draw).map(|_| ())
    }

    pub fn leave(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let _ = panic::take_hook();
        restore_terminal(self.enhanced_keys)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = panic::take_hook();
            let _ = restore_terminal(self.enhanced_keys);
            self.active = false;
        }
    }
}

fn install_panic_restore(enhanced_keys: bool) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal(enhanced_keys);
        previous(info);
    }));
}

fn restore_terminal(enhanced_keys: bool) -> io::Result<()> {
    let mut stdout = io::stdout();
    if enhanced_keys {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let execute_result = execute!(stdout, DisableBracketedPaste, Show, LeaveAlternateScreen);
    let raw_result = disable_raw_mode();
    let _ = stdout.flush();
    execute_result.and(raw_result)
}
