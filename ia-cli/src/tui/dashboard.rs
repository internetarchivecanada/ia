//! Multi-tab dashboard coordinator.
//!
//! Implements the [`Dashboard`] trait from `framework.rs`, wrapping all four
//! tabs and rendering the shared header, tab bar, and footer.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::Frame;

use super::errors_tab::ErrorsTab;
use super::framework::Dashboard;
use super::help;
use super::joblog_state::JoblogState;
use super::log_tab::LogTab;
use super::s3_state::S3TaskState;
use super::tab::TabId;
use super::tasks_tab::TasksTab;
use super::theme::Theme;
use super::upload_app::{UploadItemStatus, UploadTuiState};
use super::upload_tab::UploadTab;
use super::widgets;

pub struct MultiTabDashboard {
    pub active_tab: TabId,
    pub show_help: bool,
    quit: bool,
    theme: Theme,

    // Shared state
    upload_state: Arc<Mutex<UploadTuiState>>,
    s3_state: Arc<Mutex<S3TaskState>>,

    // Tabs
    upload_tab: UploadTab,
    tasks_tab: TasksTab,
    log_tab: LogTab,
    errors_tab: ErrorsTab,
}

impl MultiTabDashboard {
    pub fn new(
        upload_state: Arc<Mutex<UploadTuiState>>,
        s3_state: Arc<Mutex<S3TaskState>>,
        joblog_state: Arc<Mutex<JoblogState>>,
    ) -> Self {
        let theme = Theme::detect();
        Self {
            active_tab: TabId::Upload,
            show_help: false,
            quit: false,
            theme,
            upload_state: upload_state.clone(),
            s3_state: s3_state.clone(),
            upload_tab: UploadTab::new(upload_state.clone(), s3_state.clone()),
            tasks_tab: TasksTab::new(s3_state.clone()),
            log_tab: LogTab::new(joblog_state),
            errors_tab: ErrorsTab::new(upload_state, s3_state),
        }
    }

    /// Check if the active tab is in a mode that consumes all input (e.g., search).
    fn is_tab_consuming_input(&self) -> bool {
        match self.active_tab {
            TabId::Tasks => self.tasks_tab.search.is_active(),
            TabId::Log => self.log_tab.search.is_active(),
            _ => false,
        }
    }

    /// Tick all tabs for periodic work (joblog tailing, gg timeout, etc.).
    pub fn tick_all(&mut self) {
        use super::tab::TabView;
        self.upload_tab.tick();
        self.tasks_tab.tick();
        self.log_tab.tick();
        self.errors_tab.tick();
    }
}

impl Dashboard for MultiTabDashboard {
    fn tick(&mut self) {
        self.tick_all();
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();

        // Layout: header(1) + tab_bar(1) + spacer(1) + content(fill) + footer(1)
        let chunks = Layout::vertical([
            Constraint::Length(1), // header
            Constraint::Length(1), // tab bar
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // content
            Constraint::Length(1), // footer
        ])
        .split(area);

        // Propagate rate-limit status from upload items to S3TaskState
        {
            let state = self.upload_state.lock().unwrap();
            let any_rate_limited = state
                .items
                .iter()
                .any(|i| matches!(i.status, UploadItemStatus::RateLimited));
            self.s3_state
                .lock()
                .unwrap()
                .set_rate_limited(any_rate_limited);
        }

        // Draw header
        let state = self.upload_state.lock().unwrap();
        let items_done = state
            .items
            .iter()
            .filter(|i| matches!(i.status, UploadItemStatus::Complete))
            .count();
        let items_total = state.items.len();
        let bytes = widgets::format_bytes(state.bytes_uploaded);
        let speed = format!(
            "{}/s",
            widgets::format_bytes(state.throughput.throughput() as u64)
        );
        let eta = widgets::format_eta(
            state.bytes_total.saturating_sub(state.bytes_uploaded),
            state.throughput.throughput(),
        );
        drop(state);

        widgets::draw_header(
            frame,
            chunks[0],
            &self.theme,
            "ia upload",
            items_done,
            items_total,
            &bytes,
            &speed,
            &eta,
        );
        widgets::draw_tab_bar(frame, chunks[1], &self.theme, self.active_tab);

        // Draw active tab content
        use super::tab::TabView;
        match self.active_tab {
            TabId::Upload => self.upload_tab.draw(frame, chunks[3], &self.theme),
            TabId::Tasks => self.tasks_tab.draw(frame, chunks[3], &self.theme),
            TabId::Log => self.log_tab.draw(frame, chunks[3], &self.theme),
            TabId::Errors => self.errors_tab.draw(frame, chunks[3], &self.theme),
        }

        // Draw footer with context-sensitive hints
        let hints = match self.active_tab {
            TabId::Upload => self.upload_tab.key_hints(),
            TabId::Tasks => self.tasks_tab.key_hints(),
            TabId::Log => self.log_tab.key_hints(),
            TabId::Errors => self.errors_tab.key_hints(),
        };
        let elapsed =
            widgets::format_elapsed(self.upload_state.lock().unwrap().throughput.elapsed());
        widgets::draw_footer(frame, chunks[4], &self.theme, &hints, &elapsed);

        // Help overlay on top
        if self.show_help {
            help::draw_help_overlay(frame, area, &self.theme, self.active_tab);
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Help overlay intercepts all keys
        if self.show_help {
            self.show_help = false;
            return true;
        }

        // Ctrl-C always quits
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return true;
        }

        // When a tab is in search/input mode, route ALL keys to the tab first
        if self.is_tab_consuming_input() {
            use super::tab::TabView;
            return match self.active_tab {
                TabId::Upload => self.upload_tab.handle_key(code, modifiers),
                TabId::Tasks => self.tasks_tab.handle_key(code, modifiers),
                TabId::Log => self.log_tab.handle_key(code, modifiers),
                TabId::Errors => self.errors_tab.handle_key(code, modifiers),
            };
        }

        // Global keys (only when no tab is consuming input)
        match code {
            KeyCode::Char('q') => {
                self.quit = true;
                return true;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                return true;
            }
            KeyCode::Char('1') => {
                self.active_tab = TabId::Upload;
                return true;
            }
            KeyCode::Char('2') => {
                self.active_tab = TabId::Tasks;
                return true;
            }
            KeyCode::Char('3') => {
                self.active_tab = TabId::Log;
                return true;
            }
            KeyCode::Char('4') => {
                self.active_tab = TabId::Errors;
                return true;
            }
            _ => {}
        }

        // Route to active tab
        use super::tab::TabView;
        match self.active_tab {
            TabId::Upload => self.upload_tab.handle_key(code, modifiers),
            TabId::Tasks => self.tasks_tab.handle_key(code, modifiers),
            TabId::Log => self.log_tab.handle_key(code, modifiers),
            TabId::Errors => self.errors_tab.handle_key(code, modifiers),
        }
    }

    fn is_done(&self) -> bool {
        let state = self.upload_state.lock().unwrap();
        state.done && state.active_files.is_empty()
    }

    fn quit_requested(&self) -> bool {
        self.quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn make_dashboard() -> MultiTabDashboard {
        let upload_state = Arc::new(Mutex::new(UploadTuiState::new(&["test".to_string()])));
        let s3_state = Arc::new(Mutex::new(S3TaskState::new("t@t.com".into())));
        let joblog_state = Arc::new(Mutex::new(JoblogState::empty()));
        MultiTabDashboard::new(upload_state, s3_state, joblog_state)
    }

    #[test]
    fn test_initial_tab() {
        let d = make_dashboard();
        assert_eq!(d.active_tab, TabId::Upload);
    }

    #[test]
    fn test_tab_switching() {
        let mut d = make_dashboard();
        d.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Tasks);
        d.handle_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Log);
        d.handle_key(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Errors);
        d.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Upload);
    }

    #[test]
    fn test_quit() {
        let mut d = make_dashboard();
        assert!(!d.quit_requested());
        d.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(d.quit_requested());
    }

    #[test]
    fn test_ctrl_c_quit() {
        let mut d = make_dashboard();
        assert!(!d.quit_requested());
        d.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(d.quit_requested());
    }

    #[test]
    fn test_help_toggle() {
        let mut d = make_dashboard();
        assert!(!d.show_help);
        d.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(d.show_help);
        // Any key dismisses
        d.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!d.show_help);
    }

    #[test]
    fn test_keys_route_to_active_tab() {
        let mut d = make_dashboard();
        // j on Upload tab should be consumed by upload tab
        let consumed = d.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(consumed);
    }

    #[test]
    fn test_is_done_initially_false() {
        let d = make_dashboard();
        assert!(!d.is_done());
    }

    #[test]
    fn test_help_blocks_tab_switch() {
        let mut d = make_dashboard();
        d.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(d.show_help);
        // '2' should dismiss help, not switch tabs
        d.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert!(!d.show_help);
        assert_eq!(d.active_tab, TabId::Upload);
    }

    #[test]
    fn test_search_mode_blocks_global_keys() {
        let mut d = make_dashboard();
        // Switch to Tasks tab and activate search
        d.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        d.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

        // '1' should go to search, not switch tabs
        d.handle_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(d.active_tab, TabId::Tasks);

        // 'q' should go to search, not quit
        d.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!d.quit_requested());

        // Escape exits search, then q quits
        d.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        d.handle_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(d.quit_requested());
    }

    #[test]
    fn test_tick_does_not_panic() {
        let mut d = make_dashboard();
        // Ensure tick_all doesn't crash with empty state
        d.tick_all();
    }
}
