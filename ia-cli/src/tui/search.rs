//! Vim-style search input and filter cycling for TUI tabs.

/// Vim-style search input state.
#[derive(Debug)]
pub struct SearchState {
    query: String,
    /// Cached lowercased version of `query` to avoid per-call allocation.
    query_lower: String,
    active: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            query_lower: String::new(),
            active: false,
        }
    }

    #[cfg(test)]
    pub fn with_query(q: &str) -> Self {
        Self {
            query: q.to_string(),
            query_lower: q.to_lowercase(),
            active: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.query_lower.clear();
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.query_lower = self.query.to_lowercase();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.query_lower = self.query.to_lowercase();
    }

    pub fn confirm(&mut self) {
        self.active = false;
        // query stays as active filter
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
        self.query_lower.clear();
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.query.clear();
        self.query_lower.clear();
    }

    pub fn matches(&self, text: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        text.to_lowercase().contains(&self.query_lower)
    }
}

/// Cycles through a fixed set of filter labels.
#[derive(Debug)]
pub struct FilterCycle {
    labels: Vec<&'static str>,
    index: usize,
}

impl FilterCycle {
    pub fn new(labels: &[&'static str]) -> Self {
        debug_assert!(
            !labels.is_empty(),
            "FilterCycle requires at least one label"
        );
        Self {
            labels: labels.to_vec(),
            index: 0,
        }
    }

    pub fn current(&self) -> &str {
        self.labels[self.index]
    }

    pub fn next(&mut self) {
        self.index = (self.index + 1) % self.labels.len();
    }

    #[cfg(test)]
    pub fn index(&self) -> usize {
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_inactive_by_default() {
        let search = SearchState::new();
        assert!(!search.is_active());
        assert!(search.query().is_empty());
    }

    #[test]
    fn test_activate_and_type() {
        let mut search = SearchState::new();
        search.activate();
        assert!(search.is_active());
        search.push('h');
        search.push('e');
        search.push('l');
        assert_eq!(search.query(), "hel");
    }

    #[test]
    fn test_backspace() {
        let mut search = SearchState::new();
        search.activate();
        search.push('a');
        search.push('b');
        search.backspace();
        assert_eq!(search.query(), "a");
    }

    #[test]
    fn test_confirm() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.confirm();
        assert!(!search.is_active());
        assert_eq!(search.query(), "x"); // filter stays active
    }

    #[test]
    fn test_cancel_clears() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.cancel();
        assert!(!search.is_active());
        assert!(search.query().is_empty()); // filter cleared
    }

    #[test]
    fn test_clear_filter() {
        let mut search = SearchState::new();
        search.activate();
        search.push('x');
        search.confirm();
        assert_eq!(search.query(), "x");
        search.clear();
        assert!(search.query().is_empty());
    }

    #[test]
    fn test_matches() {
        let search = SearchState::with_query("nasa");
        assert!(search.matches("nasa-photos-2024"));
        assert!(search.matches("NASA-data")); // case-insensitive
        assert!(!search.matches("hubble-deep"));
    }

    #[test]
    fn test_filter_cycle() {
        let mut filter = FilterCycle::new(&["All", "Uploaded", "Skipped", "Failed"]);
        assert_eq!(filter.current(), "All");
        filter.next();
        assert_eq!(filter.current(), "Uploaded");
        filter.next();
        assert_eq!(filter.current(), "Skipped");
        filter.next();
        assert_eq!(filter.current(), "Failed");
        filter.next();
        assert_eq!(filter.current(), "All"); // wraps around
    }
}
