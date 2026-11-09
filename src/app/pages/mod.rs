//! The pages, and what they share.

mod accounts;
mod admin;
mod book;
mod device;
mod library;
mod messages;

use axum::http::{HeaderName, HeaderValue, StatusCode, header, request::Parts};
use leptos::prelude::*;
use leptos_axum::ResponseOptions;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    SsrMode,
    components::{Route, Router, Routes},
    path,
};

use crate::server::{AppState, auth::session::CurrentUser, db::models::BookCard, now};

/// The HTML document around the app.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    set_document_headers();

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <HydrationScripts options islands=true />
                <MetaTags />
            </head>
            <body>
                <App />
            </body>
        </html>
    }
}

/// Sets the CSP here rather than in middleware, because the nonce is generated
/// per render by `leptos_axum` and is only available inside the app context.
fn set_document_headers() {
    let Some(response) = use_context::<ResponseOptions>() else {
        return;
    };
    let Some(nonce) = leptos::nonce::use_nonce() else {
        return;
    };
    let csp = format!(
        "default-src 'self'; script-src 'nonce-{nonce}' 'strict-dynamic' 'wasm-unsafe-eval'; \
         style-src 'self'; img-src 'self'; connect-src 'self'; object-src 'none'; \
         base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    );
    if let Ok(value) = HeaderValue::from_str(&csp) {
        response.insert_header(HeaderName::from_static("content-security-policy"), value);
    }
    response.insert_header(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, no-transform"),
    );
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="main" href="/pkg/simple-books.css" />
        <Title formatter=|title: String| {
            if title.is_empty() { "simple-books".to_owned() } else { format!("{title} · simple-books") }
        } />
        <Router>
            <Routes fallback=NotFound>
                <Route path=path!("") view=library::LibraryPage ssr=SsrMode::Async />
                <Route path=path!("shared") view=library::SharedPage ssr=SsrMode::Async />
                <Route path=path!("books/:id") view=book::BookPage ssr=SsrMode::Async />
                <Route path=path!("device") view=device::DevicePage ssr=SsrMode::Async />
                <Route path=path!("admin/users") view=admin::UsersPage ssr=SsrMode::Async />
                <Route path=path!("login") view=accounts::LoginPage ssr=SsrMode::Async />
                <Route path=path!("setup/:token") view=accounts::SetupPage ssr=SsrMode::Async />
            </Routes>
        </Router>
    }
}

fn set_status(status: StatusCode) {
    if let Some(response) = use_context::<ResponseOptions>() {
        response.set_status(status);
    }
}

#[component]
fn NotFound() -> impl IntoView {
    set_status(StatusCode::NOT_FOUND);
    view! {
        <main class="narrow">
            <Title text="Not found" />
            <h1>"Not found"</h1>
            <p><a href="/">"Back to your books"</a></p>
        </main>
    }
}

fn request_parts() -> Parts {
    expect_context::<Parts>()
}

fn app_state() -> AppState {
    expect_context::<AppState>()
}

/// The gate middleware has already turned away anyone who shouldn't be here.
fn signed_in() -> Option<CurrentUser> {
    request_parts().extensions.get::<CurrentUser>().cloned()
}

/// A value from the page's query string.
fn query(name: &str) -> Option<String> {
    let parts = request_parts();
    let map =
        axum::extract::Query::<std::collections::HashMap<String, String>>::try_from_uri(&parts.uri)
            .ok()?;
    map.0.get(name).cloned()
}

/// For a failure the person can do nothing about. Details go to the log only.
fn server_error(error: &dyn std::fmt::Display) -> AnyView {
    tracing::error!(%error, "a page failed to load");
    set_status(StatusCode::INTERNAL_SERVER_ERROR);
    view! { <p class="flash error">"Something went wrong on the server. Please try again."</p> }
        .into_any()
}

fn not_found_view() -> AnyView {
    set_status(StatusCode::NOT_FOUND);
    view! {
        <h1>"Not found"</h1>
        <p><a href="/">"Back to your books"</a></p>
    }
    .into_any()
}

/// Bytes as a person would say them.
#[expect(clippy::cast_precision_loss, reason = "one decimal place, for reading")]
fn human_size(bytes: i64) -> String {
    const MIB: i64 = 1024 * 1024;
    const GIB: i64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{} MB", bytes / MIB)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// How long ago, roughly.
fn time_ago(at: i64) -> String {
    let seconds = (now() - at).max(0);
    let plural = |count: i64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => plural(seconds / 60, "minute"),
        3600..86_400 => plural(seconds / 3600, "hour"),
        _ => plural(seconds / 86_400, "day"),
    }
}

/// A message from the last thing done, chosen from a fixed list: nothing from the
/// URL itself ever appears on the page.
#[component]
fn Flash() -> impl IntoView {
    let done = query("done").and_then(|code| messages::done(&code));
    let error = query("error").and_then(|code| messages::error(&code));
    view! {
        {done.map(|text| view! { <p class="flash" role="status">{text}</p> })}
        {error.map(|text| view! { <p class="flash error" role="alert">{text}</p> })}
    }
}

/// Every signed-in page.
#[component]
fn Layout(user: CurrentUser, #[prop(into)] title: String, children: Children) -> impl IntoView {
    view! {
        <Title text=title />
        <header class="site">
            <a class="brand" href="/">"simple-books"</a>
            <nav>
                <a href="/">"My books"</a>
                <a href="/shared">"Shared"</a>
                <a href="/device">"My Kobo"</a>
                {user.is_admin.then(|| view! { <a href="/admin/users">"Users"</a> })}
                <form method="post" action="/logout">
                    <button class="link" type="submit">"Log out"</button>
                </form>
            </nav>
        </header>
        <main>
            <Flash />
            {children()}
        </main>
    }
}

fn cover_url(card_id: &str, cover: &str) -> String {
    format!("/books/{card_id}/cover/{cover}/thumb.jpg")
}

/// One book in a grid. `actions` are whatever buttons the page wants under it.
#[component]
fn BookTile(card: BookCard, #[prop(optional)] children: Option<Children>) -> impl IntoView {
    let href = format!("/books/{}", card.id);
    let authors = card.authors().join(", ");
    let picture = match &card.cover_id {
        Some(cover) => view! {
            <img src=cover_url(&card.id, cover) alt="" loading="lazy" />
        }
        .into_any(),
        None => view! { <span class="no-cover">{card.title.clone()}</span> }.into_any(),
    };
    let preparing = card.kepub_state == "pending";
    view! {
        <li class="book">
            // The title below links to the same place, so this one is for the mouse only.
            <a class="cover" href=href.clone() tabindex="-1" aria-hidden="true">{picture}</a>
            <a class="title" href=href>{card.title}</a>
            <span class="authors">{authors}</span>
            {preparing.then(|| view! {
                <span class="badge">"Getting it ready for your Kobo"</span>
            })}
            {children.map(|children| children())}
        </li>
    }
}
