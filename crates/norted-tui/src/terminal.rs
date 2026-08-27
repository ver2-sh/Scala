use std::io::{self, IsTerminal, Stdout, Write};
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

type PanicHook = Arc<dyn for<'a> Fn(&panic::PanicHookInfo<'a>) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, Default)]
struct TerminalState {
    raw_mode: bool,
    alternate_screen: bool,
    bracketed_paste: bool,
    cursor_hidden: bool,
    enhanced_keys: bool,
}

impl TerminalState {
    fn restore(self) -> io::Result<()> {
        let mut stdout = io::stdout();
        let mut first_error = None;
        if self.enhanced_keys {
            record(
                &mut first_error,
                execute!(stdout, PopKeyboardEnhancementFlags),
            );
        }
        if self.bracketed_paste {
            record(&mut first_error, execute!(stdout, DisableBracketedPaste));
        }
        if self.cursor_hidden {
            record(&mut first_error, execute!(stdout, Show));
        }
        if self.alternate_screen {
            record(&mut first_error, execute!(stdout, LeaveAlternateScreen));
        }
        if self.raw_mode {
            record(&mut first_error, disable_raw_mode());
        }
        let _ = stdout.flush();
        first_error.map_or(Ok(()), Err)
    }
}

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: TerminalState,
    previous_panic_hook: Option<PanicHook>,
    restored: Arc<AtomicBool>,
    active: bool,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(
                "the TUI requires an interactive terminal on stdin and stdout",
            ));
        }

        let mut state = TerminalState::default();
        enable_raw_mode()?;
        state.raw_mode = true;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = state.restore();
            return Err(error);
        }
        state.alternate_screen = true;
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = state.restore();
            return Err(error);
        }
        state.bracketed_paste = true;
        if let Err(error) = execute!(stdout, Hide) {
            let _ = state.restore();
            return Err(error);
        }
        state.cursor_hidden = true;
        state.enhanced_keys = execute!(
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
                let _ = state.restore();
                return Err(error);
            }
        };
        let previous_panic_hook: PanicHook = panic::take_hook().into();
        let hook_for_panic = previous_panic_hook.clone();
        let restored = Arc::new(AtomicBool::new(false));
        let restored_for_panic = restored.clone();
        panic::set_hook(Box::new(move |info| {
            if !restored_for_panic.swap(true, Ordering::SeqCst) {
                let _ = state.restore();
            }
            hook_for_panic(info);
        }));
        Ok(Self {
            terminal,
            state,
            previous_panic_hook: Some(previous_panic_hook),
            restored,
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
        self.restore_panic_hook();
        let result = self.restore_terminal_once();
        self.active = false;
        result
    }

    fn restore_panic_hook(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let Some(previous) = self.previous_panic_hook.take() else {
            return;
        };
        let _ = panic::take_hook();
        panic::set_hook(Box::new(move |info| previous(info)));
    }

    fn restore_terminal_once(&self) -> io::Result<()> {
        if self.restored.swap(true, Ordering::SeqCst) {
            Ok(())
        } else {
            self.state.restore()
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            self.restore_panic_hook();
            let _ = self.restore_terminal_once();
            self.active = false;
        }
    }
}

fn record(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(error) = result
        && first_error.is_none()
    {
        *first_error = Some(error);
    }
}
