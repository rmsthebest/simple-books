//! `GET /status`: what the signed-in user is waiting on. An open page checks it
//! now and then, and shows the change once it does.

use axum::{
    http::header::{CACHE_CONTROL, CONTENT_TYPE},
    response::{IntoResponse, Response},
};

use crate::server::{auth::session::CurrentUser, error::AppError};

pub async fn waiting_on(user: CurrentUser) -> Result<Response, AppError> {
    let summary = user.scope.waiting_on().await?;
    Ok((
        [
            (CONTENT_TYPE, "text/plain; charset=utf-8"),
            (CACHE_CONTROL, "private, no-store"),
        ],
        summary,
    )
        .into_response())
}
