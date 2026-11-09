//! Accounts: there's no signup. An admin creates a user, and the user sets their
//! password through a one-time link. The same link resets a forgotten one.

use super::{
    device::{generate_secret, hash_secret},
    password::{self, MAX_PASSWORD_BYTES, MIN_PASSWORD_CHARS, PasswordError},
    session,
};
use crate::server::{db::Db, now};

const SETUP_LINK_SECONDS: i64 = 72 * 60 * 60;
const MAX_USERNAME_CHARS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("usernames are 1 to {MAX_USERNAME_CHARS} characters, with no spaces at the ends")]
    InvalidUsername,
    #[error("that username is taken")]
    UsernameTaken,
    #[error("no such user")]
    NoSuchUser,
    #[error("passwords are at least {MIN_PASSWORD_CHARS} characters")]
    PasswordTooShort,
    #[error("that password is too long")]
    PasswordTooLong,
    #[error("this link has expired or was already used")]
    InvalidSetupLink,
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

fn check_username(username: &str) -> Result<(), AccountError> {
    let chars = username.chars().count();
    let tidy = username == username.trim()
        && !username.chars().any(char::is_control)
        && !username.contains('/');
    if (1..=MAX_USERNAME_CHARS).contains(&chars) && tidy {
        Ok(())
    } else {
        Err(AccountError::InvalidUsername)
    }
}

fn check_new_password(password: &str) -> Result<(), AccountError> {
    if password.len() > MAX_PASSWORD_BYTES {
        Err(AccountError::PasswordTooLong)
    } else if password.chars().count() < MIN_PASSWORD_CHARS {
        Err(AccountError::PasswordTooShort)
    } else {
        Ok(())
    }
}

pub async fn create_user(
    db: &Db,
    username: &str,
    quota_bytes: i64,
    is_admin: bool,
) -> Result<i64, AccountError> {
    check_username(username)?;
    db.create_user(username, quota_bytes, is_admin)
        .await
        .map_err(|error| match &error {
            sqlx::Error::Database(e) if e.is_unique_violation() => AccountError::UsernameTaken,
            _ => AccountError::Db(error),
        })
}

/// Creates a link for a user to set their password, replacing any earlier one.
pub async fn setup_link(db: &Db, public_url: &str, user_id: i64) -> Result<String, AccountError> {
    let token = generate_secret();
    db.replace_setup_token(user_id, &hash_secret(&token), now() + SETUP_LINK_SECONDS)
        .await?;
    Ok(format!("{public_url}/setup/{token}"))
}

/// The same, by username, for the command line.
pub async fn setup_link_for(
    db: &Db,
    public_url: &str,
    username: &str,
) -> Result<String, AccountError> {
    let user_id = db
        .user_id_by_name(username)
        .await?
        .ok_or(AccountError::NoSuchUser)?;
    setup_link(db, public_url, user_id).await
}

/// Who a setup link is for, to show on its page. `None` if it's no good.
pub async fn setup_link_user(db: &Db, token: &str) -> Result<Option<String>, sqlx::Error> {
    db.setup_token_user(&hash_secret(token)).await
}

/// Sets the password from a setup link and signs in. Returns a session token.
/// The link is checked first, so nobody can make the server hash passwords
/// without holding one.
pub async fn complete_setup(
    db: &Db,
    token: &str,
    password: String,
) -> Result<String, AccountError> {
    check_new_password(&password)?;
    let hash = hash_secret(token);
    if db.setup_token_user(&hash).await?.is_none() {
        return Err(AccountError::InvalidSetupLink);
    }
    let password_hash = password::hash(password).await?;
    let user_id = db
        .redeem_setup_token(&hash, &password_hash)
        .await?
        .ok_or(AccountError::InvalidSetupLink)?;
    Ok(session::start(db, user_id).await?)
}

/// Checks a login and returns a session token. Wrong password, unknown user,
/// disabled user and unfinished setup all look the same and take the same time.
pub async fn log_in(
    db: &Db,
    username: &str,
    password: String,
) -> Result<Option<String>, AccountError> {
    let candidate = db.login_candidate(username).await?;
    let (user_id, stored) = match candidate {
        Some(candidate) => (Some(candidate.id), candidate.password_hash),
        None => (None, None),
    };
    if !password::verify(stored, password).await? {
        return Ok(None);
    }
    match user_id {
        Some(id) => Ok(Some(session::start(db, id).await?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        for good in ["ann", "Ann Lee", "åsa", "a.b-c_d", &"x".repeat(64)] {
            assert!(check_username(good).is_ok(), "{good}");
        }
        for bad in ["", " ann", "ann ", "a\nb", "a/b", &"x".repeat(65)] {
            assert!(check_username(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn new_passwords() {
        assert!(check_new_password("0123456789").is_ok());
        assert!(
            check_new_password("åååååååååå").is_ok(),
            "counts characters"
        );
        assert!(matches!(
            check_new_password("012345678"),
            Err(AccountError::PasswordTooShort)
        ));
        assert!(matches!(
            check_new_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)),
            Err(AccountError::PasswordTooLong)
        ));
    }
}
