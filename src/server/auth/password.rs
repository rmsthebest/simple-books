//! Password hashing. Argon2 is deliberately slow and memory-hungry, so it runs on
//! the blocking pool and at most a couple at a time: a burst of logins queues
//! instead of exhausting memory.

use std::sync::LazyLock;

use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use rand::RngCore;
use tokio::sync::Semaphore;

/// OWASP's baseline for Argon2id: 19 MiB, 2 passes, 1 lane.
const MEMORY_KIB: u32 = 19_456;
const PASSES: u32 = 2;
const LANES: u32 = 1;

const MAX_CONCURRENT: usize = 2;
pub const MIN_PASSWORD_CHARS: usize = 10;
/// Well above any real password, and it bounds the work one request can cause.
pub const MAX_PASSWORD_BYTES: usize = 1024;

static PERMITS: Semaphore = Semaphore::const_new(MAX_CONCURRENT);

/// Verified against when there's no real hash, so that an unknown username takes
/// as long to reject as a wrong password.
static DUMMY_HASH: LazyLock<Option<String>> =
    LazyLock::new(|| hash_blocking("no account has this password").ok());

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password hashing failed: {0}")]
    Hash(String),
    #[error("password task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("password limiter closed")]
    Closed(#[from] tokio::sync::AcquireError),
}

fn hasher() -> Result<Argon2<'static>, PasswordError> {
    let params = Params::new(MEMORY_KIB, PASSES, LANES, None)
        .map_err(|error| PasswordError::Hash(error.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn hash_blocking(password: &str) -> Result<String, PasswordError> {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    hasher()?
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| PasswordError::Hash(error.to_string()))
}

fn verify_blocking(stored: Option<&str>, password: &str) -> Result<bool, PasswordError> {
    let (hash, real) = match stored {
        Some(hash) => (hash.to_owned(), true),
        None => match DUMMY_HASH.as_ref() {
            Some(hash) => (hash.clone(), false),
            None => return Err(PasswordError::Hash("no dummy hash".to_owned())),
        },
    };
    let Ok(parsed) = PasswordHash::new(&hash) else {
        return Ok(false);
    };
    let matched = hasher()?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok();
    Ok(matched && real)
}

/// Hashes a new password.
pub async fn hash(password: String) -> Result<String, PasswordError> {
    let _permit = PERMITS.acquire().await?;
    tokio::task::spawn_blocking(move || hash_blocking(&password)).await?
}

/// Checks a password. With no stored hash it does the same work and returns false.
pub async fn verify(stored: Option<String>, password: String) -> Result<bool, PasswordError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Ok(false);
    }
    let _permit = PERMITS.acquire().await?;
    tokio::task::spawn_blocking(move || verify_blocking(stored.as_deref(), &password)).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_uses_argon2id_with_our_parameters() {
        let stored = hash("correct horse".to_owned()).await.unwrap();
        assert!(
            stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{stored}"
        );
        assert!(
            verify(Some(stored.clone()), "correct horse".to_owned())
                .await
                .unwrap()
        );
        assert!(
            !verify(Some(stored), "wrong horse".to_owned())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn salts_differ() {
        let a = hash("same".to_owned()).await.unwrap();
        let b = hash("same".to_owned()).await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn no_stored_hash_never_matches_even_the_dummy_password() {
        for attempt in ["anything", "no account has this password"] {
            assert!(!verify(None, attempt.to_owned()).await.unwrap());
        }
    }

    #[tokio::test]
    async fn oversized_password_is_refused_without_hashing() {
        let stored = hash("x".repeat(MAX_PASSWORD_BYTES + 1)).await.unwrap();
        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(!verify(Some(stored), long).await.unwrap());
    }

    #[tokio::test]
    async fn a_corrupt_stored_hash_fails_closed() {
        assert!(
            !verify(Some("not a hash".to_owned()), "x".to_owned())
                .await
                .unwrap()
        );
    }
}
