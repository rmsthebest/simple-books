//! Tidies text taken from a file. It's displayed escaped everywhere, so this is
//! about reading well and staying a sensible size, not about being the only
//! defence against markup.

use quick_xml::escape::resolve_predefined_entity;

/// Collapses whitespace, drops control characters, and caps the length.
pub(crate) fn line(input: &str, max_chars: usize) -> Option<String> {
    let mut out = String::with_capacity(input.len().min(max_chars));
    let mut count = 0;
    for word in input.split_whitespace() {
        let word = word.chars().filter(|c| !c.is_control());
        for c in (!out.is_empty()).then_some(' ').into_iter().chain(word) {
            if count == max_chars {
                return finish(&out);
            }
            out.push(c);
            count += 1;
        }
    }
    finish(&out)
}

fn finish(out: &str) -> Option<String> {
    let trimmed = out.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Reduces a description, which is often HTML, to plain text. Tags become spaces
/// and entities are decoded before tags are removed, so a tag hidden in an entity
/// goes too. No `<` or `>` survives.
pub(crate) fn plain(input: &str, max_chars: usize) -> Option<String> {
    let decoded = decode_entities(&decode_entities(input));
    let mut out = String::with_capacity(decoded.len());
    let mut in_tag = false;
    for c in decoded.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    line(&out, max_chars)
}

fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(end) = after.find(';').filter(|end| *end <= 12) {
            match decode_entity(&after[..end]) {
                Some(decoded) => out.push_str(&decoded),
                None => out.push_str(&rest[at..=at + 1 + end]),
            }
            rest = &after[end + 1..];
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(name: &str) -> Option<String> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).map(String::from);
    }
    if name == "nbsp" {
        return Some(" ".to_owned());
    }
    resolve_predefined_entity(name).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn lines_are_tidied() {
        assert_eq!(line("  a \n\t b  ", 10).as_deref(), Some("a b"));
        assert_eq!(line("a\u{7}b", 10).as_deref(), Some("ab"));
        assert_eq!(line("   ", 10), None);
        assert_eq!(line("abcdef", 3).as_deref(), Some("abc"));
        assert_eq!(line("ab cd", 3).as_deref(), Some("ab"), "no trailing space");
    }

    #[test]
    fn descriptions_lose_their_markup() {
        assert_eq!(
            plain("<p>One &amp; <b>two</b></p><p>three</p>", 100).as_deref(),
            Some("One & two three")
        );
    }

    #[test]
    fn markup_hidden_in_entities_goes_too() {
        for input in [
            "&lt;script&gt;alert(1)&lt;/script&gt;x",
            "&amp;lt;script&amp;gt;alert(1)&amp;lt;/script&amp;gt;x",
            "&#60;script&#62;alert(1)&#60;/script&#62;x",
        ] {
            assert_eq!(plain(input, 100).as_deref(), Some("alert(1) x"), "{input}");
        }
    }

    #[test]
    fn an_ampersand_is_just_an_ampersand() {
        assert_eq!(
            plain("Salt & pepper", 100).as_deref(),
            Some("Salt & pepper")
        );
        assert_eq!(plain("AT&T; rocks", 100).as_deref(), Some("AT&T; rocks"));
    }

    proptest! {
        #[test]
        fn plain_never_leaves_angle_brackets_or_exceeds_the_cap(input in ".{0,300}") {
            if let Some(out) = plain(&input, 50) {
                prop_assert!(!out.contains('<') && !out.contains('>'), "{out}");
                prop_assert!(out.chars().count() <= 50);
            }
        }
    }
}
