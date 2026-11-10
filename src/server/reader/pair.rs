//! `POST /device/v1/pair` and `GET /device/v1/pair`.
//!
//! A reader asks for a code and gets it with a new secret. The person types
//! the code on the website's device page while signed in ([`enter`]), which
//! makes the secret a device of theirs. Meanwhile the reader asks, with the
//! secret, whether that's happened.
//!
//! The code is 8 letters from 32, about 10¹² of them, and lasts 15 minutes.
//! Guessing one needs a signed-in account and runs into a limit per user.
//! Asking for codes needs nothing, so it's limited per address and capped in
//! total.

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use rand::Rng;
use sync_proto::{MAX_NAME, PairRequest, PairStart, PairStatus};

use super::{bearer, internal_error, problem};
use crate::server::{
    AppState,
    auth::device::{generate_secret, hash_secret},
    db::pairing::{PairError, PairState},
    http::client_ip,
    now,
};

/// No 0, O, 1 or I, which look alike on a small screen.
const LETTERS: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 8;
const LIFETIME: i64 = 15 * 60;
const INTERVAL: u32 = 5;

fn new_code() -> String {
    let mut rng = rand::rng();
    (0..CODE_LENGTH)
        .map(|_| char::from(LETTERS[rng.random_range(0..LETTERS.len())]))
        .collect()
}

/// A code as shown: `ABCD-EFGH`.
fn shown(code: &str) -> String {
    let (a, b) = code.split_at(CODE_LENGTH / 2);
    format!("{a}-{b}")
}

/// A code as typed, in any case, with or without its dash or spaces. `None`
/// if it can't be one.
pub fn normalize(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LENGTH && code.bytes().all(|b| LETTERS.contains(&b))).then_some(code)
}

/// The device's name as the website shows it.
fn clean_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "Reader".to_owned()
    } else {
        name.to_owned()
    }
}

pub async fn start(
    State(state): State<AppState>,
    parts: Parts,
    body: Result<Json<PairRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return problem(StatusCode::BAD_REQUEST, "that wasn't a pairing request");
    };
    let ip = client_ip::limit_key(client_ip::from_request(
        &state.config,
        &parts.headers,
        &parts.extensions,
    ));
    if !state.limits.pairings.acquire(&ip) {
        return problem(
            StatusCode::TOO_MANY_REQUESTS,
            "too many codes asked for; try again later",
        );
    }

    let name = clean_name(&request.name);
    let secret = generate_secret();
    let at = now();
    // A clash with a waiting code is unlikely, but possible.
    for _ in 0..3 {
        let code = new_code();
        match state
            .db
            .start_pairing(&hash_secret(&secret), &code, &name, at, at + LIFETIME)
            .await
        {
            Ok(true) => {
                return Json(PairStart {
                    secret,
                    code: shown(&code),
                    url: format!("{}/device", state.config.public_url),
                    expires_in: LIFETIME as u32,
                    interval: INTERVAL,
                })
                .into_response();
            }
            Ok(false) => {}
            Err(error) => return internal_error(&error),
        }
    }
    problem(
        StatusCode::SERVICE_UNAVAILABLE,
        "too many readers are pairing; try again in a few minutes",
    )
}

pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(secret) = bearer(&headers) else {
        return problem(StatusCode::UNAUTHORIZED, "no secret given");
    };
    match state.db.pair_state(&hash_secret(secret), now()).await {
        Ok(PairState::Pending) => Json(PairStatus::Pending).into_response(),
        Ok(PairState::Paired { username }) => {
            Json(PairStatus::Paired { user: username }).into_response()
        }
        Ok(PairState::Unknown) => Json(PairStatus::Expired).into_response(),
        Err(error) => internal_error(&error),
    }
}

/// What came of typing a code on the website.
#[derive(Debug)]
pub enum Entered {
    /// The reader with this name is now the user's.
    Paired(String),
    /// Not a code that's waiting.
    Wrong,
    /// Too many wrong codes lately.
    Slow,
}

/// Pairs the reader waiting with `typed` to this user. Wrong codes count
/// towards a limit per user, so they can't be guessed from one account.
pub async fn enter(state: &AppState, user_id: i64, typed: &str) -> Result<Entered, sqlx::Error> {
    let key = user_id.to_string();
    if state.limits.pair_codes.exceeded(&key) {
        return Ok(Entered::Slow);
    }
    let Some(code) = normalize(typed) else {
        state.limits.pair_codes.hit(&key);
        return Ok(Entered::Wrong);
    };
    match state.db.finish_pairing(user_id, &code, now()).await {
        Ok(name) => Ok(Entered::Paired(name)),
        Err(PairError::NoSuchCode) => {
            state.limits.pair_codes.hit(&key);
            Ok(Entered::Wrong)
        }
        Err(PairError::Db(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_shown_with_a_dash_and_typed_any_way() {
        let code = new_code();
        assert_eq!(code.len(), CODE_LENGTH);
        let shown = shown(&code);
        assert_eq!(shown.len(), CODE_LENGTH + 1);
        assert_eq!(normalize(&shown).as_deref(), Some(code.as_str()));
        assert_eq!(
            normalize(" abcd efgh ").as_deref(),
            Some("ABCDEFGH"),
            "spaces and lower case"
        );
        for bad in [
            "",
            "ABCD-EFG",
            "ABCD-EFGHJ",
            "ABCD-EFG0",
            "ABCD-EFGI",
            "ABCD_EFGH",
        ] {
            assert_eq!(normalize(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn names_are_cut_and_cleaned() {
        assert_eq!(clean_name("  Kobo Clara BW \n"), "Kobo Clara BW");
        assert_eq!(clean_name("\u{7}"), "Reader");
        assert_eq!(clean_name(&"x".repeat(200)).chars().count(), MAX_NAME);
    }
}
