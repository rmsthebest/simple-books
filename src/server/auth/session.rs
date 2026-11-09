//! Web sessions. The token is random, lives only in an `HttpOnly` cookie, and is
//! stored as a hash. Only [`CurrentUser`] turns a request into a [`UserScope`].

use axum::{
    extract::FromRequestParts,
    http::{
        HeaderMap, HeaderValue,
        header::{COOKIE, SET_COOKIE},
        request::Parts,
    },
    response::{IntoResponse, Response},
};

use super::device::{generate_secret, hash_secret};
use crate::server::{
    AppState,
    db::{Db, scope::UserScope},
    error::AppError,
    now,
};

pub const COOKIE_NAME: &str = "__Host-sb";

/// A session ends after this long unused. Staying signed in is the point of a
/// page people drop a book on every few weeks.
const IDLE_SECONDS: i64 = 90 * 24 * 60 * 60;
/// The cookie outlives the idle limit, which is the limit that counts: the server
/// extends it on use, without having to send the cookie again.
const COOKIE_MAX_AGE_SECONDS: i64 = 365 * 24 * 60 * 60;

/// Starts a session and returns its token, for [`set_cookie`]. Each login gets a
/// new token, so one planted before login is worthless.
pub async fn start(db: &Db, user_id: i64) -> Result<String, sqlx::Error> {
    let token = generate_secret();
    db.create_session(&hash_secret(&token), user_id, now() + IDLE_SECONDS)
        .await?;
    Ok(token)
}

pub async fn end(db: &Db, headers: &HeaderMap) -> Result<(), sqlx::Error> {
    if let Some(token) = token_from_headers(headers) {
        db.delete_session(&hash_secret(token)).await?;
    }
    Ok(())
}

fn cookie_value(value: &str, max_age: i64) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax"
    ))
    .ok()
}

/// Adds the session cookie to a response.
pub fn set_cookie(response: &mut Response, token: &str) {
    if let Some(value) = cookie_value(token, COOKIE_MAX_AGE_SECONDS) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

/// Removes the session cookie from the browser.
pub fn clear_cookie(response: &mut Response) {
    if let Some(value) = cookie_value("", 0) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

/// The session token, if the request has exactly the cookie we set.
pub fn token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// A signed-in user. Handlers take this to require a session; there's no other
/// way for a web handler to get a [`UserScope`].
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub scope: UserScope,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || AppError::Unauthorized.into_response();
        let token = token_from_headers(&parts.headers).ok_or_else(unauthorized)?;
        let hash = hash_secret(token);
        let user = state
            .db
            .session_user(&hash)
            .await
            .map_err(|error| AppError::Db(error).into_response())?
            .ok_or_else(unauthorized)?;

        if let Err(error) = state.db.touch_session(&hash, now() + IDLE_SECONDS).await {
            tracing::warn!(%error, "couldn't extend a session");
        }

        Ok(Self {
            id: user.id,
            username: user.username,
            is_admin: user.is_admin,
            scope: state.db.scope(user.id),
        })
    }
}

/// A signed-in admin. Anyone else gets 403.
#[derive(Debug, Clone)]
pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        if user.is_admin {
            Ok(Self(user))
        } else {
            Err(AppError::Forbidden.into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(COOKIE, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn finds_our_cookie_among_others() {
        let map = headers(&["a=1; __Host-sb=tok; b=2"]);
        assert_eq!(token_from_headers(&map), Some("tok"));
        let map = headers(&["a=1", "__Host-sb=tok"]);
        assert_eq!(token_from_headers(&map), Some("tok"));
    }

    #[test]
    fn ignores_lookalikes_and_empty_values() {
        for value in [
            "sb=tok",
            "x__Host-sb=tok",
            "__Host-sbx=tok",
            "__Host-sb=",
            "",
        ] {
            assert_eq!(token_from_headers(&headers(&[value])), None, "{value}");
        }
    }

    #[test]
    fn cookie_has_every_protection() {
        let value = cookie_value("tok", 60).unwrap();
        let value = value.to_str().unwrap();
        for part in [
            "__Host-sb=tok",
            "Path=/",
            "HttpOnly",
            "Secure",
            "SameSite=Lax",
        ] {
            assert!(value.contains(part), "{value} lacks {part}");
        }
        assert!(!value.contains("Domain"), "__Host- forbids a domain");
    }
}
