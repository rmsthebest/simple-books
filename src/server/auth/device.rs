//! Per-device sync secrets. They are high-entropy and random, so a plain
//! SHA-256 is the right hash: there's nothing to brute force, and they're
//! checked on every request. Lookup is by hash, so no secret is ever compared.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::server::db::Db;

/// Generates a 256-bit URL-safe secret.
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Where a Kobo syncs, secret included. It can only be shown when it's first made,
/// because the secret isn't stored.
fn endpoint(public_url: &str, secret: &str) -> String {
    format!("{public_url}/kobo/{secret}")
}

/// Adds a Kobo for this user and returns the address to put in its config.
pub async fn connect(db: &Db, public_url: &str, user_id: i64) -> Result<String, sqlx::Error> {
    let secret = generate_secret();
    db.create_device(user_id, "Kobo", &hash_secret(&secret))
        .await?;
    Ok(endpoint(public_url, &secret))
}

/// A new address for a Kobo already connected. The old one works until this one is
/// first used. `None` if it isn't this user's device.
pub async fn reconnect(
    db: &Db,
    public_url: &str,
    user_id: i64,
    device_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let secret = generate_secret();
    let started = db
        .begin_reconnect(user_id, device_id, &hash_secret(&secret))
        .await?;
    Ok(started.map(|_| endpoint(public_url, &secret)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_long_unique_and_url_safe() {
        let a = generate_secret();
        assert_eq!(a.len(), 43);
        assert_ne!(a, generate_secret());
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn hash_is_stable_and_not_the_secret() {
        assert_eq!(hash_secret("x"), hash_secret("x"));
        assert_ne!(hash_secret("x"), "x");
        assert_ne!(hash_secret("x"), hash_secret("y"));
    }
}
