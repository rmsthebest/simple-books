//! Connecting Hardcover, and choosing a book there by hand. The sign-in happens on
//! Hardcover's own site: this only asks for a code to show, and the background task
//! notices when it has been entered.

use std::time::Duration;

use axum::{
    Form,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use super::to;
use crate::server::{AppState, auth::session::CurrentUser, error::AppError, hardcover::rules, now};

/// Never longer than Hardcover's own limit, or shorter than enough to type a code.
const SIGN_IN_SECONDS: std::ops::RangeInclusive<i64> = 120..=1800;

/// `POST /hardcover/connect`
pub async fn connect(
    user: CurrentUser,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Some(hardcover) = state.hardcover.clone() else {
        return Err(AppError::NotFound);
    };
    hardcover.pace().await;
    let start = match hardcover.api.start_sign_in().await {
        Ok(start) => start,
        Err(error) => {
            tracing::warn!(%error, "couldn't start a Hardcover sign-in");
            return Ok(to("/device?error=hardcover_down").into_response());
        }
    };
    // Shown as a link and as text, so only Hardcover's own pages are accepted.
    let url = start
        .verification_uri_complete
        .as_deref()
        .filter(|url| is_hardcover_page(url))
        .or(Some(start.verification_uri.as_str()).filter(|url| is_hardcover_page(url)));
    let code_ok = !start.user_code.is_empty()
        && start.user_code.len() <= 32
        && start
            .user_code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    let (Some(url), true) = (url, code_ok) else {
        tracing::warn!("Hardcover's sign-in answer looked wrong");
        return Ok(to("/device?error=hardcover_down").into_response());
    };
    let at = now();
    state
        .db
        .hardcover_save_sign_in(
            user.id,
            &start.device_code,
            &start.user_code,
            url,
            start.interval.clamp(1, 60),
            at + start
                .expires_in
                .clamp(*SIGN_IN_SECONDS.start(), *SIGN_IN_SECONDS.end()),
            at,
        )
        .await?;
    Ok(to("/device").into_response())
}

fn is_hardcover_page(url: &str) -> bool {
    url.starts_with("https://hardcover.app/")
        && url.len() <= 300
        && !url.contains(|c: char| c.is_whitespace() || c.is_control() || "\"'<>\\".contains(c))
}

/// `POST /hardcover/cancel`
pub async fn cancel(user: CurrentUser) -> Result<Response, AppError> {
    user.scope.hardcover_cancel_sign_in().await?;
    Ok(to("/device").into_response())
}

/// `POST /hardcover/disconnect`
pub async fn disconnect(
    user: CurrentUser,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let token = state.db.hardcover_disconnect(user.id).await?;
    if let (Some(token), Some(hardcover)) = (token, state.hardcover.clone()) {
        // Ending it on Hardcover's side too is a courtesy, so it can't hold this up.
        tokio::spawn(async move {
            hardcover.pace().await;
            let _ =
                tokio::time::timeout(Duration::from_secs(15), hardcover.api.revoke(&token)).await;
        });
    }
    Ok(to("/device?done=hardcover_off").into_response())
}

#[derive(Debug, Deserialize)]
pub struct LinkForm {
    link: String,
}

/// `POST /books/{id}/hardcover`: the user says which book it is on Hardcover.
pub async fn choose(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<LinkForm>,
) -> Result<Response, AppError> {
    let Some(slug) = rules::slug_from_link(&form.link) else {
        return Ok(to(&format!("/books/{book}?error=hardcover_link")).into_response());
    };
    if !user.scope.hardcover_choose(book, &slug).await? {
        return Err(AppError::NotFound);
    }
    Ok(to(&format!("/books/{book}?done=hardcover_link")).into_response())
}

#[cfg(test)]
mod tests {
    use super::is_hardcover_page;

    #[test]
    fn only_hardcovers_own_pages_are_linked() {
        assert!(is_hardcover_page("https://hardcover.app/link"));
        assert!(is_hardcover_page(
            "https://hardcover.app/link?code=ABCD-1234"
        ));
        for bad in [
            "http://hardcover.app/link",
            "https://hardcover.app.evil.example/link",
            "https://evil.example/https://hardcover.app/",
            "https://hardcover.app/link\" onclick=\"x",
            "javascript:alert(1)",
        ] {
            assert!(!is_hardcover_page(bad), "{bad}");
        }
    }
}
