//! Soft hyphens, where the book's language allows a word to break. The reader
//! draws a hyphen only at the ones it breaks a line at.

use book_format::SOFT_HYPHEN;
use hypher::Lang;

/// Words longer than this are left alone: no real word is, and the patterns
/// cost more the longer the word.
const MAX_WORD_CHARS: usize = 64;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Hyphenator(Lang);

impl Hyphenator {
    /// For a BCP 47 tag such as `sv` or `en-GB`. `None` for a language with no
    /// patterns, in which case a book keeps whatever soft hyphens it has.
    pub(crate) fn for_language(tag: &str) -> Option<Self> {
        let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
        let code: [u8; 2] = primary.as_bytes().try_into().ok()?;
        Lang::from_iso(code).map(Self)
    }

    /// Replaces any soft hyphens with the patterns' own. Web and email
    /// addresses are left whole.
    pub(crate) fn apply(self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + text.len() / 8);
        for (index, token) in text.split(' ').enumerate() {
            if index > 0 {
                out.push(' ');
            }
            if is_address(token) {
                out.push_str(token);
            } else {
                self.push_token(&mut out, token);
            }
        }
        out
    }

    fn push_token(self, out: &mut String, token: &str) {
        let mut word = String::new();
        for c in token.chars().filter(|c| *c != SOFT_HYPHEN) {
            if c.is_alphabetic() {
                word.push(c);
            } else {
                self.push_word(out, &word);
                word.clear();
                out.push(c);
            }
        }
        self.push_word(out, &word);
    }

    fn push_word(self, out: &mut String, word: &str) {
        if word.chars().count() > MAX_WORD_CHARS {
            out.push_str(word);
            return;
        }
        for (index, syllable) in hypher::hyphenate(word, self.0).enumerate() {
            if index > 0 {
                out.push(SOFT_HYPHEN);
            }
            out.push_str(syllable);
        }
    }
}

/// Something like `example.com`, `https://…` or `a@b`: a dot between letters
/// doesn't happen in prose, where a space follows a full stop.
fn is_address(token: &str) -> bool {
    if token.contains("://") || token.contains('@') || token.starts_with("www.") {
        return true;
    }
    let chars: Vec<char> = token.chars().collect();
    chars
        .windows(3)
        .any(|w| w[0].is_alphanumeric() && w[1] == '.' && w[2].is_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str) -> String {
        text.replace(SOFT_HYPHEN, "-")
    }

    #[test]
    fn hyphenates_by_language() {
        let english = Hyphenator::for_language("en-US").unwrap();
        assert_eq!(shown(&english.apply("Extensive, it")), "Ex-ten-sive, it");
        let swedish = Hyphenator::for_language("sv").unwrap();
        assert_eq!(
            shown(&swedish.apply("sjukhuset")),
            "sjuk-hu-set",
            "Swedish patterns, not English ones"
        );
        assert!(Hyphenator::for_language("xx").is_none());
        assert!(Hyphenator::for_language("").is_none());
        assert!(Hyphenator::for_language("eng").is_none());
    }

    #[test]
    fn replaces_the_books_own_soft_hyphens() {
        let english = Hyphenator::for_language("en").unwrap();
        assert_eq!(
            shown(&english.apply("ex\u{AD}tens\u{AD}ive")),
            "ex-ten-sive"
        );
    }

    #[test]
    fn leaves_addresses_whole() {
        let english = Hyphenator::for_language("en").unwrap();
        let text = "penguinrandomhouse.com https://eu-contact.penguin.ie publisher@example.org www.weylerforlag.se";
        assert_eq!(english.apply(text), text);
        assert_eq!(
            shown(&english.apply("extensive. Extensive")),
            "ex-ten-sive. Ex-ten-sive"
        );
    }

    #[test]
    fn leaves_everything_else_as_it_was() {
        let english = Hyphenator::for_language("en").unwrap();
        let text = "a  b\n1234 — “q” don't";
        assert_eq!(english.apply(text), text);
        let long = "x".repeat(MAX_WORD_CHARS + 1);
        assert_eq!(english.apply(&long), long);
    }
}
