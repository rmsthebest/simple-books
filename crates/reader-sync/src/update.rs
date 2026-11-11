//! The reader updating itself to the app the server it syncs with would
//! install: the files of its app folder, `.adds/simple-books` on the Kobo.
//!
//! 1. After a sync, [`stage`] asks the server for its files. If their
//!    `version` isn't this one's, it downloads the files that differ into
//!    `update/`, checks each, and writes `update/ready.json` last.
//! 2. When asked to, [`apply`] moves each staged file over the one it
//!    replaces, keeping that in `previous/`, and leaves `update-trying`.
//!    The platform then starts the new version.
//! 3. The new version, once it's up, removes `update-trying` with
//!    [`settled`]. Still there when it exits, it never got that far, and
//!    the scripts that start it put `previous/` back, as [`roll_back`]
//!    does.
//!
//! The server is trusted as the place the books come from is: over HTTPS,
//! with the reader's secret.

use std::{fs, io, path::Path, sync::atomic::AtomicBool};

use serde::{Deserialize, Serialize};
use sync_proto::{KoboFile, is_kobo_file_name};

use crate::{
    Client, Error,
    client::{save_json, sha256_of},
};

/// Where the files of an update wait, in the app folder.
pub const STAGING: &str = "update";
/// What an update replaced, in the app folder, until the next one.
pub const PREVIOUS: &str = "previous";
/// There from when an update is put in place until the new version is up.
pub const TRYING: &str = "update-trying";
/// The version file, in the app folder and among the server's files.
pub const VERSION: &str = "version";
/// In [`STAGING`], once everything in it is checked.
const READY: &str = "ready.json";

/// What's staged, as `ready.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Ready {
    version: String,
    /// Only those that differ from the app folder's.
    files: Vec<KoboFile>,
}

/// An update put in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub version: String,
    /// The files replaced or added, by name.
    pub files: Vec<String>,
}

/// The version in the app folder `dir`, if it says.
pub fn running(dir: &Path) -> Option<String> {
    let version = fs::read_to_string(dir.join(VERSION)).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_owned())
}

/// The version staged in `dir` and ready to apply, if there is one.
pub fn ready(dir: &Path) -> Option<String> {
    load_ready(dir).map(|ready| ready.version)
}

fn load_ready(dir: &Path) -> Option<Ready> {
    let bytes = fs::read(dir.join(STAGING).join(READY)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Asks the server what it would install, and stages it if it isn't what's
/// in `dir`. Returns the version ready to apply, if there is one. Setting
/// `stop` ends it soon after, keeping what's downloaded for next time.
pub fn stage(
    client: &Client,
    secret: &str,
    dir: &Path,
    stop: &AtomicBool,
) -> Result<Option<String>, Error> {
    let Some(offered) = client.kobo_files(secret)? else {
        clear(dir)?;
        return Ok(None);
    };
    if let Some(bad) = offered.files.iter().find(|f| !is_kobo_file_name(&f.name)) {
        return Err(Error::Damaged(format!(
            "the server offered a file called {:?}",
            bad.name
        )));
    }
    if !offered.files.iter().any(|f| f.name == VERSION) {
        return Err(Error::Damaged("the server's files have no version".into()));
    }
    if running(dir).as_deref() == Some(offered.version.as_str()) {
        clear(dir)?;
        return Ok(None);
    }
    if ready(dir).as_deref() == Some(offered.version.as_str()) {
        return Ok(Some(offered.version));
    }
    let staging = dir.join(STAGING);
    remove_file(&staging.join(READY))?;
    fs::create_dir_all(&staging)?;
    let mut changed = Vec::new();
    for file in &offered.files {
        if same(&dir.join(&file.name), file) {
            continue;
        }
        let staged = staging.join(&file.name);
        if !same(&staged, file) {
            let partial = staging.join(format!("{}.part", file.name));
            client.kobo_file(secret, file, &partial, stop)?;
            fs::rename(&partial, &staged)?;
        }
        changed.push(file.clone());
    }
    // Anything left from staging another version.
    for item in fs::read_dir(&staging)? {
        let item = item?;
        let name = item.file_name();
        if !changed.iter().any(|f| *f.name == *name) {
            remove_file(&item.path())?;
        }
    }
    save_json(
        &staging.join(READY),
        &Ready {
            version: offered.version.clone(),
            files: changed,
        },
    )?;
    Ok(Some(offered.version))
}

/// Whether `path` holds just what `file` says.
fn same(path: &Path, file: &KoboFile) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == file.size)
        && sha256_of(path).is_ok_and(|sha256| sha256 == file.sha256)
}

/// Forgets anything staged.
fn clear(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir.join(STAGING)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Puts the staged update in place in `dir`: each file moves over the one
/// it replaces, which goes to `previous/`, and `version` goes last. Leaves
/// [`TRYING`], for the new version to remove once it's up. If anything
/// goes wrong, puts the old files back and says what. `None` if nothing is
/// staged.
pub fn apply(dir: &Path) -> io::Result<Option<Applied>> {
    let Some(mut ready) = load_ready(dir) else {
        return Ok(None);
    };
    let staging = dir.join(STAGING);
    for file in &ready.files {
        if !is_kobo_file_name(&file.name) || !same(&staging.join(&file.name), file) {
            clear(dir)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} changed after it was downloaded", file.name),
            ));
        }
    }
    let previous = dir.join(PREVIOUS);
    match fs::remove_dir_all(&previous) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::create_dir_all(&previous)?;
    write_synced(&dir.join(TRYING), ready.version.as_bytes())?;
    ready.files.sort_by_key(|f| f.name == VERSION);
    let mut moved = Vec::new();
    for file in &ready.files {
        if let Err(error) = replace(dir, &previous, &staging, &file.name) {
            roll_back(dir)?;
            return Err(io::Error::new(
                error.kind(),
                format!("putting {} in place: {error}", file.name),
            ));
        }
        moved.push(file.name.clone());
    }
    clear(dir)?;
    Ok(Some(Applied {
        version: ready.version,
        files: moved,
    }))
}

fn replace(dir: &Path, previous: &Path, staging: &Path, name: &str) -> io::Result<()> {
    let old = dir.join(name);
    if old.exists() {
        fs::rename(&old, previous.join(name))?;
    }
    fs::rename(staging.join(name), &old)
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The new version is up: forgets [`TRYING`]. Returns the version it was
/// trying, if it was.
pub fn settled(dir: &Path) -> Option<String> {
    let path = dir.join(TRYING);
    let version = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    Some(version.trim().to_owned())
}

/// Puts back everything in `previous/`, as before the last update, and
/// forgets [`TRYING`]. Returns the files put back. The files the update
/// added stay; nothing uses them.
pub fn roll_back(dir: &Path) -> io::Result<Vec<String>> {
    let previous = dir.join(PREVIOUS);
    let mut back = Vec::new();
    if previous.is_dir() {
        for item in fs::read_dir(&previous)? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            fs::rename(item.path(), dir.join(&name))?;
            back.push(name);
        }
    }
    remove_file(&dir.join(TRYING))?;
    back.sort();
    Ok(back)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sha2::{Digest, Sha256};

    use super::*;

    fn staged_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(STAGING).join(name)
    }

    fn file(name: &str, bytes: &[u8]) -> KoboFile {
        KoboFile {
            name: name.to_owned(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }

    /// An app folder at v1, with v2's changed files staged.
    fn staged() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path();
        fs::write(app.join("reader-kobo"), b"reader 1").unwrap();
        fs::write(app.join("dict-en-sv"), b"same").unwrap();
        fs::write(app.join(VERSION), b"v1\n").unwrap();
        fs::create_dir_all(app.join(STAGING)).unwrap();
        fs::write(staged_path(app, "reader-kobo"), b"reader 2").unwrap();
        fs::write(staged_path(app, "new-file"), b"new").unwrap();
        fs::write(staged_path(app, VERSION), b"v2\n").unwrap();
        save_json(
            &app.join(STAGING).join(READY),
            &Ready {
                version: "v2".into(),
                files: vec![
                    file(VERSION, b"v2\n"),
                    file("reader-kobo", b"reader 2"),
                    file("new-file", b"new"),
                ],
            },
        )
        .unwrap();
        dir
    }

    #[test]
    fn applying_keeps_what_it_replaced_until_rolled_back() {
        let dir = staged();
        let app = dir.path();
        assert_eq!(running(app).as_deref(), Some("v1"));
        assert_eq!(ready(app).as_deref(), Some("v2"));

        let applied = apply(app).unwrap().unwrap();
        assert_eq!(applied.version, "v2");
        assert_eq!(applied.files, ["reader-kobo", "new-file", VERSION]);
        assert_eq!(running(app).as_deref(), Some("v2"));
        assert_eq!(fs::read(app.join("reader-kobo")).unwrap(), b"reader 2");
        assert_eq!(fs::read(app.join("dict-en-sv")).unwrap(), b"same");
        assert_eq!(
            fs::read(app.join(PREVIOUS).join("reader-kobo")).unwrap(),
            b"reader 1"
        );
        assert!(!app.join(STAGING).exists());
        assert!(app.join(TRYING).exists());
        assert_eq!(ready(app), None);
        assert_eq!(apply(app).unwrap(), None, "nothing left to apply");

        assert_eq!(roll_back(app).unwrap(), ["reader-kobo", VERSION]);
        assert_eq!(running(app).as_deref(), Some("v1"));
        assert_eq!(fs::read(app.join("reader-kobo")).unwrap(), b"reader 1");
        assert!(!app.join(TRYING).exists());
    }

    #[test]
    fn once_up_the_new_version_is_kept() {
        let dir = staged();
        let app = dir.path();
        apply(app).unwrap().unwrap();
        assert_eq!(settled(app).as_deref(), Some("v2"));
        assert_eq!(settled(app), None);
        assert_eq!(running(app).as_deref(), Some("v2"));
    }

    #[test]
    fn a_staged_file_that_changed_is_not_applied() {
        let dir = staged();
        let app = dir.path();
        fs::write(staged_path(app, "reader-kobo"), b"reader ?").unwrap();
        assert!(apply(app).is_err());
        assert_eq!(fs::read(app.join("reader-kobo")).unwrap(), b"reader 1");
        assert!(!app.join(TRYING).exists());
        assert_eq!(ready(app), None, "staged afresh next time");
    }
}
