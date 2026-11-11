//! Changing a book, from its page and the shared page.

use axum::{
    Form,
    extract::Path,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use super::to;
use crate::server::{
    auth::session::CurrentUser,
    db::{
        models::{BookDetails, MetadataEdit, Visibility},
        scope::BookError,
    },
    error::AppError,
};

const MAX_TITLE_CHARS: usize = 500;
const MAX_LINE_CHARS: usize = 300;
const MAX_DESCRIPTION_CHARS: usize = 20_000;
const MAX_AUTHORS: usize = 50;

#[derive(Debug, Deserialize)]
pub struct EditForm {
    title: String,
    authors: String,
    series: String,
    series_index: String,
    publisher: String,
    language: String,
    published_on: String,
    #[serde(default)]
    isbn: String,
    description: String,
}

fn one_line(value: &str) -> Option<String> {
    let value: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LINE_CHARS)
        .collect();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

impl EditForm {
    /// `None` if there's nothing a Kobo could show as a title, or the series number
    /// or ISBN isn't one.
    fn clean(self) -> Option<MetadataEdit> {
        let title: String = self
            .title
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_TITLE_CHARS)
            .collect();
        let title = title.trim().to_owned();
        if title.is_empty() {
            return None;
        }
        let index = self.series_index.trim();
        let series_index = if index.is_empty() {
            None
        } else {
            Some(
                index
                    .parse::<f64>()
                    .ok()
                    .filter(|i| i.is_finite() && *i >= 0.0)?,
            )
        };
        let isbn = self.isbn.trim();
        let isbn = if isbn.is_empty() {
            None
        } else {
            Some(epub::isbn::normalize(isbn)?)
        };
        let description: String = self
            .description
            .chars()
            .take(MAX_DESCRIPTION_CHARS)
            .collect();
        let description = description.trim();
        Some(MetadataEdit {
            title,
            authors: self
                .authors
                .lines()
                .filter_map(one_line)
                .take(MAX_AUTHORS)
                .collect(),
            details: BookDetails {
                series: one_line(&self.series),
                series_index,
                description: (!description.is_empty()).then(|| description.to_owned()),
                publisher: one_line(&self.publisher),
                language: one_line(&self.language),
                published_on: one_line(&self.published_on),
                isbn,
            },
        })
    }
}

/// `POST /books/{id}/edit`
pub async fn edit(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    let Some(edit) = form.clean() else {
        return Ok(to(&format!("/books/{book}?error=details")).into_response());
    };
    user.scope.set_metadata(book, &edit).await?;
    Ok(to(&format!("/books/{book}?done=saved")).into_response())
}

#[derive(Debug, Deserialize)]
pub struct VisibilityForm {
    visibility: String,
}

/// `POST /books/{id}/visibility`
pub async fn visibility(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<VisibilityForm>,
) -> Result<Response, AppError> {
    let visibility = match form.visibility.as_str() {
        "private" => Visibility::Private,
        "shared" => Visibility::Shared,
        _ => return Ok(to(&format!("/books/{book}?error=details")).into_response()),
    };
    match user.scope.set_visibility(book, visibility).await {
        Ok(()) => Ok(to(&format!("/books/{book}?done={}", visibility.as_str())).into_response()),
        Err(BookError::AlreadyShared) => {
            Ok(to(&format!("/books/{book}?error=already_shared")).into_response())
        }
        Err(error) => Err(error.into()),
    }
}

/// Where to go back to. A fixed choice, so that a form can't send anyone elsewhere.
#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Back {
    Shared,
    #[default]
    Book,
    /// The library, showing the finished books.
    Finished,
    /// The library, showing the books not on the Kobo.
    Off,
}

#[derive(Debug, Deserialize)]
pub struct BackForm {
    #[serde(default)]
    back: Back,
}

fn back_to(back: Back, book: Uuid, done: &str) -> Response {
    match back {
        Back::Shared => to(&format!("/shared?done={done}")).into_response(),
        Back::Book => to(&format!("/books/{book}?done={done}")).into_response(),
        Back::Finished => to(&format!("/?show=finished&done={done}")).into_response(),
        Back::Off => to(&format!("/?show=off&done={done}")).into_response(),
    }
}

/// `POST /books/{id}/add`: "Add to my Kobo".
pub async fn add(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> Result<Response, AppError> {
    user.scope.add_to_library(book).await?;
    Ok(back_to(form.back, book, "added"))
}

/// `POST /books/{id}/off-kobo`: "Remove from my Kobo", keeping the book in
/// this user's library, with their place in it.
pub async fn off_kobo(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> Result<Response, AppError> {
    user.scope.set_on_device(book, false).await?;
    Ok(back_to(form.back, book, "off_kobo"))
}

/// `POST /books/{id}/on-kobo`: "Put back on my Kobo".
pub async fn on_kobo(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> Result<Response, AppError> {
    user.scope.set_on_device(book, true).await?;
    Ok(back_to(form.back, book, "on_kobo"))
}

/// `POST /books/{id}/remove`: takes someone else's book out of this user's library.
pub async fn remove(
    user: CurrentUser,
    Path(book): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> Result<Response, AppError> {
    user.scope.remove_from_library(book).await?;
    Ok(back_to(form.back, book, "removed"))
}
