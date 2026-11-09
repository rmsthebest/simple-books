//! Points a Kobo's `Kobo eReader.conf` at a sync server.
//!
//! This runs in the browser, on a file that also holds the device's Wi-Fi passwords
//! and its store token, and the file never leaves the browser. It changes one line
//! and leaves every other byte alone. One line is enough: the Kobo fills in the rest
//! of the section from what the server sends.
#![forbid(unsafe_code)]

const SECTION: &[u8] = b"[OneStoreServices]";
const KEY: &[u8] = b"api_endpoint";
/// Sections a Kobo always writes, to tell its config from some other file. Which one
/// is present varies, as a device that never signed in has no `OneStoreServices`.
const KNOWN_SECTIONS: [&[u8]; 3] = [SECTION, b"[ApplicationPreferences]", b"[PowerOptions]"];
/// A real one is a few KiB. Anything like this size is the wrong file.
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// The file isn't a Kobo config.
    NotAKoboConfig,
    /// The address isn't one that can go in the file.
    BadEndpoint,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAKoboConfig => "This doesn't look like a Kobo eReader.conf file.",
            Self::BadEndpoint => "This server's address can't be used.",
        })
    }
}

impl std::error::Error for PatchError {}

/// One line of the file, with its line ending.
fn lines(conf: &[u8]) -> impl Iterator<Item = &[u8]> {
    conf.split_inclusive(|&byte| byte == b'\n')
}

fn trimmed(line: &[u8]) -> &[u8] {
    line.trim_ascii()
}

fn is_section(line: &[u8]) -> bool {
    let line = trimmed(line);
    line.first() == Some(&b'[') && line.last() == Some(&b']')
}

/// The line's value, if it sets `api_endpoint`.
fn endpoint_value(line: &[u8]) -> Option<&[u8]> {
    let at = line.iter().position(|&byte| byte == b'=')?;
    let (key, value) = line.split_at(at);
    (trimmed(key) == KEY).then(|| trimmed(&value[1..]))
}

fn check_is_config(conf: &[u8]) -> Result<(), PatchError> {
    let known = lines(conf).any(|line| KNOWN_SECTIONS.contains(&trimmed(line)));
    if conf.len() > MAX_BYTES || conf.contains(&0) || !known {
        return Err(PatchError::NotAKoboConfig);
    }
    Ok(())
}

fn check_endpoint(endpoint: &str) -> Result<(), PatchError> {
    let scheme = endpoint.starts_with("https://") || endpoint.starts_with("http://");
    let clean = endpoint.chars().all(|c| !c.is_control() && c != ' ');
    if scheme && clean {
        Ok(())
    } else {
        Err(PatchError::BadEndpoint)
    }
}

/// Whether these bytes look like a Kobo config. An empty or damaged backup doesn't.
#[must_use]
pub fn looks_like_config(conf: &[u8]) -> bool {
    check_is_config(conf).is_ok()
}

/// The address the file points at now, if any.
pub fn current_endpoint(conf: &[u8]) -> Option<String> {
    let mut inside = false;
    let mut found = None;
    for line in lines(conf) {
        if is_section(line) {
            inside = trimmed(line) == SECTION;
        } else if inside && let Some(value) = endpoint_value(line) {
            // Qt keeps the last of a repeated key.
            found = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    found
}

/// Returns the file with `api_endpoint` set. Running it again changes nothing.
pub fn patch(conf: &[u8], endpoint: &str) -> Result<Vec<u8>, PatchError> {
    check_is_config(conf)?;
    check_endpoint(endpoint)?;

    // Follow the file's own habit, so that it stays consistent.
    let newline: &[u8] = if conf.windows(2).any(|pair| pair == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let setting = [KEY, b"=", endpoint.as_bytes()].concat();

    let mut out = Vec::with_capacity(conf.len() + setting.len() + SECTION.len() + 8);
    let mut in_section = false;
    let mut section_seen = false;
    let mut replaced = false;

    for line in lines(conf) {
        if is_section(line) {
            in_section = trimmed(line) == SECTION;
            section_seen |= in_section;
        } else if in_section && endpoint_value(line).is_some() {
            out.extend_from_slice(&setting);
            // What follows the value on this line: its ending, or nothing at the
            // very end of a file.
            let body = line.trim_ascii_end().len();
            out.extend_from_slice(&line[body..]);
            replaced = true;
            continue;
        }
        out.extend_from_slice(line);
    }
    if replaced {
        return Ok(out);
    }

    if section_seen {
        return Ok(insert_after_section(conf, &setting, newline));
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.extend_from_slice(newline);
    }
    if !out.is_empty() {
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(SECTION);
    out.extend_from_slice(newline);
    out.extend_from_slice(&setting);
    out.extend_from_slice(newline);
    Ok(out)
}

fn insert_after_section(conf: &[u8], setting: &[u8], newline: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(conf.len() + setting.len() + 2);
    let mut done = false;
    for line in lines(conf) {
        out.extend_from_slice(line);
        if !done && trimmed(line) == SECTION {
            if !line.ends_with(b"\n") {
                out.extend_from_slice(newline);
            }
            out.extend_from_slice(setting);
            out.extend_from_slice(newline);
            done = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NEW: &str = "https://books.example.com/kobo/abc";
    const CONF: &str = "\
[ApplicationPreferences]
AutoColorEnabled=false
WifiPassword=keep me exactly = as is

[OneStoreServices]
activate_url=https://old.example.net/activate
api_endpoint=https://old.example.net/kobo/old
image_host=https://old.example.net/

[PowerOptions]
api_endpoint=https://not-this-one.example.net
AutoOffMinutes=60
";

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn changes_only_that_line() {
        let out = patch(CONF.as_bytes(), NEW).unwrap();
        let expected = CONF.replace(
            "api_endpoint=https://old.example.net/kobo/old",
            &format!("api_endpoint={NEW}"),
        );
        assert_eq!(text(&out), expected);
    }

    #[test]
    fn the_same_key_elsewhere_is_left_alone() {
        let out = text(&patch(CONF.as_bytes(), NEW).unwrap());
        assert!(out.contains("api_endpoint=https://not-this-one.example.net"));
    }

    #[test]
    fn running_it_twice_is_the_same_as_once() {
        let once = patch(CONF.as_bytes(), NEW).unwrap();
        assert_eq!(patch(&once, NEW).unwrap(), once);
    }

    #[test]
    fn keeps_windows_line_endings() {
        let conf = CONF.replace('\n', "\r\n");
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        let expected = conf.replace(
            "api_endpoint=https://old.example.net/kobo/old",
            &format!("api_endpoint={NEW}"),
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn adds_the_line_when_the_section_lacks_it() {
        let conf = "[OneStoreServices]\nimage_host=x\n\n[PowerOptions]\nA=1\n";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(
            out,
            format!(
                "[OneStoreServices]\napi_endpoint={NEW}\nimage_host=x\n\n[PowerOptions]\nA=1\n"
            )
        );
    }

    #[test]
    fn adds_the_section_when_the_file_lacks_it() {
        let conf = "[PowerOptions]\nA=1\n";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(
            out,
            format!("[PowerOptions]\nA=1\n\n[OneStoreServices]\napi_endpoint={NEW}\n")
        );
    }

    #[test]
    fn copes_with_no_newline_at_the_very_end() {
        let conf = "[PowerOptions]\nA=1";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(
            out,
            format!("[PowerOptions]\nA=1\n\n[OneStoreServices]\napi_endpoint={NEW}\n")
        );

        let conf = "[OneStoreServices]\napi_endpoint=old";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(out, format!("[OneStoreServices]\napi_endpoint={NEW}"));

        let conf = "[OneStoreServices]";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(out, format!("[OneStoreServices]\napi_endpoint={NEW}\n"));
    }

    #[test]
    fn a_commented_out_line_is_not_the_setting() {
        let conf = "[OneStoreServices]\n#api_endpoint=old\n";
        let out = text(&patch(conf.as_bytes(), NEW).unwrap());
        assert_eq!(
            out,
            format!("[OneStoreServices]\napi_endpoint={NEW}\n#api_endpoint=old\n")
        );
    }

    #[test]
    fn a_repeated_key_is_replaced_every_time() {
        let conf = "[OneStoreServices]\napi_endpoint=a\napi_endpoint=b\n";
        let out = patch(conf.as_bytes(), NEW).unwrap();
        assert_eq!(current_endpoint(&out).as_deref(), Some(NEW));
        assert!(!text(&out).contains("=a") && !text(&out).contains("=b"));
    }

    #[test]
    fn refuses_files_that_are_not_a_kobo_config() {
        let too_big = format!("[PowerOptions]\n{}", "x".repeat(MAX_BYTES));
        for file in [
            &b""[..],
            b"hello world\n",
            b"<html><body>[not it]</body></html>",
            b"[SomeOtherApp]\napi_endpoint=x\n",
            b"\x89PNG\r\n\x1a\n\0\0\0",
            b"[PowerOptions]\n\0binary\n",
            too_big.as_bytes(),
        ] {
            assert_eq!(patch(file, NEW), Err(PatchError::NotAKoboConfig));
            assert!(!looks_like_config(file));
        }
        assert!(looks_like_config(CONF.as_bytes()));
    }

    #[test]
    fn refuses_an_address_that_could_add_lines() {
        for endpoint in [
            "https://x\n[Other]\nevil=1",
            "https://x\r",
            "ftp://x",
            "",
            "https://x y",
        ] {
            assert_eq!(
                patch(CONF.as_bytes(), endpoint),
                Err(PatchError::BadEndpoint),
                "{endpoint:?}"
            );
        }
    }

    #[test]
    fn reads_the_current_address() {
        assert_eq!(
            current_endpoint(CONF.as_bytes()).as_deref(),
            Some("https://old.example.net/kobo/old")
        );
        assert_eq!(current_endpoint(b"[PowerOptions]\napi_endpoint=x\n"), None);
    }

    /// Any line outside the setting, in any section, in any order.
    fn other_line() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z][A-Za-z_]{0,8}=[ -~]{0,20}",
            Just(String::new()),
            "#[ -~]{0,20}",
            Just("[PowerOptions]".to_owned()),
            Just("[Reading]".to_owned()),
        ]
        .prop_filter("not the setting", |line| !line.starts_with("api_endpoint"))
    }

    proptest! {
        #[test]
        fn every_other_line_survives(
            before in prop::collection::vec(other_line(), 0..12),
            inside in prop::collection::vec(other_line(), 0..12),
            crlf: bool,
            had_it: bool,
        ) {
            let eol = if crlf { "\r\n" } else { "\n" };
            // Stop at a section header so that "inside" stays in the section.
            let inside: Vec<String> = inside
                .into_iter()
                .take_while(|line| !line.starts_with('['))
                .collect();
            let mut source = vec!["[PowerOptions]".to_owned()];
            source.extend(before);
            source.push("[OneStoreServices]".to_owned());
            if had_it {
                source.push("api_endpoint=https://old.example.net".to_owned());
            }
            source.extend(inside);
            let conf = source.join(eol) + eol;

            let out = patch(conf.as_bytes(), NEW).unwrap();
            prop_assert_eq!(current_endpoint(&out), Some(NEW.to_owned()));

            let out_text = text(&out);
            let without: Vec<&str> = out_text
                .split(eol)
                .filter(|line| !line.starts_with("api_endpoint="))
                .collect();
            let conf_without: Vec<&str> = conf
                .split(eol)
                .filter(|line| !line.starts_with("api_endpoint="))
                .collect();
            prop_assert_eq!(without, conf_without);
            prop_assert_eq!(patch(&out, NEW).unwrap(), out);
        }
    }
}
