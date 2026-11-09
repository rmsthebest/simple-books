//! Signing in and out, and setting a password from a link.

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, request::Parts},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::to;
use crate::server::{
    AppState,
    auth::{
        accounts::{self, AccountError},
        session,
    },
    error::AppError,
    http::client_ip,
    now,
};

const MAX_USERNAME_KEY_CHARS: usize = 64;

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

fn signed_in(token: &str) -> Response {
    let mut response = to("/").into_response();
    session::set_cookie(&mut response, token);
    response
}

/// `POST /login`. Each failure looks the same, and only failures count against the
/// limits, so a person with the right password is never held up by someone else.
pub async fn log_in(
    State(state): State<AppState>,
    parts: Parts,
    Form(form): Form<LoginForm>,
) -> Response {
    let wrong = || to("/login?error=wrong").into_response();

    let ip = client_ip::limit_key(client_ip::from_request(
        &state.config,
        &parts.headers,
        &parts.extensions,
    ));
    let username: String = form
        .username
        .chars()
        .take(MAX_USERNAME_KEY_CHARS)
        .collect::<String>()
        .to_lowercase();
    let limits = &state.limits;
    if !limits.login_ip.acquire(&ip) {
        return to("/login?error=slow").into_response();
    }
    if !limits.login_user.acquire(&username) {
        limits.login_ip.refund(&ip);
        return to("/login?error=slow").into_response();
    }

    match accounts::log_in(&state.db, &form.username, form.password).await {
        Ok(Some(token)) => {
            limits.login_ip.refund(&ip);
            limits.login_user.refund(&username);
            signed_in(&token)
        }
        Ok(None) => wrong(),
        Err(error) => {
            tracing::error!(%error, "login failed");
            AppError::Internal("login failed".to_owned()).into_response()
        }
    }
}

/// `POST /logout`
pub async fn log_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    session::end(&state.db, &headers).await?;
    let mut response = to("/login").into_response();
    session::clear_cookie(&mut response);
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub struct PasswordForm {
    /// Only on the first admin's form: everyone else's was chosen by an admin.
    #[serde(default)]
    username: Option<String>,
    password: String,
    confirm: String,
}

/// `POST /setup/{token}`
pub async fn set_password(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Form(form): Form<PasswordForm>,
) -> Response {
    let back = |error: &str| to(&format!("/setup/{token}?error={error}")).into_response();
    if form.password != form.confirm {
        return back("mismatch");
    }
    let done = match form.username {
        Some(username) if state.first_run.is_valid(&token, now()) => {
            let quota = i64::try_from(state.config.default_quota_bytes).unwrap_or(i64::MAX);
            accounts::claim_first_run(
                &state.db,
                &state.first_run,
                &token,
                username.trim(),
                form.password,
                quota,
            )
            .await
        }
        _ => accounts::complete_setup(&state.db, &token, form.password).await,
    };
    match done {
        Ok(session) => signed_in(&session),
        Err(AccountError::InvalidUsername) => back("username"),
        Err(AccountError::PasswordTooShort) => back("short"),
        Err(AccountError::PasswordTooLong) => back("long"),
        Err(AccountError::InvalidSetupLink) => back("expired"),
        Err(error) => {
            tracing::error!(%error, "setting a password failed");
            AppError::Internal("setup failed".to_owned()).into_response()
        }
    }
}
