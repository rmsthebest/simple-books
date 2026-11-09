//! The admin's user list. There's no signup: this is the only way in.

use axum::{
    Form,
    extract::{Path, State},
    http::request::Parts,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{Revealed, render_with, to};
use crate::server::{
    AppState,
    auth::{
        accounts::{self, AccountError},
        session::AdminUser,
    },
    error::AppError,
};

const MIB: i64 = 1024 * 1024;
const MAX_QUOTA_MB: i64 = i64::MAX / MIB;

#[derive(Debug, Deserialize)]
pub struct NewUserForm {
    username: String,
    /// A checkbox: sent only when ticked.
    admin: Option<String>,
}

/// `POST /admin/users`
pub async fn create_user(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    parts: Parts,
    Form(form): Form<NewUserForm>,
) -> Result<Response, AppError> {
    let quota = i64::try_from(state.config.default_quota_bytes).unwrap_or(i64::MAX);
    let id =
        match accounts::create_user(&state.db, &form.username, quota, form.admin.is_some()).await {
            Ok(id) => id,
            Err(AccountError::InvalidUsername) => {
                return Ok(to("/admin/users?error=username").into_response());
            }
            Err(AccountError::UsernameTaken) => {
                return Ok(to("/admin/users?error=taken").into_response());
            }
            Err(error) => return Err(AppError::Internal(error.to_string())),
        };
    let link = accounts::setup_link(&state.db, &state.config.public_url, id)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let revealed = Revealed::SetupLink {
        username: form.username,
        link,
    };
    Ok(render_with(state, user, parts, "/admin/users", revealed).await)
}

/// `POST /admin/users/{id}/setup-link`: also how a forgotten password gets reset.
pub async fn setup_link(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    parts: Parts,
) -> Result<Response, AppError> {
    let target = state
        .db
        .list_users()
        .await?
        .into_iter()
        .find(|candidate| candidate.id == id && !candidate.disabled)
        .ok_or(AppError::NotFound)?;
    let link = accounts::setup_link(&state.db, &state.config.public_url, id)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let revealed = Revealed::SetupLink {
        username: target.username,
        link,
    };
    Ok(render_with(state, user, parts, "/admin/users", revealed).await)
}

/// `POST /admin/users/{id}/disable`. Refused for yourself, or the last admin could
/// lock everyone out.
pub async fn disable(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if id == user.id {
        return Ok(to("/admin/users?error=self").into_response());
    }
    if !state.db.set_user_disabled(id, true).await? {
        return Err(AppError::NotFound);
    }
    Ok(to("/admin/users?done=disabled").into_response())
}

/// `POST /admin/users/{id}/enable`
pub async fn enable(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if !state.db.set_user_disabled(id, false).await? {
        return Err(AppError::NotFound);
    }
    Ok(to("/admin/users?done=enabled").into_response())
}

#[derive(Debug, Deserialize)]
pub struct QuotaForm {
    quota_mb: String,
}

/// `POST /admin/users/{id}/quota`
pub async fn quota(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<QuotaForm>,
) -> Result<Response, AppError> {
    let Some(mb) = form
        .quota_mb
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|mb| (0..=MAX_QUOTA_MB).contains(mb))
    else {
        return Ok(to("/admin/users?error=quota").into_response());
    };
    if !state.db.set_user_quota(id, mb * MIB).await? {
        return Err(AppError::NotFound);
    }
    Ok(to("/admin/users?done=quota").into_response())
}
