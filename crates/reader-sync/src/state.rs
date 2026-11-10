//! What the reader keeps between syncs: where the next one starts, every
//! book the server has listed, and the place it last confirmed for each.
//! The secret is kept apart, in its own file only the owner can read.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sync_proto::{Book, Spot};

use crate::client::save_json;

/// Where a reader keeps its sync's files.
#[derive(Debug, Clone)]
pub struct Paths {
    /// [`State`], as JSON.
    pub state: PathBuf,
    /// The secret, alone.
    pub secret: PathBuf,
    /// Where books go, as `{id}.book`; partial downloads are `{id}.part`.
    pub books: PathBuf,
}

impl Paths {
    /// The usual files in an app folder.
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            state: dir.join("sync.json"),
            secret: dir.join("sync-secret"),
            books: dir.join("books"),
        }
    }

    pub fn book(&self, id: &str) -> PathBuf {
        self.books.join(format!("{id}.book"))
    }

    pub(crate) fn partial(&self, id: &str) -> PathBuf {
        self.books.join(format!("{id}.part"))
    }

    /// The secret, if paired.
    pub fn secret(&self) -> Option<String> {
        let secret = fs::read_to_string(&self.secret).ok()?;
        let secret = secret.trim();
        (!secret.is_empty()).then(|| secret.to_owned())
    }

    /// Keeps the secret where only this user can read it.
    pub fn save_secret(&self, secret: &str) -> io::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;

        let temporary = self.secret.with_extension("new");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(secret.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &self.secret)
    }

    /// Forgets the secret and everything synced, as when unpairing. The
    /// books stay, as books added by hand.
    pub fn forget(&self) -> io::Result<()> {
        for path in [&self.secret, &self.state] {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }
}

/// A book the server has listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Synced {
    pub book: Book,
    /// The SHA-256 of the file downloaded for it, if any. When it isn't the
    /// server's, the file is downloaded again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub have: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Where the next run of changes starts.
    pub since: i64,
    /// Every book the server has listed and not removed, by id.
    #[serde(default)]
    pub books: BTreeMap<String, Synced>,
    /// The place the server last said it has, by book. A local place that
    /// differs is sent at the next sync.
    #[serde(default)]
    pub server_places: BTreeMap<String, Spot>,
    /// While a run from nothing is under way, the books it has listed so
    /// far. When it ends, any others are gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_run: Option<BTreeSet<String>>,
}

impl State {
    /// The state saved at `path`, or a fresh one if there's none or it can't
    /// be read. A fresh state syncs from nothing, which puts everything right.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        save_json(path, self)
    }

    /// The synced book a file in the books folder is, if it's one.
    pub fn book_for_file(&self, file: &str) -> Option<&Synced> {
        let id = file.strip_suffix(".book")?;
        self.books.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_is_private_and_forgettable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        assert_eq!(paths.secret(), None);
        paths.save_secret("s3cret").unwrap();
        assert_eq!(paths.secret().as_deref(), Some("s3cret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&paths.secret).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        State::default().save(&paths.state).unwrap();
        paths.forget().unwrap();
        assert_eq!(paths.secret(), None);
        assert!(!paths.state.exists());
        paths.forget().unwrap();
    }

    #[test]
    fn a_damaged_state_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.json");
        fs::write(&path, b"{ half").unwrap();
        assert_eq!(State::load(&path), State::default());
        let state = State {
            since: 7,
            ..State::default()
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
    }
}
