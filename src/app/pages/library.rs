//! Your books, and everyone's shared books.

use std::fmt::Write as _;

use leptos::prelude::*;

use super::{BookTile, Layout, app_state, human_size, query, server_error, signed_in};
use crate::{
    app::upload_zone::UploadZone,
    server::db::models::{BookCard, Reading},
};

/// Which of the library's books to show, from `?show=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    All,
    Reading,
    Finished,
    /// In the library, but taken off the Kobo.
    OffKobo,
}

impl Show {
    const ALL: [Show; 4] = [Show::All, Show::Reading, Show::Finished, Show::OffKobo];

    fn from_query(value: Option<&str>) -> Self {
        match value {
            Some("reading") => Self::Reading,
            Some("finished") => Self::Finished,
            Some("off") => Self::OffKobo,
            _ => Self::All,
        }
    }

    fn query(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Reading => Some("reading"),
            Self::Finished => Some("finished"),
            Self::OffKobo => Some("off"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Reading => "Reading",
            Self::Finished => "Finished",
            Self::OffKobo => "Not on Kobo",
        }
    }

    fn keeps(self, card: &BookCard) -> bool {
        match self {
            Self::All => true,
            Self::Reading => matches!(card.reading(), Reading::Reading(_)),
            Self::Finished => card.reading() == Reading::Finished,
            Self::OffKobo => !card.on_device,
        }
    }
}

/// All, Reading, Finished and Not on Kobo, each with how many there are,
/// keeping the search.
#[component]
fn ShowLinks(shown: Show, cards: Vec<BookCard>, filter: Option<String>) -> impl IntoView {
    let links = Show::ALL.map(|show| {
        let count = cards.iter().filter(|card| show.keeps(card)).count();
        let mut params = Vec::new();
        if let Some(value) = show.query() {
            params.push(format!("show={value}"));
        }
        if let Some(filter) = filter.as_deref().filter(|f| !f.trim().is_empty()) {
            params.push(format!("q={}", urlencoding(filter)));
        }
        let href = if params.is_empty() {
            "/".to_owned()
        } else {
            format!("/?{}", params.join("&"))
        };
        let current = (show == shown).then_some("page");
        view! {
            <li>
                <a href=href aria-current=current>{show.label()} " (" {count} ")"</a>
            </li>
        }
    });
    view! {
        <nav class="show" aria-label="Which books">
            <ul>{links.into_iter().collect_view()}</ul>
        </nav>
    }
}

/// Just enough percent-encoding for a search in a link.
fn urlencoding(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

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
    let shown = Show::from_query(query("show").as_deref());

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
                        let found = apply_filter(cards, filter.as_deref());
                        let cards: Vec<BookCard> =
                            found.iter().filter(|card| shown.keeps(card)).cloned().collect();
                        let none_shown = cards.is_empty() && total > 0;
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
                                <ShowLinks shown=shown cards=found filter=filter.clone() />
                            })}
                            {(shown == Show::Finished).then(|| view! {
                                <p class="muted">
                                    "Books you've finished. Taking one off your Kobo frees room \
                                     there, and keeps it here, with your place in it."
                                </p>
                            })}
                            {(shown == Show::OffKobo).then(|| view! {
                                <p class="muted">
                                    "Books in your library that aren't on your Kobo. Put one \
                                     back and it's there at the next sync."
                                </p>
                            })}
                            <ul class="grid">
                                {cards.into_iter().map(|card| {
                                    let id = card.id.clone();
                                    let button = match shown {
                                        Show::Finished if card.on_device => Some(view! {
                                            <KoboButton id=id on=false back="finished" />
                                        }),
                                        Show::OffKobo => Some(view! {
                                            <KoboButton id=id on=true back="off" />
                                        }),
                                        _ => None,
                                    };
                                    view! { <BookTile card=card>{button}</BookTile> }
                                }).collect_view()}
                            </ul>
                            {none_shown.then(|| view! {
                                <p class="muted empty">"No books here."</p>
                            })}
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

/// On a tile in the Finished or Not on Kobo view: takes the book off the
/// Kobo, or puts it back, and comes back to the same view.
#[component]
fn KoboButton(id: String, on: bool, back: &'static str) -> impl IntoView {
    let (path, label) = if on {
        ("on-kobo", "Put back on my Kobo")
    } else {
        ("off-kobo", "Remove from my Kobo")
    };
    view! {
        <form method="post" action=format!("/books/{id}/{path}")>
            <input type="hidden" name="back" value=back />
            <button class="quiet" type="submit">{label}</button>
        </form>
    }
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
