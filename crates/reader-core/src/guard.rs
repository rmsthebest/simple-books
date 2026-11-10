//! Ignores page turns that come too soon after the last one.
//!
//! The Phase 0 capture on the Clara BW found no bounce in the touch panel:
//! every tap was one down and one up. So this doesn't filter touches. It
//! refuses a second turn that arrives within a short window of the first, or
//! while the panel is still drawing the page that turn asked for. Nobody
//! means to turn past a page they haven't seen yet.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TurnGuard {
    /// The shortest time between two turns that are both kept.
    pub window: Duration,
    last: Option<Instant>,
    refreshing_until: Option<Instant>,
}

impl Default for TurnGuard {
    /// A 300 ms window: longer than a double tap's bounce would be, shorter
    /// than anyone reads a page. The Clara BW takes about 280 ms to draw one.
    fn default() -> Self {
        Self::new(Duration::from_millis(300))
    }
}

impl TurnGuard {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last: None,
            refreshing_until: None,
        }
    }

    /// Whether a turn asked for at `now` should happen. A kept turn starts a
    /// new window.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.window)
            || self.refreshing_until.is_some_and(|until| now < until)
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    /// Tells the guard the panel is busy until `until`, such as from the
    /// moment a refresh is sent until the driver says it's done.
    pub fn refreshing(&mut self, until: Instant) {
        self.refreshing_until = Some(until);
    }

    /// The panel finished early.
    pub fn refreshed(&mut self) {
        self.refreshing_until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_second_turn_inside_the_window_is_ignored() {
        let start = Instant::now();
        let mut guard = TurnGuard::default();
        assert!(guard.allow(start));
        assert!(!guard.allow(start + 120 * MS));
        // The ignored turn didn't restart the window.
        assert!(guard.allow(start + 300 * MS));
        assert!(!guard.allow(start + 599 * MS));
        assert!(guard.allow(start + 600 * MS));
    }

    #[test]
    fn turns_wait_for_the_panel() {
        let start = Instant::now();
        let mut guard = TurnGuard::new(100 * MS);
        assert!(guard.allow(start));
        guard.refreshing(start + 450 * MS);
        assert!(!guard.allow(start + 200 * MS));
        assert!(guard.allow(start + 450 * MS));
        guard.refreshing(start + 2000 * MS);
        guard.refreshed();
        assert!(guard.allow(start + 600 * MS));
    }
}
