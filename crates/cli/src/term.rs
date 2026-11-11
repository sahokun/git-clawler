//! RAII terminal guard for `watch` mode.
//!
//! [`TerminalGuard::enter`] sets up the terminal for interactive use and
//! returns a guard that restores it unconditionally on drop:
//!
//! - Enables crossterm raw mode (disables line buffering / echo)
//! - Enters the alternate screen buffer (prevents output accumulating in scrollback)
//! - Hides the cursor
//! - Installs a panic hook that disables raw mode, leaves the alternate screen,
//!   and shows the cursor before the original hook runs, so the terminal is
//!   usable even after a panic
//!
//! [`TerminalGuard`]'s `Drop` impl runs on every exit path — normal return,
//! `q` / Ctrl-C / SIGTERM break, and panic — ensuring the calling shell is
//! always restored to a sane state.

use std::io::IsTerminal;
use std::sync::Arc;

use crossterm::{cursor, execute, terminal};

#[allow(clippy::incompatible_msrv)]
type PanicHookFn = dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static;

pub struct TerminalGuard {
    raw_mode: bool,
    alternate_screen: bool,
    cursor_hidden: bool,
    prev_hook: Option<Arc<PanicHookFn>>,
}

impl TerminalGuard {
    pub fn enter() -> anyhow::Result<Self> {
        if !std::io::stdout().is_terminal() {
            anyhow::bail!("watch requires a TTY");
        }

        terminal::enable_raw_mode()?;
        // Construct guard immediately so Drop cleans up if subsequent steps fail.
        let mut guard = Self {
            raw_mode: true,
            alternate_screen: false,
            cursor_hidden: false,
            prev_hook: None,
        };

        // Enter alternate screen before installing the panic hook so that the
        // hook can always issue LeaveAlternateScreen regardless of ordering.
        let mut stdout = std::io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen)?;
        guard.alternate_screen = true;

        let prev_box = std::panic::take_hook();
        let prev: Arc<PanicHookFn> = Arc::from(prev_box);
        let prev_for_hook = Arc::clone(&prev);
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            let mut stdout = std::io::stdout();
            let _ = execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen);
            prev_for_hook(info);
        }));
        guard.prev_hook = Some(prev);

        execute!(stdout, cursor::Hide)?;
        guard.cursor_hidden = true;

        Ok(guard)
    }

    pub fn is_tty() -> bool {
        std::io::stdout().is_terminal()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        if self.cursor_hidden {
            let _ = execute!(stdout, cursor::Show);
        }
        if self.alternate_screen {
            let _ = execute!(stdout, terminal::LeaveAlternateScreen);
        }
        if self.raw_mode {
            let _ = terminal::disable_raw_mode();
        }
        if let Some(prev) = self.prev_hook.take() {
            // Restore the previous panic hook only when we are NOT already
            // unwinding due to a panic.  `std::panic::set_hook` itself panics
            // when called from a panicking thread, which would cause a
            // double-panic and convert the original panic into an abort.
            // The raw-mode / cursor-show / alternate-screen cleanup above is
            // safe in both cases.
            if !std::thread::panicking() {
                std::panic::set_hook(Box::new(move |info| prev(info)));
            }
        }
    }
}
