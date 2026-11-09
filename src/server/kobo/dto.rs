//! Wire types for Kobo sync, and conversion from database rows.

use serde::Serialize;
use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use uuid::Uuid;

use crate::server::db::models::{BookRow, ReadingStateRow};

const TIMESTAMP: &[FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
/// The placeholder category Nickel requires to be present.
const DEFAULT_CATEGORY: &str = "00000000-0000-0000-0000-000000000001";
/// Namespace for deterministic series IDs.
const SERIES_NAMESPACE: Uuid = Uuid::from_u128(0x5b2e_8a1c_7c4f_4e0d_9a36_1f0e_6d2b_4c11);

/// Formats a Unix timestamp the way Nickel expects: whole seconds, trailing `Z`.
pub fn kobo_time(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|t| t.format(TIMESTAMP).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ActivePeriod {
    pub from: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BookEntitlement {
    pub accessibility: &'static str,
    /// Nickel silently drops a book if this is missing.
    pub active_period: ActivePeriod,
    pub created: String,
    pub cross_revision_id: String,
    pub id: String,
    pub is_removed: bool,
    pub is_hidden_from_archive: bool,
    pub is_locked: bool,
    pub last_modified: String,
    pub origin_category: &'static str,
    pub revision_id: String,
    pub status: &'static str,
}

impl BookEntitlement {
    fn new(id: &str, created: i64, last_modified: i64, now: i64, is_removed: bool) -> Self {
        Self {
            accessibility: "Full",
            active_period: ActivePeriod {
                from: kobo_time(now),
            },
            created: kobo_time(created),
            cross_revision_id: id.to_owned(),
            id: id.to_owned(),
            is_removed,
            is_hidden_from_archive: false,
            is_locked: false,
            last_modified: kobo_time(last_modified),
            origin_category: "Imported",
            revision_id: id.to_owned(),
            status: "Active",
        }
    }

    pub fn for_book(book: &BookRow, now: i64) -> Self {
        Self::new(&book.id, book.created_at, book.updated_at, now, false)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DownloadUrl {
    pub format: &'static str,
    /// Must equal the response's `Content-Length`, or Nickel rejects the file.
    pub size: i64,
    /// Must start with the device's `api_endpoint`, or Nickel silently skips it.
    pub url: String,
    pub platform: &'static str,
}

/// The download list for a book: the KEPUB if there is one, otherwise the EPUB.
pub fn download_urls(book: &BookRow, base_url: &str) -> Vec<DownloadUrl> {
    if let Some(size) = book.ready_kepub_size() {
        return vec![DownloadUrl {
            format: "KEPUB",
            size,
            url: format!("{base_url}/download/{}/kepub", book.id),
            platform: "Generic",
        }];
    }
    ["EPUB3", "EPUB"]
        .into_iter()
        .map(|format| DownloadUrl {
            format,
            size: book.original_size,
            url: format!("{base_url}/download/{}/epub", book.id),
            platform: "Generic",
        })
        .collect()
}

/// The `BookMetadata` object for a book.
pub fn book_metadata(book: &BookRow, base_url: &str) -> Value {
    let authors = book.authors();
    let roles: Vec<Value> = authors.iter().map(|name| json!({ "Name": name })).collect();
    let language = book
        .language
        .as_deref()
        .filter(|l| !l.is_empty())
        .unwrap_or("en");
    let published = book.published_on.clone().map_or_else(
        || kobo_time(book.created_at),
        |date| format!("{date}T00:00:00Z"),
    );

    let mut metadata = json!({
        "Categories": [DEFAULT_CATEGORY],
        "ContributorRoles": roles,
        "Contributors": authors,
        "CoverImageId": book.cover_id.as_deref().unwrap_or(&book.id),
        "CrossRevisionId": book.id,
        "CurrentDisplayPrice": { "CurrencyCode": "USD", "TotalAmount": 0 },
        "CurrentLoveDisplayPrice": { "TotalAmount": 0 },
        "Description": book.description,
        "DownloadUrls": download_urls(book, base_url),
        "EntitlementId": book.id,
        "ExternalIds": [],
        "Genre": DEFAULT_CATEGORY,
        "IsEligibleForKoboLove": false,
        "IsInternetArchive": false,
        "IsPreOrder": false,
        "IsSocialEnabled": true,
        "Language": language,
        "PhoneticPronunciations": {},
        "PublicationDate": published,
        "Publisher": { "Imprint": "", "Name": book.publisher },
        "RevisionId": book.id,
        "Title": book.title,
        "WorkId": book.id,
    });

    if let (Some(name), Value::Object(fields)) = (&book.series, &mut metadata) {
        let number = book.series_index.unwrap_or(1.0);
        fields.insert(
            "Series".to_owned(),
            json!({
                "Name": name,
                "Number": number,
                "NumberFloat": number,
                "Id": Uuid::new_v5(&SERIES_NAMESPACE, name.as_bytes()),
            }),
        );
    }
    metadata
}

/// The fields of a stored device state that are echoed back. Anything else a
/// device sent is dropped on the way in, so this is bounded and predictable.
pub const STATE_FIELDS: [&str; 3] = ["StatusInfo", "Statistics", "CurrentBookmark"];

/// The `ReadingState` object: what the device last reported, or a fresh state.
pub fn reading_state(book: &BookRow, state: Option<&ReadingStateRow>) -> Value {
    let last_modified = kobo_time(state.map_or(book.updated_at, |s| s.last_modified));
    let stored: Map<String, Value> = state
        .and_then(|s| serde_json::from_str(&s.state_json).ok())
        .unwrap_or_default();

    let field = |name: &str, fresh: Value| stored.get(name).cloned().unwrap_or(fresh);

    json!({
        "EntitlementId": book.id,
        "Created": kobo_time(book.created_at),
        "LastModified": last_modified,
        "PriorityTimestamp": last_modified,
        "StatusInfo": field("StatusInfo", json!({
            "LastModified": last_modified,
            "Status": "ReadyToRead",
            "TimesStartedReading": 0,
        })),
        "Statistics": field("Statistics", json!({ "LastModified": last_modified })),
        "CurrentBookmark": field("CurrentBookmark", json!({ "LastModified": last_modified })),
    })
}

/// A removal: the entitlement plus just enough metadata to identify the book.
pub fn removed_entitlement(id: &str, now: i64) -> Value {
    json!({
        "ChangedEntitlement": {
            "BookEntitlement": BookEntitlement::new(id, now, now, now, true),
            "BookMetadata": {
                "CoverImageId": id,
                "CrossRevisionId": id,
                "EntitlementId": id,
                "RevisionId": id,
                "Title": id,
                "WorkId": id,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> BookRow {
        BookRow {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            owner_id: 1,
            visibility: "private".to_owned(),
            title: "Título".to_owned(),
            authors_json: r#"["Ann","Bob"]"#.to_owned(),
            series: None,
            series_index: None,
            description: None,
            publisher: None,
            language: None,
            published_on: None,
            isbn: None,
            cover_id: None,
            original_size: 100,
            kepub_state: "failed".to_owned(),
            kepub_size: None,
            first_seq: Some(1),
            change_seq: 1,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        }
    }

    const BASE: &str = "https://h/kobo/t";

    #[test]
    fn timestamps_have_whole_seconds_and_a_z() {
        assert_eq!(kobo_time(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn entitlement_has_the_fields_nickel_requires() {
        let value =
            serde_json::to_value(BookEntitlement::for_book(&book(), 1_700_000_200)).unwrap();
        assert_eq!(value["ActivePeriod"]["From"], "2023-11-14T22:16:40Z");
        assert_eq!(value["Accessibility"], "Full");
        assert_eq!(value["OriginCategory"], "Imported");
        assert_eq!(value["Status"], "Active");
        assert_eq!(value["IsRemoved"], false);
        assert_eq!(value["Id"], book().id);
    }

    #[test]
    fn metadata_without_kepub_offers_epub_under_the_endpoint() {
        let value = book_metadata(&book(), BASE);
        let urls = value["DownloadUrls"].as_array().unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0]["Format"], "EPUB3");
        assert_eq!(urls[0]["Size"], 100);
        assert_eq!(urls[0]["Platform"], "Generic");
        assert!(urls[0]["Url"].as_str().unwrap().starts_with(BASE));
    }

    #[test]
    fn metadata_prefers_a_ready_kepub() {
        let mut book = book();
        book.kepub_state = "ready".to_owned();
        book.kepub_size = Some(55);
        let value = book_metadata(&book, BASE);
        let urls = value["DownloadUrls"].as_array().unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0]["Format"], "KEPUB");
        assert_eq!(urls[0]["Size"], 55);
    }

    #[test]
    fn metadata_shape_matches_what_nickel_needs() {
        let value = book_metadata(&book(), BASE);
        assert_eq!(value["Title"], "Título");
        assert_eq!(value["Language"], "en");
        assert_eq!(value["Contributors"], json!(["Ann", "Bob"]));
        assert_eq!(
            value["ContributorRoles"],
            json!([{ "Name": "Ann" }, { "Name": "Bob" }])
        );
        assert_eq!(value["Categories"], json!([DEFAULT_CATEGORY]));
        assert_eq!(value["Genre"], DEFAULT_CATEGORY);
        assert_eq!(value["Publisher"]["Name"], Value::Null);
        assert!(
            value.get("Series").is_none(),
            "Series must be absent, not null"
        );
    }

    #[test]
    fn series_appears_with_a_stable_id() {
        let mut book = book();
        book.series = Some("Saga".to_owned());
        book.series_index = Some(2.5);
        let one = book_metadata(&book, BASE);
        let two = book_metadata(&book, BASE);
        assert_eq!(one["Series"]["Name"], "Saga");
        assert_eq!(one["Series"]["NumberFloat"], 2.5);
        assert_eq!(one["Series"]["Id"], two["Series"]["Id"]);
    }

    #[test]
    fn fresh_reading_state_has_no_progress() {
        let value = reading_state(&book(), None);
        assert_eq!(value["StatusInfo"]["Status"], "ReadyToRead");
        assert!(value["CurrentBookmark"].get("ProgressPercent").is_none());
    }

    #[test]
    fn stored_reading_state_is_echoed() {
        let row = ReadingStateRow {
            state_json: r#"{"CurrentBookmark":{"ProgressPercent":42}}"#.to_owned(),
            last_modified: 1_700_000_300,
        };
        let value = reading_state(&book(), Some(&row));
        assert_eq!(value["CurrentBookmark"]["ProgressPercent"], 42);
        assert_eq!(value["LastModified"], kobo_time(1_700_000_300));
        assert_eq!(value["StatusInfo"]["Status"], "ReadyToRead");
    }

    #[test]
    fn removal_is_marked_removed() {
        let value = removed_entitlement("abc", 1_700_000_000);
        assert_eq!(
            value["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
            true
        );
    }
}
