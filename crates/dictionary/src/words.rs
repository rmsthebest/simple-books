//! From a word as printed to the forms to look up.

/// The word as it's looked up: without soft hyphens or the punctuation and
/// quotes around it, with straight apostrophes, in lower case.
pub fn normalize(word: &str) -> String {
    let word: String = word
        .chars()
        .filter(|c| !matches!(c, '\u{ad}' | '\u{200b}' | '\u{2060}'))
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{2010}' | '\u{2011}' => '-',
            c => c,
        })
        .collect();
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The forms to try for `word`, best first: the word itself, without a
/// possessive 's, then what a few English suffix rules make of it, such as
/// "run" for "running" and "happy" for "happier". Some won't be words; the
/// dictionary decides.
pub fn candidates(word: &str) -> Vec<String> {
    let word = normalize(word);
    let mut out: Vec<String> = Vec::new();
    if word.is_empty() {
        return out;
    }
    let mut push = |form: String| {
        if form.chars().count() >= 2 && !out.contains(&form) {
            out.push(form);
        }
    };
    push(word.clone());
    let mut bases = vec![word.clone()];
    if let Some(base) = word.strip_suffix("'s").or_else(|| word.strip_suffix('\'')) {
        push(base.to_owned());
        bases.push(base.to_owned());
    }
    for base in bases {
        for stem in stems(&base) {
            push(stem);
        }
    }
    // Only a word itself may be short, such as "TV" or "go".
    if word.chars().count() < 2 {
        out.insert(0, word);
    }
    out
}

/// Suffixes and what they may have replaced, longest first.
const RULES: &[(&str, &[&str])] = &[
    ("iest", &["y"]),
    ("ies", &["y"]),
    ("ied", &["y"]),
    ("ier", &["y"]),
    ("ily", &["y"]),
    ("ves", &["f", "fe"]),
    ("est", &["", "e"]),
    ("ing", &["", "e"]),
    ("es", &[""]),
    ("ed", &["", "e"]),
    ("er", &["", "e"]),
    ("ly", &[""]),
    ("s", &[""]),
];

fn stems(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (suffix, replacements) in RULES {
        let Some(stem) = word.strip_suffix(suffix) else {
            continue;
        };
        // "class" isn't "clas" + s.
        if *suffix == "s" && stem.ends_with('s') {
            continue;
        }
        if stem.chars().count() < 2 {
            continue;
        }
        let last = stem.chars().last().unwrap_or_default();
        for replacement in *replacements {
            // "hoped" is "hope", but "seed" isn't "see".
            if *replacement == "e" && !is_consonant(last) {
                continue;
            }
            out.push(format!("{stem}{replacement}"));
        }
        // "stopped", "running", "bigger": one of a doubled consonant.
        let mut chars = stem.chars().rev();
        if let (Some(a), Some(b)) = (chars.next(), chars.next())
            && a == b
            && is_consonant(a)
            && matches!(*suffix, "ed" | "ing" | "er" | "est")
        {
            out.push(stem[..stem.len() - a.len_utf8()].to_owned());
        }
    }
    out
}

fn is_consonant(c: char) -> bool {
    c.is_ascii_alphabetic() && !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_words_are_cleaned_up() {
        assert_eq!(normalize("“Hello,”"), "hello");
        assert_eq!(normalize("can’t"), "can't");
        assert_eq!(normalize("ex\u{ad}am\u{ad}ple."), "example");
        assert_eq!(normalize("well‐known"), "well-known");
        assert_eq!(normalize("(1984)"), "1984");
        assert_eq!(normalize("—"), "");
    }

    #[test]
    fn suffix_rules_reach_base_forms() {
        let has = |word: &str, form: &str| candidates(word).contains(&form.to_owned());
        assert!(has("running", "run"));
        assert!(has("stopped", "stop"));
        assert!(has("hoped", "hope"));
        assert!(has("walked", "walk"));
        assert!(has("cities", "city"));
        assert!(has("carried", "carry"));
        assert!(has("happier", "happy"));
        assert!(has("happiest", "happy"));
        assert!(has("happily", "happy"));
        assert!(has("wolves", "wolf"));
        assert!(has("knives", "knife"));
        assert!(has("boxes", "box"));
        assert!(has("bigger", "big"));
        assert!(has("quickly", "quick"));
        assert!(has("dog's", "dog"));
        assert!(has("dogs'", "dog"));
        assert!(!has("class", "clas"));
        assert!(!has("seed", "see"));
        assert!(!has("being", "bee"));
        assert_eq!(candidates("running")[0], "running", "the word itself first");
        assert_eq!(candidates(""), Vec::<String>::new());
    }
}
