//! Errors for plain Axum handlers. Details go to the log, never to the client.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::db::scope::BookError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("not signed in")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("another user already shares this file")]
    AlreadyShared,
    #[error("you already have this file")]
    AlreadyYours,
    #[error("this would go over the storage limit")]
    QuotaExceeded,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Internal(String),
}

impl From<BookError> for AppError {
    fn from(error: BookError) -> Self {
        match error {
            BookError::NotFound => Self::NotFound,
            BookError::Forbidden => Self::Forbidden,
            BookError::AlreadyShared => Self::AlreadyShared,
            BookError::AlreadyYours => Self::AlreadyYours,
            BookError::QuotaExceeded => Self::QuotaExceeded,
            BookError::Db(error) => Self::Db(error),
        }
    }
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::AlreadyShared | Self::AlreadyYours => StatusCode::CONFLICT,
            Self::QuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Db(_) | Self::Io(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Logs anything that isn't an expected client-facing outcome.
    pub fn log(&self) {
        if self.status().is_server_error() {
            tracing::error!(error = %self, "request failed");
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        self.log();
        self.status().into_response()
    }
}
