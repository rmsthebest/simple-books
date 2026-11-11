//! The reader's English–Swedish dictionary: what's shown when a finger
//! rests on a word.
//!
//! - **The file** (`dict-en-sv` on the Kobo) is UTF-8 text, small enough to
//!   read whole. Its first line is [`MAGIC`], a tab, and where the words
//!   come from and their licence. Then come the entries, one per line,
//!   numbered from 0 in order: `E`, the word, its class ("verb") and its
//!   senses. Last come the keys, one per line and sorted: `K`, a word as
//!   it's looked up (lower case, a headword or an inflected form, such as
//!   "ran"), and the numbers of its entries. Fields are split by tabs,
//!   senses by U+001E, and a sense's parts by U+001F.
//! - **Looking up** ([`Dictionary::lookup`]) goes from a word as printed,
//!   "Running," say, to its entries: first the word itself, then the first
//!   base form a few English suffix rules reach, "run" ([`candidates`]).
//! - **Making the file:** [`Builder`], which the converter from Folkets
//!   lexikon's XML (`convert`, with the `convert` feature) and tests use.
#![forbid(unsafe_code)]

#[cfg(feature = "convert")]
pub mod convert;
mod words;

use std::{collections::BTreeMap, fmt, fmt::Write as _, ops::Range};

pub use words::{candidates, normalize};

/// The start of every dictionary file, and its version.
pub const MAGIC: &str = "simple-books dictionary 1";
const FIELD: char = '\t';
const SENSE: char = '\u{1e}';
const PART: char = '\u{1f}';

/// A word and what it means, in one word class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub word: String,
    /// Such as "verb", or empty.
    pub class: String,
    pub senses: Vec<Sense>,
}

/// One meaning: its translations, a note, and an example.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sense {
    /// Such as "gå (i trafiken), köra". Empty when there's no word for it,
    /// and only the example says what it means.
    pub translations: String,
    /// An explanation, in Swedish, or empty.
    pub note: String,
    /// An English example and its Swedish translation.
    pub example: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The first line isn't [`MAGIC`].
    NotADictionary,
    /// This line (counting from 1) isn't an entry or a key, or the keys
    /// aren't in order.
    Corrupt(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotADictionary => write!(f, "not a simple-books dictionary"),
            Error::Corrupt(line) => write!(f, "the dictionary is damaged at line {line}"),
        }
    }
}

impl std::error::Error for Error {}

/// A dictionary file, read whole, with where each entry and key is.
pub struct Dictionary {
    text: String,
    about: Range<usize>,
    /// Each entry's line, after `E\t`.
    entries: Vec<Range<usize>>,
    /// Each key's line, after `K\t`: the key, then its entry numbers.
    keys: Vec<(Range<usize>, Range<usize>)>,
}

impl fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dictionary")
            .field("about", &self.about())
            .field("entries", &self.entries.len())
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

impl Dictionary {
    pub fn parse(text: String) -> Result<Self, Error> {
        let mut about = 0..0;
        let mut entries = Vec::new();
        let mut keys: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        let mut start = 0;
        for (number, line) in text.split('\n').enumerate() {
            let range = start..start + line.len();
            start = range.end + 1;
            if number == 0 {
                let rest = line.strip_prefix(MAGIC).ok_or(Error::NotADictionary)?;
                if !(rest.is_empty() || rest.starts_with(FIELD)) {
                    return Err(Error::NotADictionary);
                }
                about = range.start + MAGIC.len() + usize::from(!rest.is_empty())..range.end;
            } else if line.starts_with("E\t") {
                entries.push(range.start + 2..range.end);
            } else if let Some(key_line) = line.strip_prefix("K\t") {
                let tab = key_line.find(FIELD).ok_or(Error::Corrupt(number + 1))?;
                let key = range.start + 2..range.start + 2 + tab;
                let ids = key.end + 1..range.end;
                if keys
                    .last()
                    .is_some_and(|(last, _)| text[last.clone()] >= text[key.clone()])
                {
                    return Err(Error::Corrupt(number + 1));
                }
                keys.push((key, ids));
            } else if !line.is_empty() {
                return Err(Error::Corrupt(number + 1));
            }
        }
        Ok(Self {
            text,
            about,
            entries,
            keys,
        })
    }

    /// Where the words come from, and their licence.
    pub fn about(&self) -> &str {
        &self.text[self.about.clone()]
    }

    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries for `word` as printed in a book: the word itself first,
    /// then those of the first base form found for it, if different.
    pub fn lookup(&self, word: &str) -> Vec<Entry> {
        let mut forms = candidates(word).into_iter();
        let Some(first) = forms.next() else {
            return Vec::new();
        };
        let mut found = self.under(&first);
        if let Some(base) = forms.map(|form| self.under(&form)).find(|e| !e.is_empty()) {
            for entry in base {
                if !found.contains(&entry) {
                    found.push(entry);
                }
            }
        }
        found
    }

    /// The entries under one key, exactly.
    pub fn under(&self, key: &str) -> Vec<Entry> {
        let Ok(at) = self
            .keys
            .binary_search_by(|(range, _)| self.text[range.clone()].cmp(key))
        else {
            return Vec::new();
        };
        self.text[self.keys[at].1.clone()]
            .split(',')
            .filter_map(|id| id.parse::<usize>().ok())
            .filter_map(|id| self.entry(id))
            .collect()
    }

    fn entry(&self, id: usize) -> Option<Entry> {
        let line = &self.text[self.entries.get(id)?.clone()];
        let mut fields = line.splitn(3, FIELD);
        let word = fields.next()?.to_owned();
        let class = fields.next().unwrap_or_default().to_owned();
        let senses = fields
            .next()
            .unwrap_or_default()
            .split(SENSE)
            .filter(|sense| !sense.is_empty())
            .map(|sense| {
                let mut parts = sense.split(PART);
                let mut part = || parts.next().unwrap_or_default().to_owned();
                let (translations, note, example, translated) = (part(), part(), part(), part());
                Sense {
                    translations,
                    note,
                    example: (!example.is_empty()).then_some((example, translated)),
                }
            })
            .collect();
        Some(Entry {
            word,
            class,
            senses,
        })
    }
}

/// Puts a dictionary file together.
#[derive(Debug, Default)]
pub struct Builder {
    entries: Vec<Entry>,
    keys: BTreeMap<String, Vec<usize>>,
}

impl Builder {
    /// Adds `entry`, found under its word and each of `forms` (such as
    /// "ran" for "run"), in lower case.
    pub fn add(&mut self, entry: Entry, forms: &[String]) {
        let id = self.entries.len();
        for key in std::iter::once(&entry.word).chain(forms) {
            let key = clean(&key.to_lowercase());
            if key.is_empty() {
                continue;
            }
            let ids = self.keys.entry(key).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        self.entries.push(entry);
    }

    /// The file, with `about` saying where the words come from.
    pub fn finish(self, about: &str) -> String {
        let mut out = format!("{MAGIC}{FIELD}{}\n", clean(about));
        for entry in &self.entries {
            let senses: Vec<String> = entry
                .senses
                .iter()
                .map(|sense| {
                    let (example, translated) = sense
                        .example
                        .as_ref()
                        .map_or(("", ""), |(e, t)| (e.as_str(), t.as_str()));
                    [&sense.translations, &sense.note, example, translated]
                        .map(clean)
                        .join(&PART.to_string())
                })
                .collect();
            let _ = writeln!(
                out,
                "E{FIELD}{}{FIELD}{}{FIELD}{}",
                clean(&entry.word),
                clean(&entry.class),
                senses.join(&SENSE.to_string())
            );
        }
        for (key, ids) in &self.keys {
            let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
            let _ = writeln!(out, "K{FIELD}{key}{FIELD}{}", ids.join(","));
        }
        out
    }
}

/// Without the characters the file splits on.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' | SENSE | PART => ' ',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sense(translations: &str) -> Sense {
        Sense {
            translations: translations.to_owned(),
            ..Sense::default()
        }
    }

    fn sample() -> Dictionary {
        let mut builder = Builder::default();
        builder.add(
            Entry {
                word: "run".into(),
                class: "verb".into(),
                senses: vec![
                    Sense {
                        translations: "springa, löpa".into(),
                        note: String::new(),
                        example: Some(("run for the bus".into(), "springa till bussen".into())),
                    },
                    sense("gå (i trafiken)"),
                ],
            },
            &["ran".into(), "run".into()],
        );
        builder.add(
            Entry {
                word: "running".into(),
                class: "noun".into(),
                senses: vec![sense("löpning")],
            },
            &[],
        );
        builder.add(
            Entry {
                word: "happy".into(),
                class: "adjective".into(),
                senses: vec![sense("glad, lycklig")],
            },
            &[],
        );
        Dictionary::parse(builder.finish("A test\tdictionary")).unwrap()
    }

    #[test]
    fn a_built_file_reads_back() {
        let dictionary = sample();
        assert_eq!(dictionary.about(), "A test dictionary");
        assert_eq!(dictionary.len(), 3);
        let run = dictionary.under("run");
        assert_eq!(run.len(), 1);
        assert_eq!(run[0].class, "verb");
        assert_eq!(run[0].senses[0].translations, "springa, löpa");
        assert_eq!(
            run[0].senses[0].example,
            Some(("run for the bus".into(), "springa till bussen".into()))
        );
        assert_eq!(run[0].senses[1].example, None);
        assert_eq!(dictionary.under("ran"), run);
        assert_eq!(dictionary.under("walk"), Vec::<Entry>::new());
    }

    #[test]
    fn lookup_finds_printed_words_and_their_base_forms() {
        let dictionary = sample();
        let words = |word: &str| -> Vec<String> {
            dictionary
                .lookup(word)
                .into_iter()
                .map(|e| e.word)
                .collect()
        };
        assert_eq!(words("Run,"), ["run"]);
        assert_eq!(words("“ran”"), ["run"]);
        assert_eq!(words("runs"), ["run"]);
        // The word itself first, then its base form.
        assert_eq!(words("running"), ["running", "run"]);
        assert_eq!(words("happier"), ["happy"]);
        assert_eq!(words("hap\u{ad}pily"), ["happy"]);
        assert_eq!(words("walked"), Vec::<String>::new());
        assert_eq!(words("…"), Vec::<String>::new());
    }

    #[test]
    fn a_damaged_file_is_refused() {
        assert_eq!(
            Dictionary::parse("hello\n".into()).unwrap_err(),
            Error::NotADictionary
        );
        let unsorted = format!("{MAGIC}\nE\ta\t\t\nK\tb\t0\nK\ta\t0\n");
        assert_eq!(Dictionary::parse(unsorted).unwrap_err(), Error::Corrupt(4));
        let odd = format!("{MAGIC}\nX\tsomething\n");
        assert_eq!(Dictionary::parse(odd).unwrap_err(), Error::Corrupt(2));
        // A key pointing nowhere finds nothing, rather than failing.
        let dangling = format!("{MAGIC}\nK\ta\t7\n");
        assert_eq!(
            Dictionary::parse(dangling).unwrap().under("a"),
            Vec::<Entry>::new()
        );
    }
}
