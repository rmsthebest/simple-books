//! ISBNs, as books write them in `<dc:identifier>`: bare, hyphenated, spaced, as
//! `urn:isbn:…`, as ISBN-10 or ISBN-13. Only one with a valid check digit counts,
//! so a book's other identifiers (UUIDs, URLs, shop codes) are never mistaken for one.

/// The ISBN-13 for `raw`, if it is an ISBN. ISBN-10s are converted.
#[must_use]
pub fn normalize(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    for prefix in ["urn:isbn:", "isbn:", "isbn"] {
        if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
            text = text[prefix.len()..].trim_start();
            break;
        }
    }
    if text.len() > 20 {
        return None;
    }
    let mut chars: Vec<char> = Vec::with_capacity(13);
    for c in text.chars() {
        match c {
            '0'..='9' => chars.push(c),
            'x' | 'X' if chars.len() == 9 => chars.push('X'),
            '-' | ' ' => {}
            _ => return None,
        }
    }
    match chars.len() {
        13 => is_isbn13(&chars).then(|| chars.into_iter().collect()),
        10 => is_isbn10(&chars).then(|| isbn10_to_13(&chars)),
        _ => None,
    }
}

fn digit(c: char) -> u32 {
    c.to_digit(10).unwrap_or(10)
}

fn is_isbn13(chars: &[char]) -> bool {
    if !(chars.starts_with(&['9', '7', '8']) || chars.starts_with(&['9', '7', '9'])) {
        return false;
    }
    if chars.iter().any(|c| !c.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = chars
        .iter()
        .enumerate()
        .map(|(at, &c)| digit(c) * if at % 2 == 0 { 1 } else { 3 })
        .sum();
    sum.is_multiple_of(10)
}

fn is_isbn10(chars: &[char]) -> bool {
    let sum: u32 = chars
        .iter()
        .enumerate()
        .map(|(at, &c)| digit(c) * (10 - u32::try_from(at).unwrap_or(0)))
        .sum();
    sum.is_multiple_of(11)
}

fn isbn10_to_13(chars: &[char]) -> String {
    let mut out: Vec<char> = "978".chars().chain(chars[..9].iter().copied()).collect();
    let sum: u32 = out
        .iter()
        .enumerate()
        .map(|(at, &c)| digit(c) * if at % 2 == 0 { 1 } else { 3 })
        .sum();
    let check = (10 - sum % 10) % 10;
    out.push(char::from_digit(check, 10).unwrap_or('0'));
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn reads_the_ways_books_write_them() {
        for raw in [
            "9781101592434",
            " 978-1-101-59243-4 ",
            "urn:isbn:9781101592434",
            "URN:ISBN:978 1 101 59243 4",
            "ISBN 978-1-101-59243-4",
            "isbn:9781101592434",
        ] {
            assert_eq!(normalize(raw).as_deref(), Some("9781101592434"), "{raw}");
        }
    }

    #[test]
    fn converts_isbn_10() {
        assert_eq!(normalize("0-345-33968-1").as_deref(), Some("9780345339683"));
        // An X check digit.
        assert_eq!(normalize("080442957X").as_deref(), Some("9780804429573"));
    }

    #[test]
    fn other_identifiers_are_not_isbns() {
        for raw in [
            "",
            "9781101592435",                        // wrong check digit
            "1234567890123",                        // not a 978/979 prefix
            "4be28826-6838-4ff7-ba73-cc553a0dba81", // a UUID
            "http://litteraturbanken.se/forfattare/JanssonM/titlar/FolkeDahlberg/epub",
            "0-345-33968-2",  // wrong ISBN-10 check digit
            "97811015924341", // too long
            "X345339681",     // X only at the end
            "978110159243\u{0663}",
        ] {
            assert_eq!(normalize(raw), None, "{raw}");
        }
    }
}
