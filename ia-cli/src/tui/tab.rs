//! Tab framework for multi-tab dashboards.
//!
//! Defines the [`TabView`] trait that individual tabs implement, and the
//! [`TabId`] enum identifying which tab is active.

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::theme::Theme;

/// Trait for individual tab views within a multi-tab dashboard.
pub trait TabView {
    /// Render the tab's content into the given area.
    fn draw(&self, frame: &mut Frame, area: Rect, theme: &Theme);

    /// Handle a key event. Returns true if the key was consumed.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool;

    /// Called on each tick for periodic updates (polling, tail reads, etc.).
    fn tick(&mut self);

    /// Return key hints for the footer, specific to this tab.
    /// Each tuple is (key_label, description).
    fn key_hints(&self) -> Vec<(&str, &str)>;
}

/// Identifies which tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Upload,
    Tasks,
    Log,
    Errors,
}

impl TabId {
    pub const ALL: [TabId; 4] = [TabId::Upload, TabId::Tasks, TabId::Log, TabId::Errors];

    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(TabId::Upload),
            1 => Some(TabId::Tasks),
            2 => Some(TabId::Log),
            3 => Some(TabId::Errors),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            TabId::Upload => 0,
            TabId::Tasks => 1,
            TabId::Log => 2,
            TabId::Errors => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TabId::Upload => "❶ Upload",
            TabId::Tasks => "❷ Tasks",
            TabId::Log => "❸ Log",
            TabId::Errors => "❹ Errors",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_id_from_index() {
        assert_eq!(TabId::from_index(0), Some(TabId::Upload));
        assert_eq!(TabId::from_index(1), Some(TabId::Tasks));
        assert_eq!(TabId::from_index(2), Some(TabId::Log));
        assert_eq!(TabId::from_index(3), Some(TabId::Errors));
        assert_eq!(TabId::from_index(4), None);
    }

    #[test]
    fn test_tab_id_label() {
        assert_eq!(TabId::Upload.label(), "❶ Upload");
        assert_eq!(TabId::Tasks.label(), "❷ Tasks");
        assert_eq!(TabId::Log.label(), "❸ Log");
        assert_eq!(TabId::Errors.label(), "❹ Errors");
    }

    #[test]
    fn test_tab_id_index() {
        assert_eq!(TabId::Upload.index(), 0);
        assert_eq!(TabId::Errors.index(), 3);
    }
}
