//! Hardcover.app: each user can connect their account, and then what they read on
//! their Kobo shows up there. Everything goes through a queue in the database, sent
//! by one background task, so a Kobo's sync never waits on Hardcover, and an update
//! that fails is tried again later.

pub mod api;
pub mod rules;
pub mod worker;

use std::time::Duration;

use tokio::sync::Mutex;

pub use api::Api;

/// One request a second at most, well under Hardcover's 60 a minute.
const PACE: Duration = Duration::from_secs(1);

/// The connection to Hardcover, when it's turned on.
#[derive(Debug)]
pub struct Hardcover {
    pub api: Api,
    pace: Duration,
    last_request: Mutex<Option<tokio::time::Instant>>,
}

impl Hardcover {
    pub fn new(base: &str, client_id: &str) -> Result<Self, api::ApiError> {
        Ok(Self {
            api: Api::new(base, client_id)?,
            pace: PACE,
            last_request: Mutex::new(None),
        })
    }

    /// For tests, against a fake Hardcover.
    #[must_use]
    pub fn with_pace(mut self, pace: Duration) -> Self {
        self.pace = pace;
        self
    }

    /// Waits until the next request is allowed.
    pub async fn pace(&self) {
        let mut last = self.last_request.lock().await;
        if let Some(at) = *last {
            tokio::time::sleep_until(at + self.pace).await;
        }
        *last = Some(tokio::time::Instant::now());
    }
}
