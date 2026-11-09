//! The background task that talks to Hardcover: finishing sign-ins, finding books,
//! and sending status and progress. One request at a time, at most one a second.

use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Hardcover,
    api::{ApiError, Poll},
    rules::{self, Change, READING},
};
use crate::server::{
    db::{
        Db,
        hardcover::{Account, Due, Found},
    },
    now,
};

/// How often to look for work. Sign-ins are polled about this often, so a person
/// who has just entered their code doesn't wait long.
const TICK: Duration = Duration::from_secs(5);
const BATCH: i64 = 20;
/// Tokens are refreshed this long before they run out.
const REFRESH_MARGIN: i64 = 600;
/// Waits between tries after a failure, in seconds. The last one repeats.
const BACKOFF: [i64; 6] = [60, 300, 1800, 7200, 21_600, 43_200];
/// A refusal is retried a few times, in case it was a passing problem, then dropped.
const MAX_REFUSED: i64 = 4;
/// Network trouble is retried for about a week.
const MAX_UNAVAILABLE: i64 = 20;

pub async fn run(db: Db, hardcover: Arc<Hardcover>) {
    loop {
        if let Err(error) = run_once(&db, &hardcover).await {
            tracing::error!(%error, "Hardcover queue failed");
        }
        tokio::time::sleep(TICK).await;
    }
}

/// One pass: sign-ins waiting on their code, then whatever is due to be sent.
pub async fn run_once(db: &Db, hardcover: &Hardcover) -> Result<(), sqlx::Error> {
    run_once_at(db, hardcover, now()).await
}

/// One pass, sending what is due by `at`. Tests use it to skip ahead.
pub async fn run_once_at(db: &Db, hardcover: &Hardcover, at: i64) -> Result<(), sqlx::Error> {
    poll_sign_ins(db, hardcover, at).await?;
    for due in db.hardcover_due(at, BATCH).await? {
        send(db, hardcover, &due).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct Me {
    me: Vec<MeRow>,
}

#[derive(Deserialize)]
struct MeRow {
    id: i64,
    username: Option<String>,
}

async fn poll_sign_ins(db: &Db, hardcover: &Hardcover, at: i64) -> Result<(), sqlx::Error> {
    for pending in db.hardcover_due_sign_ins(at).await? {
        let (user, code) = (pending.user_id, pending.device_code.as_str());
        if pending.expires_at <= at {
            db.hardcover_sign_in_ended(user, code, "expired").await?;
            continue;
        }
        hardcover.pace().await;
        match hardcover.api.poll_sign_in(code).await {
            Ok(Poll::Waiting) => {
                db.hardcover_poll_later(user, code, pending.poll_seconds, now())
                    .await?;
            }
            Ok(Poll::SlowDown) => {
                let slower = (pending.poll_seconds + 5).min(60);
                db.hardcover_poll_later(user, code, slower, now()).await?;
            }
            Ok(Poll::Approved(tokens)) => {
                hardcover.pace().await;
                let me = hardcover
                    .api
                    .graphql::<Me>(
                        &tokens.access_token,
                        "query { me { id username } }",
                        json!({}),
                    )
                    .await;
                let Some(me) = me.ok().and_then(|me| me.me.into_iter().next()) else {
                    tracing::warn!(
                        user,
                        "Hardcover sign-in worked, but couldn't read the account"
                    );
                    db.hardcover_sign_in_ended(user, code, "failed").await?;
                    hardcover.api.revoke(&tokens.refresh_token).await;
                    continue;
                };
                let username: String = me
                    .username
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect();
                let at = now();
                let stored = db
                    .hardcover_connect(
                        user,
                        code,
                        me.id,
                        &username,
                        &tokens.access_token,
                        at + tokens.expires_in.clamp(60, 31_536_000),
                        &tokens.refresh_token,
                        at,
                    )
                    .await?;
                if stored {
                    tracing::info!(user, "Hardcover connected");
                } else {
                    // Cancelled while it was being approved.
                    hardcover.api.revoke(&tokens.refresh_token).await;
                }
            }
            Ok(Poll::Denied) => db.hardcover_sign_in_ended(user, code, "denied").await?,
            Ok(Poll::Expired) => db.hardcover_sign_in_ended(user, code, "expired").await?,
            Err(error) => {
                tracing::warn!(%error, user, "Hardcover sign-in check failed");
                db.hardcover_poll_later(user, code, pending.poll_seconds, now())
                    .await?;
            }
        }
    }
    Ok(())
}

/// New tokens for an account. `None` if they couldn't be had: either the
/// connection is gone, and is marked so, or Hardcover is unreachable for now.
async fn refresh(
    db: &Db,
    hardcover: &Hardcover,
    account: &Account,
) -> Result<Option<Account>, sqlx::Error> {
    hardcover.pace().await;
    match hardcover.api.refresh(&account.refresh_token).await {
        Ok(tokens) => {
            let expires = now() + tokens.expires_in.clamp(60, 31_536_000);
            db.hardcover_save_tokens(
                account.user_id,
                &account.refresh_token,
                &tokens.access_token,
                expires,
                &tokens.refresh_token,
            )
            .await?;
            Ok(Some(Account {
                access_token: tokens.access_token,
                access_expires: expires,
                refresh_token: tokens.refresh_token,
                ..account.clone()
            }))
        }
        Err(ApiError::Unauthorized) => {
            tracing::warn!(
                user = account.user_id,
                "Hardcover no longer accepts this connection"
            );
            db.hardcover_mark_broken(account.user_id, &account.refresh_token, now())
                .await?;
            Ok(None)
        }
        Err(error) => {
            tracing::warn!(%error, user = account.user_id, "couldn't refresh a Hardcover token");
            db.hardcover_hold_user(account.user_id, now() + BACKOFF[1])
                .await?;
            Ok(None)
        }
    }
}

enum Failure {
    Db(sqlx::Error),
    Api(ApiError),
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

async fn send(db: &Db, hardcover: &Hardcover, due: &Due) -> Result<(), sqlx::Error> {
    let Some(mut account) = db.hardcover_account(due.user_id).await? else {
        return Ok(());
    };
    if account.access_expires <= now() + REFRESH_MARGIN {
        match refresh(db, hardcover, &account).await? {
            Some(fresh) => account = fresh,
            None => return Ok(()),
        }
    }

    let error = match deliver(db, hardcover, &account, due).await {
        Ok(()) => return Ok(()),
        Err(Failure::Db(error)) => return Err(error),
        Err(Failure::Api(error)) => error,
    };
    let book = due.book_id.as_str();
    match error {
        ApiError::Unauthorized => {
            // The access token was refused before it ran out: refresh, and go again
            // shortly. If refreshing fails too, the connection is marked broken.
            if refresh(db, hardcover, &account).await?.is_some() {
                db.hardcover_retry(due, Some(now() + 5)).await?;
            }
        }
        ApiError::RateLimited(wait) => {
            let wait = i64::try_from(wait.as_secs()).unwrap_or(BACKOFF[1]);
            db.hardcover_hold_user(due.user_id, now() + wait).await?;
        }
        ApiError::Refused(reason) => {
            tracing::warn!(user = due.user_id, book, %reason, "Hardcover refused an update");
            db.hardcover_retry(due, retry_at(due.attempts, MAX_REFUSED))
                .await?;
        }
        ApiError::Unavailable(reason) => {
            tracing::warn!(user = due.user_id, book, %reason, "Hardcover unavailable");
            db.hardcover_retry(due, retry_at(due.attempts, MAX_UNAVAILABLE))
                .await?;
        }
    }
    Ok(())
}

fn retry_at(attempts: i64, max: i64) -> Option<i64> {
    let index = usize::try_from(attempts).unwrap_or(usize::MAX);
    (attempts + 1 < max).then(|| now() + BACKOFF[index.min(BACKOFF.len() - 1)])
}

/// A `user_books` row: the book on the user's shelves.
#[derive(Deserialize)]
struct Shelved {
    id: i64,
    status_id: i64,
    #[serde(default)]
    user_book_reads: Vec<IdOnly>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: i64,
}

#[derive(Deserialize)]
struct Written {
    error: Option<String>,
    id: Option<i64>,
}

impl Written {
    fn id(self) -> Result<i64, ApiError> {
        match (self.error, self.id) {
            (None, Some(id)) => Ok(id),
            (Some(error), _) => Err(ApiError::Refused(error.chars().take(200).collect())),
            (None, None) => Err(ApiError::Refused("nothing written".to_owned())),
        }
    }
}

async fn query<T: serde::de::DeserializeOwned>(
    hardcover: &Hardcover,
    account: &Account,
    text: &str,
    variables: Value,
) -> Result<T, ApiError> {
    hardcover.pace().await;
    hardcover
        .api
        .graphql(&account.access_token, text, variables)
        .await
}

/// Takes the one field a mutation returns.
async fn write(
    hardcover: &Hardcover,
    account: &Account,
    field: &str,
    text: &str,
    variables: Value,
) -> Result<i64, ApiError> {
    let mut data: serde_json::Map<String, Value> =
        query(hardcover, account, text, variables).await?;
    let written: Written = data
        .remove(field)
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or_else(|| ApiError::Unavailable(format!("no {field} in the answer")))?;
    written.id()
}

async fn deliver(
    db: &Db,
    hardcover: &Hardcover,
    account: &Account,
    due: &Due,
) -> Result<(), Failure> {
    let found = if let (true, Some(book_id)) = (due.matched, due.hc_book_id) {
        Found {
            book_id,
            edition_id: due.hc_edition_id,
            slug: None,
            pages: due.hc_pages,
        }
    } else {
        let found = find(hardcover, account, due).await?;
        db.hardcover_matched(due, found.as_ref()).await?;
        let Some(found) = found else {
            return Ok(());
        };
        found
    };

    let shelved = shelved(hardcover, account, found.book_id).await?;
    let Some((user_book, status, changed)) =
        set_status(hardcover, account, due, &found, shelved.as_ref()).await?
    else {
        return Ok(());
    };

    let mut sent_pages = None;
    if status == READING
        && let Some(pages) = rules::pages_read(due.want_percent, found.pages)
        && (changed || due.sent_pages != Some(pages))
    {
        // Moving a book to "currently reading" starts a read on Hardcover's side, so
        // after a change, look again rather than trust what was read before.
        let open_read = if changed {
            shelved_read(hardcover, account, user_book).await?
        } else {
            shelved
                .as_ref()
                .and_then(|s| s.user_book_reads.first())
                .map(|read| read.id)
        };
        set_progress(hardcover, account, user_book, open_read, &found, pages).await?;
        sent_pages = Some(pages);
    }
    db.hardcover_sent(due, status, sent_pages).await?;
    Ok(())
}

/// Puts the book on the user's shelves, or moves it along. Returns its
/// `user_books` id, its status now, and whether this changed it.
async fn set_status(
    hardcover: &Hardcover,
    account: &Account,
    due: &Due,
    found: &Found,
    shelved: Option<&Shelved>,
) -> Result<Option<(i64, i64, bool)>, ApiError> {
    Ok(
        match (
            rules::change_for(due.want_status, shelved.map(|s| s.status_id)),
            shelved,
        ) {
            (Change::Add(status), _) => {
                let mut object = json!({ "book_id": found.book_id, "status_id": status });
                if let Some(edition) = found.edition_id {
                    object["edition_id"] = json!(edition);
                }
                let id = write(
                    hardcover,
                    account,
                    "insert_user_book",
                    "mutation ($object: UserBookCreateInput!) {
                         insert_user_book(object: $object) { error id }
                     }",
                    json!({ "object": object }),
                )
                .await?;
                Some((id, status, true))
            }
            (Change::Move(status), Some(shelved)) => {
                write(
                    hardcover,
                    account,
                    "update_user_book",
                    "mutation ($id: Int!, $status: Int!) {
                         update_user_book(id: $id, object: { status_id: $status }) { error id }
                     }",
                    json!({ "id": shelved.id, "status": status }),
                )
                .await?;
                Some((shelved.id, status, true))
            }
            (_, Some(shelved)) => Some((shelved.id, shelved.status_id, false)),
            (_, None) => None,
        },
    )
}

async fn set_progress(
    hardcover: &Hardcover,
    account: &Account,
    user_book: i64,
    open_read: Option<i64>,
    found: &Found,
    pages: i64,
) -> Result<(), ApiError> {
    if let Some(read) = open_read {
        write(
            hardcover,
            account,
            "update_user_book_read",
            "mutation ($id: Int!, $pages: Int!) {
                 update_user_book_read(id: $id, object: { progress_pages: $pages }) { error id }
             }",
            json!({ "id": read, "pages": pages }),
        )
        .await?;
        return Ok(());
    }
    let today = time::OffsetDateTime::now_utc().date().to_string();
    let mut read = json!({ "progress_pages": pages, "started_at": today });
    if let Some(edition) = found.edition_id {
        read["edition_id"] = json!(edition);
    }
    write(
        hardcover,
        account,
        "insert_user_book_read",
        "mutation ($id: Int!, $read: DatesReadInput!) {
             insert_user_book_read(user_book_id: $id, user_book_read: $read) { error id }
         }",
        json!({ "id": user_book, "read": read }),
    )
    .await?;
    Ok(())
}

async fn shelved(
    hardcover: &Hardcover,
    account: &Account,
    book_id: i64,
) -> Result<Option<Shelved>, ApiError> {
    #[derive(Deserialize)]
    struct Answer {
        user_books: Vec<Shelved>,
    }
    let answer: Answer = query(
        hardcover,
        account,
        "query ($user: Int!, $book: Int!) {
             user_books(where: { user_id: { _eq: $user }, book_id: { _eq: $book } }, limit: 1) {
                 id status_id
                 user_book_reads(where: { finished_at: { _is_null: true } },
                                 order_by: { id: desc }, limit: 1) { id }
             }
         }",
        json!({ "user": account.hardcover_id, "book": book_id }),
    )
    .await?;
    Ok(answer.user_books.into_iter().next())
}

async fn shelved_read(
    hardcover: &Hardcover,
    account: &Account,
    user_book: i64,
) -> Result<Option<i64>, ApiError> {
    #[derive(Deserialize)]
    struct Answer {
        user_book_reads: Vec<IdOnly>,
    }
    let answer: Answer = query(
        hardcover,
        account,
        "query ($id: Int!) {
             user_book_reads(where: { user_book_id: { _eq: $id }, finished_at: { _is_null: true } },
                             order_by: { id: desc }, limit: 1) { id }
         }",
        json!({ "id": user_book }),
    )
    .await?;
    Ok(answer.user_book_reads.first().map(|read| read.id))
}

#[derive(Deserialize)]
struct CatalogBook {
    id: i64,
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    pages: Option<i64>,
    #[serde(default)]
    contributions: Vec<Contribution>,
}

impl CatalogBook {
    fn found(self, edition: Option<(i64, Option<i64>)>) -> Found {
        Found {
            book_id: self.id,
            edition_id: edition.map(|(id, _)| id),
            slug: self.slug,
            pages: edition.and_then(|(_, pages)| pages).or(self.pages),
        }
    }

    fn author_names(&self) -> Vec<String> {
        self.contributions
            .iter()
            .filter_map(|c| c.author.as_ref().map(|a| a.name.clone()))
            .collect()
    }
}

#[derive(Deserialize)]
struct Contribution {
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    name: String,
}

#[derive(Deserialize)]
struct Books {
    books: Vec<CatalogBook>,
}

/// Finds the book: the link the user gave, else its ISBN, else a search by title
/// and author that only accepts a close match.
async fn find(
    hardcover: &Hardcover,
    account: &Account,
    due: &Due,
) -> Result<Option<Found>, ApiError> {
    if let Some(slug) = &due.chosen_slug {
        return by_slug(hardcover, account, slug).await;
    }
    if let Some(isbn) = &due.isbn
        && let Some(found) = by_isbn(hardcover, account, isbn).await?
    {
        return Ok(Some(found));
    }
    by_search(hardcover, account, due).await
}

async fn by_slug(
    hardcover: &Hardcover,
    account: &Account,
    slug: &str,
) -> Result<Option<Found>, ApiError> {
    let answer: Books = query(
        hardcover,
        account,
        "query ($slug: String!) { books(where: { slug: { _eq: $slug } }, limit: 1) { id slug pages } }",
        json!({ "slug": slug }),
    )
    .await?;
    Ok(answer.books.into_iter().next().map(|book| book.found(None)))
}

async fn by_isbn(
    hardcover: &Hardcover,
    account: &Account,
    isbn: &str,
) -> Result<Option<Found>, ApiError> {
    #[derive(Deserialize)]
    struct Edition {
        id: i64,
        pages: Option<i64>,
        book: Option<CatalogBook>,
    }
    #[derive(Deserialize)]
    struct Editions {
        editions: Vec<Edition>,
    }
    let answer: Editions = query(
        hardcover,
        account,
        "query ($isbn: String!) {
             editions(where: { isbn_13: { _eq: $isbn } }, limit: 1) {
                 id pages book { id slug pages }
             }
         }",
        json!({ "isbn": isbn }),
    )
    .await?;
    Ok(answer.editions.into_iter().next().and_then(|edition| {
        edition
            .book
            .map(|book| book.found(Some((edition.id, edition.pages))))
    }))
}

#[derive(Deserialize)]
struct SearchIds {
    #[serde(default)]
    ids: Vec<Value>,
}

#[derive(Deserialize)]
struct Search {
    search: Option<SearchIds>,
}

async fn by_search(
    hardcover: &Hardcover,
    account: &Account,
    due: &Due,
) -> Result<Option<Found>, ApiError> {
    let authors = due.authors();
    let text: String = match authors.first() {
        Some(author) => format!("{} {author}", due.title),
        None => due.title.clone(),
    }
    .chars()
    .take(300)
    .collect();
    let answer: Search = query(
        hardcover,
        account,
        "query ($q: String!) { search(query: $q, query_type: \"Book\", per_page: 5, page: 1) { ids } }",
        json!({ "q": text }),
    )
    .await?;
    // The ids may come as numbers or as strings of digits.
    let ids: Vec<i64> = answer
        .search
        .map(|search| search.ids)
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id.as_i64().or_else(|| id.as_str()?.parse().ok()))
        .take(5)
        .collect();
    if ids.is_empty() {
        return Ok(None);
    }
    let answer: Books = query(
        hardcover,
        account,
        "query ($ids: [Int!]) {
             books(where: { id: { _in: $ids } }) {
                 id slug title subtitle pages contributions { author { name } }
             }
         }",
        json!({ "ids": ids }),
    )
    .await?;
    let mut candidates = answer.books;
    // Keep the search's order: its first close match wins.
    candidates.sort_by_key(|book| ids.iter().position(|id| *id == book.id));
    Ok(candidates
        .into_iter()
        .find(|book| {
            book.title
                .as_deref()
                .is_some_and(|title| rules::same_title(&due.title, title, book.subtitle.as_deref()))
                && rules::same_author(&authors, &book.author_names())
        })
        .map(|book| book.found(None)))
}
