//! The decisions, kept apart from the network so they can be tested on their own:
//! what a Kobo status means on Hardcover, how far through a book is in pages,
//! whether a search result is the right book, and what to change on Hardcover.

/// Hardcover's numbers for the statuses this sends.
pub const WANT_TO_READ: i64 = 1;
pub const READING: i64 = 2;
pub const READ: i64 = 3;

/// The status a Kobo's reading state asks for.
pub fn status_from_kobo(status: &str) -> i64 {
    match status {
        "Reading" => READING,
        "Finished" => READ,
        _ => WANT_TO_READ,
    }
}

/// Pages read, from a Kobo's percentage. While reading it stays short of the last
/// page: reaching it would make Hardcover mark the book read, and only the Kobo
/// saying "finished" should do that.
pub fn pages_read(percent: Option<f64>, pages: Option<i64>) -> Option<i64> {
    let percent = percent.filter(|p| p.is_finite())?.clamp(0.0, 100.0);
    let pages = pages.filter(|p| *p > 1 && *p < 100_000)?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "pages is under 100,000, and the result is clamped to it"
    )]
    let read = (percent / 100.0 * pages as f64).round() as i64;
    Some(read.clamp(0, pages - 1))
}

/// What to change about a book on Hardcover, given what it says now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// It isn't on the user's shelves yet: add it with this status.
    Add(i64),
    /// Move it to this status.
    Move(i64),
    Nothing,
}

/// Hardcover is the user's own record, so this only ever moves a book forward. A book
/// marked read stays read, and "want to read" never replaces anything.
pub fn change_for(want: i64, theirs: Option<i64>) -> Change {
    match theirs {
        None => Change::Add(want),
        Some(READ) => Change::Nothing,
        Some(current) if want == WANT_TO_READ || want == current => Change::Nothing,
        // Paused, did not finish, or ignored, and now being read again on the Kobo.
        Some(_) => Change::Move(want),
    }
}

/// Lowercase letters and digits, with everything else as single spaces, so that
/// punctuation and case don't stop two titles matching.
fn simplify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        } else {
            space = true;
        }
    }
    out
}

/// Whether a search result's title is the book's, allowing for a subtitle on either
/// side, since files and catalogues often disagree on whether to include it.
pub fn same_title(ours: &str, title: &str, subtitle: Option<&str>) -> bool {
    let ours_full = simplify(ours);
    let ours_main = simplify(ours.split([':', '(']).next().unwrap_or(ours));
    let theirs = simplify(title);
    let theirs_full = subtitle.map(|sub| simplify(&format!("{title} {sub}")));
    !theirs.is_empty()
        && (ours_full == theirs
            || ours_main == theirs
            || theirs_full.is_some_and(|full| full == ours_full))
}

/// Whether an author on each side shares a surname. A book with no authors in its
/// file can't be checked, so it takes the title's word for it.
pub fn same_author(ours: &[String], theirs: &[String]) -> bool {
    let surname = |name: &str| {
        // "Green, John" and "John Green" are the same person.
        let name = name.split_once(',').map_or(name, |(last, _)| last);
        simplify(name).rsplit(' ').next().map(str::to_owned)
    };
    if ours.is_empty() {
        return true;
    }
    ours.iter().filter_map(|a| surname(a)).any(|ours| {
        theirs
            .iter()
            .filter_map(|t| {
                let simple = simplify(t);
                simple.rsplit(' ').next().map(str::to_owned)
            })
            .any(|theirs| !ours.is_empty() && theirs == ours)
    })
}

/// The book's slug from a Hardcover link, or a slug on its own.
pub fn slug_from_link(text: &str) -> Option<String> {
    let text = text.trim();
    let path = [
        "https://hardcover.app/books/",
        "http://hardcover.app/books/",
        "hardcover.app/books/",
    ]
    .iter()
    .find_map(|prefix| text.strip_prefix(prefix))
    .unwrap_or(text);
    let slug = path.split(['/', '?', '#']).next()?;
    let valid = !slug.is_empty()
        && slug.len() <= 200
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then(|| slug.to_owned())
}

/// A link to the book on Hardcover. The slug came from Hardcover, so it's checked
/// before it goes in a page.
pub fn book_link(slug: &str) -> Option<String> {
    slug_from_link(slug).map(|slug| format!("https://hardcover.app/books/{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kobo_statuses() {
        assert_eq!(status_from_kobo("Reading"), READING);
        assert_eq!(status_from_kobo("Finished"), READ);
        assert_eq!(status_from_kobo("ReadyToRead"), WANT_TO_READ);
        assert_eq!(status_from_kobo("anything else"), WANT_TO_READ);
    }

    #[test]
    fn pages_from_percent() {
        assert_eq!(pages_read(Some(50.0), Some(300)), Some(150));
        assert_eq!(pages_read(Some(0.0), Some(300)), Some(0));
        // Never the last page while reading, so Hardcover doesn't finish it.
        assert_eq!(pages_read(Some(100.0), Some(300)), Some(299));
        assert_eq!(pages_read(Some(250.0), Some(300)), Some(299));
        assert_eq!(pages_read(Some(-5.0), Some(300)), Some(0));
        // Nothing to go on.
        assert_eq!(pages_read(None, Some(300)), None);
        assert_eq!(pages_read(Some(50.0), None), None);
        assert_eq!(pages_read(Some(50.0), Some(0)), None);
        assert_eq!(pages_read(Some(f64::NAN), Some(300)), None);
    }

    #[test]
    fn only_ever_forward() {
        assert_eq!(change_for(WANT_TO_READ, None), Change::Add(WANT_TO_READ));
        assert_eq!(change_for(READING, None), Change::Add(READING));
        assert_eq!(change_for(READ, None), Change::Add(READ));
        // Already on their shelves: "want to read" changes nothing.
        for theirs in 1..=6 {
            assert_eq!(change_for(WANT_TO_READ, Some(theirs)), Change::Nothing);
        }
        assert_eq!(
            change_for(READING, Some(WANT_TO_READ)),
            Change::Move(READING)
        );
        assert_eq!(change_for(READING, Some(READING)), Change::Nothing);
        assert_eq!(change_for(READ, Some(READING)), Change::Move(READ));
        // Read stays read.
        assert_eq!(change_for(READING, Some(READ)), Change::Nothing);
        assert_eq!(change_for(READ, Some(READ)), Change::Nothing);
        // Paused or did-not-finish, then picked up again.
        assert_eq!(change_for(READING, Some(4)), Change::Move(READING));
        assert_eq!(change_for(READ, Some(5)), Change::Move(READ));
    }

    #[test]
    fn titles_match_despite_punctuation_and_subtitles() {
        assert!(same_title("Hollywood, Ending", "Hollywood Ending", None));
        assert!(same_title(
            "THE HOBBIT",
            "The Hobbit",
            Some("or There and Back Again")
        ));
        assert!(same_title(
            "The Hobbit: or There and Back Again",
            "The Hobbit",
            None
        ));
        assert!(same_title(
            "The Hobbit: or There and Back Again",
            "The Hobbit",
            Some("Or, There and Back Again")
        ));
        assert!(same_title("Träna din hjärna", "Träna Din Hjärna", None));
        assert!(!same_title("The Hobbit", "The Hobbit Companion", None));
        assert!(!same_title("Dune", "Dune Messiah", None));
        assert!(!same_title("Dune", "", None));
    }

    #[test]
    fn authors_match_by_surname() {
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(same_author(
            &names(&["John Green"]),
            &names(&["John Green"])
        ));
        assert!(same_author(
            &names(&["Green, John"]),
            &names(&["John  Green"])
        ));
        assert!(same_author(
            &names(&["J. R. R. Tolkien"]),
            &names(&["J.R.R. Tolkien"])
        ));
        assert!(same_author(
            &names(&["Ann", "Bob Smith"]),
            &names(&["Robert Smith"])
        ));
        assert!(!same_author(
            &names(&["John Green"]),
            &names(&["Hank Brown"])
        ));
        assert!(!same_author(&names(&["John Green"]), &[]));
        assert!(same_author(&[], &names(&["Anyone"])));
    }

    #[test]
    fn slugs_from_links() {
        for link in [
            "https://hardcover.app/books/hollywood-ending",
            " https://hardcover.app/books/hollywood-ending/editions?x=1 ",
            "hardcover.app/books/hollywood-ending#top",
            "hollywood-ending",
        ] {
            assert_eq!(
                slug_from_link(link).as_deref(),
                Some("hollywood-ending"),
                "{link}"
            );
        }
        for bad in [
            "",
            "https://evil.example/books/x",
            "https://hardcover.app/books/",
            "Hollywood Ending",
            "javascript:alert(1)",
            "x\"><script>",
        ] {
            assert_eq!(slug_from_link(bad), None, "{bad}");
        }
        assert_eq!(
            book_link("hollywood-ending").as_deref(),
            Some("https://hardcover.app/books/hollywood-ending")
        );
    }
}
