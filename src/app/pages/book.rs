//! One book: details for anyone who can see it, and the controls for those who may
//! change it. Only there for when something's wrong.

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;

use super::{Layout, human_size, not_found_view, server_error, signed_in};
use crate::{
    app::cover_picker::CoverPicker,
    server::{
        db::{
            hardcover::BookLink,
            models::{BookRow, Reading},
        },
        hardcover::rules::book_link,
    },
};

fn or_empty(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

/// For the text inside a `<textarea>`. Leptos doesn't escape that, as it does other
/// text, and book metadata comes from uploaded files. `</textarea>` in a description
/// would otherwise end the form field and start whatever follows. The browser decodes
/// these again, so the person editing sees the original text.
fn textarea_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;")
}

fn people(count: i64) -> String {
    if count == 1 {
        "1 other person has".to_owned()
    } else {
        format!("{count} other people have")
    }
}

#[component]
pub fn BookPage() -> impl IntoView {
    let user = signed_in();
    let id = use_params_map()
        .read_untracked()
        .get("id")
        .and_then(|id| Uuid::parse_str(&id).ok());

    user.map(|user| {
        let scope = user.scope.clone();
        let user_id = user.id;
        let is_admin = user.is_admin;
        view! {
            <Layout user title="Book">
                <Suspense>
                    {Suspend::new(async move {
                        let Some(id) = id else { return not_found_view() };
                        let book = match scope.readable_book(id).await {
                            Ok(Some(book)) => book,
                            Ok(None) => return not_found_view(),
                            Err(error) => return server_error(&error),
                        };
                        let others = match scope.other_holders(id).await {
                            Ok(others) => others,
                            Err(error) => return server_error(&error),
                        };
                        let hardcover = match scope.hardcover_book(id).await {
                            Ok(link) => link,
                            Err(error) => return server_error(&error),
                        };
                        let reading = match scope.reading(id).await {
                            Ok(reading) => reading,
                            Err(error) => return server_error(&error),
                        };
                        let editable = book.editable_by(user_id, is_admin);
                        let mine = book.owner_id == user_id;
                        view! {
                            <BookDetails book=book editable=editable mine=mine others=others hardcover=hardcover reading=reading />
                        }
                        .into_any()
                    })}
                </Suspense>
            </Layout>
        }
    })
}

#[component]
fn BookDetails(
    book: BookRow,
    editable: bool,
    mine: bool,
    others: i64,
    hardcover: Option<BookLink>,
    /// How far through it this user is, and whether it's on their Kobo, if
    /// it's in their library.
    reading: Option<(Reading, bool)>,
) -> impl IntoView {
    let id = book.id.clone();
    let authors = book.authors().join(", ");
    let picture = book.cover_id.as_ref().map(|cover| {
        view! { <img src=format!("/books/{id}/cover/{cover}/full.jpg") alt="" /> }
    });
    let series = book.series.as_ref().map(|series| match book.series_index {
        Some(index) => format!("{series} #{index}"),
        None => series.clone(),
    });
    let in_library = book.first_seq.is_some() || mine;
    let on_device = reading.is_none_or(|(_, on)| on);
    let on_kobo = match book.kepub_state.as_str() {
        _ if !on_device => "Not on your Kobo.",
        "pending" => "Getting it ready for your Kobo.",
        "failed" => "Your Kobo will get this as a plain EPUB, because it couldn't be converted.",
        _ => "Ready for your Kobo.",
    };
    let reading_said = reading.and_then(|(reading, _)| reading.shown());
    let size = human_size(book.original_size);
    let visibility = if book.visibility == "shared" {
        "Shared with everyone"
    } else {
        "Just for you"
    };

    view! {
        <article class="book-page">
            <div class="big-cover">{picture}</div>
            <div class="about">
                <h1>{book.title.clone()}</h1>
                <p class="authors">{authors}</p>
                {series.map(|series| view! { <p class="muted">{series}</p> })}
                {book.description.clone().map(|text| view! { <p class="description">{text}</p> })}
                {reading_said.map(|said| view! { <p class="reading">{said}</p> })}
                <p class="muted">{visibility} " · " {size} " · " {on_kobo}</p>
                <div class="actions">
                    <a class="button quiet" href=format!("/books/{id}/download")>"Download EPUB"</a>
                    {(mine && reading.is_some()).then(|| {
                        let (path, label) = if on_device {
                            ("off-kobo", "Remove from my Kobo")
                        } else {
                            ("on-kobo", "Put back on my Kobo")
                        };
                        view! {
                            <form method="post" action=format!("/books/{id}/{path}")>
                                <button class="quiet" type="submit">{label}</button>
                            </form>
                        }
                    })}
                    {(!mine).then(|| {
                        let (path, label) = if in_library {
                            ("remove", "Remove from my Kobo")
                        } else {
                            ("add", "Add to my Kobo")
                        };
                        view! {
                            <form method="post" action=format!("/books/{id}/{path}")>
                                <button type="submit">{label}</button>
                            </form>
                        }
                    })}
                </div>
                {hardcover.map(|link| view! { <HardcoverLink id=id.clone() link=link /> })}
            </div>
        </article>
        {editable.then(|| view! { <BookControls book=book mine=mine others=others /> })}
    }
}

#[component]
fn BookControls(book: BookRow, mine: bool, others: i64) -> impl IntoView {
    let id = book.id.clone();
    let authors = book.authors().join("\n");
    let shared = book.visibility == "shared";
    let index = book
        .series_index
        .map(|index| index.to_string())
        .unwrap_or_default();
    let warning = (others > 0).then(|| {
        format!(
            "{} this book. It will leave their Kobos at their next sync.",
            people(others)
        )
    });

    view! {
        <section class="controls">
            <h2>"Fix the details"</h2>
            <p class="muted">"Your Kobo shows what you save here. The file itself doesn't change."</p>
            <form class="stack" method="post" action=format!("/books/{id}/edit")>
                <label>"Title"<input name="title" value=book.title.clone() required /></label>
                <label>"Authors, one per line"<textarea name="authors" rows="2">{textarea_text(&authors)}</textarea></label>
                <div class="row">
                    <label>"Series"<input name="series" value=or_empty(book.series.as_ref()) /></label>
                    <label class="small">"Number"<input name="series_index" inputmode="decimal" value=index /></label>
                </div>
                <div class="row">
                    <label>"Publisher"<input name="publisher" value=or_empty(book.publisher.as_ref()) /></label>
                    <label class="small">"Language"<input name="language" value=or_empty(book.language.as_ref()) /></label>
                    <label class="small">"Published"<input name="published_on" value=or_empty(book.published_on.as_ref()) /></label>
                    <label class="small">"ISBN"<input name="isbn" inputmode="numeric" value=or_empty(book.isbn.as_ref()) /></label>
                </div>
                <label>"Description"
                    <textarea name="description" rows="6">{textarea_text(&or_empty(book.description.as_ref()))}</textarea>
                </label>
                <button type="submit">"Save"</button>
            </form>

            <h2>"Change the cover"</h2>
            <CoverPicker action=format!("/books/{id}/cover") />

            <h2>"Who can have it"</h2>
            <form method="post" action=format!("/books/{id}/visibility")>
                {if shared {
                    view! {
                        <p>"Shared with everyone."</p>
                        {warning.clone().map(|text| view! { <p class="warning">{text}</p> })}
                        <input type="hidden" name="visibility" value="private" />
                        <button class="quiet" type="submit">
                            {if mine { "Make it just for me" } else { "Make it just for the uploader" }}
                        </button>
                    }
                    .into_any()
                } else {
                    view! {
                        <p>"Just for you."</p>
                        <input type="hidden" name="visibility" value="shared" />
                        <button class="quiet" type="submit">"Share with everyone"</button>
                    }
                    .into_any()
                }}
            </form>

            <h2>"Delete"</h2>
            <details>
                <summary>"Delete this book"</summary>
                {warning.map(|text| view! { <p class="warning">{text}</p> })}
                <p>"This can't be undone."</p>
                <form method="post" action=format!("/books/{id}/delete")>
                    <button class="danger" type="submit">"Yes, delete it"</button>
                </form>
            </details>
        </section>
    }
}

/// Where this book is on Hardcover, for someone who connected it. A book it couldn't
/// find, or found wrongly, can be pointed at the right one by pasting its link.
#[component]
fn HardcoverLink(id: String, link: BookLink) -> impl IntoView {
    let (line, open) = match &link {
        BookLink::Looking => (
            Some(view! { <span>"Looking for it on Hardcover…"</span> }.into_any()),
            false,
        ),
        BookLink::Found { slug } => (
            slug.as_deref().and_then(book_link).map(|href| {
                view! {
                    <a href=href target="_blank" rel="noopener noreferrer">"See it on Hardcover"</a>
                }
                .into_any()
            }),
            false,
        ),
        BookLink::Missing => (
            Some(
                view! { <span class="warning">"Couldn't find this book on Hardcover."</span> }
                    .into_any(),
            ),
            true,
        ),
    };
    view! {
        <div class="hardcover-link">
            {line}
            <details open=open>
                <summary>{if open { "Say which book it is" } else { "Wrong book?" }}</summary>
                <form class="inline" method="post" action=format!("/books/{id}/hardcover")>
                    <input name="link" type="url" required placeholder="https://hardcover.app/books/…"
                        aria-label="The book's link on Hardcover" />
                    <button class="quiet" type="submit">"Use this book"</button>
                </form>
            </details>
        </div>
    }
}
