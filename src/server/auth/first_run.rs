//! The link that makes the first admin. A server with no users prints it when it
//! starts, and whoever opens it chooses the admin's username and password. It lives
//! only in memory: a restart replaces it, and it stops working once anyone exists.

use std::sync::{Mutex, PoisonError};

use super::device::{generate_secret, hash_secret};

/// Long enough to set up a server at leisure. A restart prints a new one.
pub const FIRST_RUN_SECONDS: i64 = 72 * 60 * 60;

#[derive(Debug, Default)]
pub struct FirstRun {
    /// The link's hash and when it expires.
    slot: Mutex<Option<(String, i64)>>,
}

impl FirstRun {
    /// A new link's secret, replacing any earlier one.
    pub fn issue(&self, at: i64) -> String {
        let token = generate_secret();
        *self.lock() = Some((hash_secret(&token), at + FIRST_RUN_SECONDS));
        token
    }

    pub fn is_valid(&self, token: &str, at: i64) -> bool {
        let hash = hash_secret(token);
        self.lock()
            .as_ref()
            .is_some_and(|(held, expires)| *held == hash && at < *expires)
    }

    /// Uses the link up, if it's still good. Only one caller can ever succeed.
    pub fn take(&self, token: &str, at: i64) -> bool {
        let mut slot = self.lock();
        let hash = hash_secret(token);
        let good = slot
            .as_ref()
            .is_some_and(|(held, expires)| *held == hash && at < *expires);
        if good {
            *slot = None;
        }
        good
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(String, i64)>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works_once_until_it_expires_or_is_replaced() {
        let first_run = FirstRun::default();
        assert!(!first_run.is_valid("anything", 0));

        let old = first_run.issue(100);
        let token = first_run.issue(100);
        assert!(!first_run.is_valid(&old, 100), "replaced");
        assert!(first_run.is_valid(&token, 100));
        assert!(
            !first_run.is_valid(&token, 100 + FIRST_RUN_SECONDS),
            "expired"
        );
        assert!(!first_run.take("wrong", 100));

        assert!(first_run.take(&token, 101));
        assert!(!first_run.take(&token, 101), "used up");
        assert!(!first_run.is_valid(&token, 101));
    }
}
