//! Fixed-window rate limits, held in memory. A restart forgets them, which costs an
//! attacker's slowdown only.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Keys can be made up by whoever's asking, so this has to stay bounded.
const MAX_KEYS: usize = 65_536;

#[derive(Debug)]
pub struct RateLimit {
    limit: u32,
    window: Duration,
    counts: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimit {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            counts: Mutex::default(),
        }
    }

    fn with_entry<T>(
        &self,
        key: &str,
        at: Instant,
        change: impl FnOnce(&mut (u32, Instant)) -> T,
    ) -> T {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if counts.len() >= MAX_KEYS && !counts.contains_key(key) {
            let window = self.window;
            counts.retain(|_, (_, started)| at.duration_since(*started) < window);
            if counts.len() >= MAX_KEYS {
                // The Argon2 semaphore still limits the damage while this refills.
                tracing::warn!("a rate limit filled up and was reset");
                counts.clear();
            }
        }
        let entry = counts.entry(key.to_owned()).or_insert((0, at));
        if at.duration_since(entry.1) >= self.window {
            *entry = (0, at);
        }
        change(entry)
    }

    fn acquire_at(&self, key: &str, at: Instant) -> bool {
        let limit = self.limit;
        self.with_entry(key, at, |entry| {
            if entry.0 >= limit {
                false
            } else {
                entry.0 += 1;
                true
            }
        })
    }

    /// Counts an attempt, unless the limit has been reached.
    pub fn acquire(&self, key: &str) -> bool {
        self.acquire_at(key, Instant::now())
    }

    /// Takes back an attempt that turned out fine.
    pub fn refund(&self, key: &str) {
        self.with_entry(key, Instant::now(), |entry| {
            entry.0 = entry.0.saturating_sub(1);
        });
    }

    /// Counts a failure.
    pub fn hit(&self, key: &str) {
        self.acquire(key);
    }

    pub fn exceeded(&self, key: &str) -> bool {
        let limit = self.limit;
        self.with_entry(key, Instant::now(), |entry| entry.0 >= limit)
    }
}

/// The limits in use.
#[derive(Debug)]
pub struct Limits {
    /// Failed logins from one address.
    pub login_ip: RateLimit,
    /// Failed logins for one username from everywhere. Well above the limit per
    /// address, so that one address alone can't lock someone out.
    pub login_user: RateLimit,
    /// Unknown device tokens from one address.
    pub device_failures: RateLimit,
    /// Codes asked for by simple-books readers from one address.
    pub pairings: RateLimit,
    /// Wrong reader codes typed by one user.
    pub pair_codes: RateLimit,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login_ip: RateLimit::new(10, Duration::from_mins(15)),
            login_user: RateLimit::new(100, Duration::from_hours(1)),
            device_failures: RateLimit::new(60, Duration::from_mins(1)),
            pairings: RateLimit::new(10, Duration::from_hours(1)),
            pair_codes: RateLimit::new(10, Duration::from_mins(15)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_the_limit_per_key() {
        let limit = RateLimit::new(2, Duration::from_mins(1));
        assert!(limit.acquire("a"));
        assert!(limit.acquire("a"));
        assert!(!limit.acquire("a"));
        assert!(limit.exceeded("a"));
        assert!(limit.acquire("b"));
        assert!(!limit.exceeded("b"));
    }

    #[test]
    fn a_new_window_starts_over() {
        let limit = RateLimit::new(1, Duration::from_mins(1));
        let start = Instant::now();
        assert!(limit.acquire_at("a", start));
        assert!(!limit.acquire_at("a", start + Duration::from_secs(59)));
        assert!(limit.acquire_at("a", start + Duration::from_secs(60)));
    }

    #[test]
    fn a_refund_gives_the_attempt_back() {
        let limit = RateLimit::new(1, Duration::from_mins(1));
        assert!(limit.acquire("a"));
        limit.refund("a");
        assert!(limit.acquire("a"));
    }

    #[test]
    fn stays_bounded_when_flooded_with_keys() {
        let limit = RateLimit::new(1, Duration::from_hours(1));
        for key in 0..=MAX_KEYS {
            limit.hit(&key.to_string());
        }
        assert!(limit.counts.lock().unwrap().len() <= MAX_KEYS);
    }
}
