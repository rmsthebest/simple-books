//! Folkets lexikon's English–Swedish XML into a dictionary file.
//!
//! Each `<word>` is one sense of a word in one class: its `<translation>`s
//! (with a comment such as "informellt"), `<example>`s with their own
//! translations, an `<explanation>`, and for irregular words a `<paradigm>`
//! of `<inflection>`s ("ran" for "run"). All the senses of a word in one
//! class become one [`Entry`], in the order the file has them. Phrases ("à la") are left
//! out, since a finger only ever rests on one word. Values are escaped
//! twice in the file, so `&amp;#39;` is an apostrophe.

use std::{borrow::Cow, collections::HashMap, fmt::Write as _};

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};

use crate::{Builder, Entry, Sense};

#[derive(Debug)]
pub struct ConvertError(String);

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the dictionary XML: {}", self.0)
    }
}

impl std::error::Error for ConvertError {}

/// Where we are inside a `<word>`.
#[derive(Debug, Default)]
struct Word {
    headword: String,
    class: String,
    comment: String,
    translations: Vec<String>,
    explanation: String,
    example: Option<(String, String)>,
    forms: Vec<String>,
    /// Inside an `<example>` or `<idiom>`, whose translations aren't the
    /// word's.
    nested: Option<Nested>,
}

#[derive(Debug, PartialEq, Eq)]
enum Nested {
    Example,
    Other,
}

impl Word {
    /// An element inside the `<word>`.
    fn element(&mut self, name: &str, start: &BytesStart<'_>, empty: bool) {
        match name {
            "translation" => {
                let text = value(start, "value").unwrap_or_default();
                let comment = value(start, "comment").unwrap_or_default();
                match &self.nested {
                    None if !text.is_empty() => self.translations.push(if comment.is_empty() {
                        text
                    } else {
                        format!("{text} ({comment})")
                    }),
                    Some(Nested::Example) => {
                        if let Some((_, translated)) = self.example.as_mut()
                            && translated.is_empty()
                        {
                            *translated = text;
                        }
                    }
                    _ => {}
                }
            }
            "example" => {
                if self.example.is_none()
                    && let Some(text) = value(start, "value")
                {
                    self.example = Some((text, String::new()));
                    self.nested = Some(Nested::Example);
                } else {
                    self.nested = Some(Nested::Other);
                }
                if empty {
                    self.nested = None;
                }
            }
            "idiom" if !empty => self.nested = Some(Nested::Other),
            "explanation" if self.explanation.is_empty() => {
                self.explanation = value(start, "value").unwrap_or_default();
            }
            "inflection" => {
                if let Some(form) = value(start, "value")
                    && !form.contains(' ')
                {
                    self.forms.push(form);
                }
            }
            _ => {}
        }
    }
}

/// The dictionary file for `xml`.
pub fn convert(xml: &str) -> Result<String, ConvertError> {
    let mut reader = Reader::from_str(xml);
    let mut about = String::from("Folkets lexikon (KTH), English–Swedish");
    let mut entries = Entries::default();
    let mut word: Option<Word> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| ConvertError(error.to_string()))?;
        let (start, empty) = match &event {
            Event::Eof => break,
            Event::DocType(_) => return Err(ConvertError("a document type".into())),
            Event::Start(start) => (Some(start), false),
            Event::Empty(start) => (Some(start), true),
            Event::End(end) => {
                match end.local_name().as_ref() {
                    "word" => {
                        if let Some(done) = word.take() {
                            entries.add(done);
                        }
                    }
                    "example" | "idiom" => {
                        if let Some(word) = word.as_mut() {
                            word.nested = None;
                        }
                    }
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        let Some(start) = start else { continue };
        match (start.local_name().as_ref(), word.as_mut()) {
            ("dictionary", _) => {
                if let Some(version) = value(start, "version") {
                    let _ = write!(about, ", version {version}");
                }
                about.push_str("; CC BY-SA 2.5, https://creativecommons.org/licenses/by-sa/2.5/");
            }
            ("word", _) => {
                let new = Word {
                    headword: value(start, "value").unwrap_or_default(),
                    class: class(&value(start, "class").unwrap_or_default()).to_owned(),
                    comment: value(start, "comment").unwrap_or_default(),
                    ..Word::default()
                };
                if empty {
                    entries.add(new);
                } else {
                    word = Some(new);
                }
            }
            (name, Some(word)) => word.element(name, start, empty),
            _ => {}
        }
    }
    let mut builder = Builder::default();
    for (entry, forms) in entries.list {
        builder.add(entry, &forms);
    }
    Ok(builder.finish(&about))
}

/// The entries so far, each with its inflected forms, and where each word
/// and class is among them.
#[derive(Debug, Default)]
struct Entries {
    list: Vec<(Entry, Vec<String>)>,
    at: HashMap<(String, String), usize>,
}

impl Entries {
    /// Adds the sense in `word` to its word and class's entry.
    fn add(&mut self, word: Word) {
        if word.headword.is_empty() || word.headword.contains(' ') {
            return;
        }
        let mut unique: Vec<&str> = Vec::new();
        for translation in &word.translations {
            if !unique.contains(&translation.as_str()) {
                unique.push(translation);
            }
        }
        let mut translations = unique.join(", ");
        if !word.comment.is_empty() && !translations.is_empty() {
            translations = format!("({}) {translations}", word.comment);
        }
        let sense = Sense {
            translations,
            note: word.explanation,
            example: word.example.filter(|(example, _)| !example.is_empty()),
        };
        if sense.translations.is_empty() && sense.note.is_empty() && sense.example.is_none() {
            return;
        }
        let key = (word.headword.clone(), word.class.clone());
        if let Some(&at) = self.at.get(&key) {
            let (entry, forms) = &mut self.list[at];
            // The file repeats some senses word for word.
            if !entry.senses.contains(&sense) {
                entry.senses.push(sense);
            }
            for form in word.forms {
                if !forms.contains(&form) {
                    forms.push(form);
                }
            }
        } else {
            self.at.insert(key, self.list.len());
            self.list.push((
                Entry {
                    word: word.headword,
                    class: word.class,
                    senses: vec![sense],
                },
                word.forms,
            ));
        }
    }
}

/// An attribute's value, unescaped twice.
fn value(element: &BytesStart<'_>, name: &str) -> Option<String> {
    let attribute = element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name)?;
    let once = attribute
        .normalized_value(XmlVersion::Implicit1_0)
        .unwrap_or_else(|_| attribute.value.clone());
    Some(unescape(&once).trim().to_owned())
}

/// The second round of escapes in the file's values.
fn unescape(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.replace("&#39;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// The word class, in plain English.
fn class(tag: &str) -> &str {
    match tag {
        "nn" => "noun",
        "vb" => "verb",
        "jj" => "adjective",
        "ab" => "adverb",
        "pp" => "preposition",
        "pn" => "pronoun",
        "abbrev" => "abbreviation",
        "kn" => "conjunction",
        "pc" => "participle",
        "in" => "interjection",
        "rg" => "number",
        "ro" => "ordinal",
        "pm" => "name",
        "ie" => "infinitive marker",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dictionary;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<dictionary name="en-sv Lexin" version="1.0.4488" license="http://creativecommons.org/licenses/by-sa/2.5/">
<word value="à la" lang="en" class="pp"><translation value="likt" />
</word>
<word value="run" lang="en" class="vb"><translation value="gå" comment="i trafiken" />
<translation value="köra" />
<paradigm><inflection value="ran" />
<inflection value="run" />
</paradigm>
<example value="the buses don&amp;#39;t run on Sundays"><translation value="bussarna går inte på söndagar" />
</example>
<grammar value="intransitivt" />
</word>
<word value="run" lang="en" class="vb"><translation value="" comment="ingen exakt översättning" />
<example value="my stocking has run"><translation value="det har gått en maska på min strumpa" />
</example>
<idiom value="run late"><translation value="vara sen" />
</idiom>
</word>
<word value="run" lang="en" class="nn"><translation value="löprunda" />
<explanation value="en tur där man springer" />
</word>
<word value="run" lang="en" class="vb"><translation value="kandidera" comment="inom politik" />
</word>
<word value="ability" lang="en" class="nn" comment="utbildning"><translation value="förmåga" /><variant value="talent" />
</word>
<word value="empty" lang="en" class="nn"></word>
</dictionary>
"#;

    #[test]
    fn senses_of_a_word_and_class_become_one_entry() {
        let dictionary = Dictionary::parse(convert(SAMPLE).unwrap()).unwrap();
        assert_eq!(
            dictionary.about(),
            "Folkets lexikon (KTH), English–Swedish, version 1.0.4488; CC BY-SA 2.5, \
             https://creativecommons.org/licenses/by-sa/2.5/"
        );
        let run = dictionary.under("run");
        assert_eq!(run.len(), 2, "the verb and the noun");
        let verb = &run[0];
        assert_eq!(verb.class, "verb");
        // The verb's third sense comes after the noun in the file.
        assert_eq!(verb.senses.len(), 3);
        assert_eq!(verb.senses[2].translations, "kandidera (inom politik)");
        assert_eq!(verb.senses[0].translations, "gå (i trafiken), köra");
        assert_eq!(
            verb.senses[0].example,
            Some((
                "the buses don't run on Sundays".into(),
                "bussarna går inte på söndagar".into()
            ))
        );
        // No word for it: the example says it, and the idiom's
        // translation isn't the word's.
        assert_eq!(verb.senses[1].translations, "");
        assert_eq!(
            verb.senses[1].example.as_ref().map(|(e, _)| e.as_str()),
            Some("my stocking has run")
        );
        assert_eq!(run[1].class, "noun");
        assert_eq!(run[1].senses[0].note, "en tur där man springer");
        // The past tense finds the verb only.
        assert_eq!(dictionary.under("ran"), run[..1]);
        assert_eq!(
            dictionary.under("ability")[0].senses[0].translations,
            "(utbildning) förmåga"
        );
        assert!(dictionary.under("à la").is_empty(), "phrases are left out");
        assert!(
            dictionary.under("talent").is_empty(),
            "variants aren't keys"
        );
        assert!(dictionary.under("empty").is_empty(), "nothing to say");
    }
}
