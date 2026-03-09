//! Shared TUI framework for dashboards.
//!
//! Provides a [`Dashboard`] trait, terminal lifecycle management ([`TerminalGuard`]),
//! and a generic event loop ([`run_dashboard_sync`]) that any dashboard can reuse.

// TODO: Remove once the download dashboard migrates to this framework (Task 3).
#![allow(dead_code)]

use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Type alias for the concrete terminal backend used throughout the TUI.
pub type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// RAII guard that restores the terminal on drop (even during panic).
///
/// Disables raw mode, leaves the alternate screen, and re-shows the cursor.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = io::stdout().execute(Show);
    }
}

/// Trait for a TUI dashboard that can be driven by `run_dashboard_sync`.
pub trait Dashboard {
    /// Render the current state to the terminal frame.
    fn draw(&self, frame: &mut ratatui::Frame);

    /// Handle a key event. Returns `true` if the key was consumed.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool;

    /// Returns `true` when all background work is complete and the dashboard
    /// should exit after the final render.
    fn is_done(&self) -> bool;

    /// Returns `true` when the user has requested to quit (e.g. pressed `q`).
    fn quit_requested(&self) -> bool;
}

/// Check for a TTY, enable raw mode, enter the alternate screen, and return
/// a ready-to-use terminal and its cleanup guard.
///
/// The `TerminalGuard` restores the terminal on drop — callers don't need
/// explicit cleanup.
pub fn setup_terminal() -> anyhow::Result<(Term, TerminalGuard)> {
    if !io::stdout().is_terminal() {
        anyhow::bail!(
            "Dashboard mode requires an interactive terminal.\n\
             Hint: remove --dashboard when piping output or running without a TTY."
        );
    }

    enable_raw_mode()?;
    let guard = TerminalGuard;

    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    Ok((terminal, guard))
}

/// Run a synchronous (blocking) event loop for a `Dashboard`.
///
/// Renders at `tick_rate`, polls for keyboard input, and exits when the
/// dashboard reports `is_done()` or `quit_requested()`.
///
/// This is a blocking call — it returns only when the dashboard is finished.
/// For async dashboards that spawn background work, call this from a
/// `tokio::task::spawn_blocking` or after spawning async tasks.
pub fn run_dashboard_sync(
    terminal: &mut Term,
    dashboard: &mut dyn Dashboard,
    tick_rate: Duration,
) -> anyhow::Result<()> {
    let mut input_disabled = false;

    loop {
        // Draw
        terminal.draw(|f| dashboard.draw(f))?;

        // Check exit conditions after drawing (so the final state is visible)
        if dashboard.quit_requested() || dashboard.is_done() {
            break;
        }

        // Handle input
        if !input_disabled {
            match event::poll(tick_rate) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => {
                        dashboard.handle_key(key.code, key.modifiers);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        // Input reader failed — continue rendering without input.
                        input_disabled = true;
                    }
                },
                Ok(false) => {}
                Err(_) => {
                    // Poll failed — disable input and sleep to avoid busy-loop.
                    input_disabled = true;
                    std::thread::sleep(tick_rate);
                }
            }
        } else {
            std::thread::sleep(tick_rate);
        }
    }

    Ok(())
}
