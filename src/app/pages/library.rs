//! Your books, and everyone's shared books.

use leptos::prelude::*;

use super::{BookTile, Layout, app_state, human_size, query, server_error, signed_in};
use crate::{app::upload_zone::UploadZone, server::db::models::BookCard};

fn matches(card: &BookCard, filter: &str) -> bool {
    let filter = filter.to_lowercase();
    card.title.to_lowercase().contains(&filter)
        || card
            .authors()
            .iter()
            .any(|author| author.to_lowercase().contains(&filter))
}

fn apply_filter(cards: Vec<BookCard>, filter: Option<&str>) -> Vec<BookCard> {
    match filter.map(str::trim).filter(|filter| !filter.is_empty()) {
        Some(filter) => cards
            .into_iter()
            .filter(|card| matches(card, filter))
            .collect(),
        None => cards,
    }
}

#[component]
fn FilterForm(action: &'static str, value: String) -> impl IntoView {
    view! {
        <form class="filter" method="get" action=action role="search">
            <input name="q" type="search" placeholder="Find a book" value=value aria-label="Find a book" />
            <button class="quiet" type="submit">"Find"</button>
        </form>
    }
}

#[component]
pub fn LibraryPage() -> impl IntoView {
    let user = signed_in();
    let state = app_state();
    let filter = query("q");

    user.map(|user| {
        let scope = user.scope.clone();
        let user_id = user.id;
        view! {
            <Layout user title="My books">
                <Suspense>
                    {Suspend::new(async move {
                        let loaded = async {
                            Ok::<_, sqlx::Error>((
                                scope.library_cards().await?,
                                state.db.list_devices(user_id).await?,
                                scope.used_bytes().await?,
                                scope.quota_bytes().await?,
                            ))
                        }
                        .await;
                        let (cards, devices, used_bytes, quota) = match loaded {
                            Ok(loaded) => loaded,
                            Err(error) => return server_error(&error),
                        };
                        let total = cards.len();
                        let cards = apply_filter(cards, filter.as_deref());
                        let no_kobo = devices.is_empty();
                        view! {
                            {no_kobo.then(|| view! {
                                <section class="callout">
                                    <h2>"Connect your Kobo"</h2>
                                    <p>"Do this once, and books you add here will reach it."</p>
                                    <form method="post" action="/device/connect">
                                        <button type="submit">"Connect your Kobo"</button>
                                    </form>
                                </section>
                            })}
                            <UploadZone />
                            {(total > 0).then(|| view! {
                                <FilterForm action="/" value=filter.clone().unwrap_or_default() />
                            })}
                            <ul class="grid">
                                {cards.into_iter().map(|card| view! {
                                    <BookTile card=card />
                                }).collect_view()}
                            </ul>
                            {(total == 0).then(|| view! {
                                <p class="muted empty">
                                    "No books yet. Drop an EPUB anywhere on this page."
                                </p>
                            })}
                            <p class="muted usage">
                                {human_size(used_bytes)} " of " {human_size(quota)} " used"
                            </p>
                        }
                        .into_any()
                    })}
                </Suspense>
            </Layout>
        }
    })
}

#[component]
pub fn SharedPage() -> impl IntoView {
    let user = signed_in();
    let filter = query("q");

    user.map(|user| {
        let scope = user.scope.clone();
        let user_id = user.id;
        view! {
            <Layout user title="Shared">
                <h1>"Shared books"</h1>
                <p class="muted">"Books anyone here has shared. Add the ones you want on your Kobo."</p>
                <Suspense>
                    {Suspend::new(async move {
                        let cards = match scope.shared_cards().await {
                            Ok(cards) => cards,
                            Err(error) => return server_error(&error),
                        };
                        let total = cards.len();
                        let cards = apply_filter(cards, filter.as_deref());
                        view! {
                            <UploadZone />
                            {(total > 0).then(|| view! {
                                <FilterForm action="/shared" value=filter.clone().unwrap_or_default() />
                            })}
                            <ul class="grid">
                                {cards.into_iter().map(|card| {
                                    let shared_by = card.shared_by.clone();
                                    let id = card.id.clone();
                                    let in_library = card.in_library;
                                    let mine = card.owner_id == user_id;
                                    view! {
                                        <BookTile card=card>
                                            {(!mine).then(|| view! {
                                                <span class="muted by">"Shared by " {shared_by}</span>
                                            })}
                                            <ShareButton id=id in_library=in_library mine=mine />
                                        </BookTile>
                                    }
                                }).collect_view()}
                            </ul>
                            {(total == 0).then(|| view! {
                                <p class="muted empty">"Nobody has shared a book yet."</p>
                            })}
                        }
                        .into_any()
                    })}
                </Suspense>
            </Layout>
        }
    })
}

#[component]
fn ShareButton(id: String, in_library: bool, mine: bool) -> impl IntoView {
    if mine {
        return view! { <span class="muted by">"You shared this"</span> }.into_any();
    }
    let (path, label, class) = if in_library {
        ("remove", "Remove", "quiet")
    } else {
        ("add", "Add to my Kobo", "")
    };
    view! {
        <form method="post" action=format!("/books/{id}/{path}")>
            <input type="hidden" name="back" value="shared" />
            <button class=class type="submit">{label}</button>
        </form>
    }
    .into_any()
}
