//! Installing Simple Books on a Kobo from the website. The device page's
//! installer fetches the files with `GET /device/kobo-files`, writes them
//! onto the Kobo's drive, and then pairs it with `POST /device/kobo-install`,
//! which gives it a secret straight away, with no code to type.
//!
//! The files are what `scripts/build-kobo-files.sh` stages, in the folder
//! `KOBO_FILES` names; the server image has them built in.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use kobo_files::Entry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use crate::server::{
    auth::device::{generate_secret, hash_secret},
    db::Db,
    now,
};

/// What a reader installed from the website is called on the device page,
/// as the reader calls itself when it pairs with a code.
pub const READER_NAME: &str = "Kobo Clara BW";

/// The answer to `POST /device/kobo-install`: what the installer writes
/// into the reader's folder, as `sync-secret` and `server`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Installed {
    pub secret: String,
    pub server: String,
}

/// The Kobo files, read from disk the first time they're asked for: as
/// one tar for the installer, and as the app folder's files for readers
/// updating themselves.
#[derive(Debug)]
pub struct KoboFiles {
    dir: PathBuf,
    built: OnceCell<Built>,
}

#[derive(Debug)]
struct Built {
    tar: Bytes,
    app: Option<Arc<App>>,
}

/// The files in the app folder, `.adds/simple-books`, which a reader
/// updates itself with. Each is a slice of the tar.
#[derive(Debug)]
pub struct App {
    pub manifest: sync_proto::KoboFiles,
    files: BTreeMap<String, Bytes>,
}

impl App {
    pub fn file(&self, name: &str) -> Option<Bytes> {
        self.files.get(name).cloned()
    }
}

/// Where the app folder's files are in the tar.
const APP_FOLDER: &str = ".adds/simple-books/";

impl KoboFiles {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            built: OnceCell::new(),
        }
    }

    async fn built(&self) -> io::Result<Option<&Built>> {
        let dir = self.dir.clone();
        let made = self
            .built
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || build(&dir))
                    .await
                    .map_err(io::Error::other)?
            })
            .await;
        match made {
            Ok(built) => Ok(Some(built)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The tar, or `None` if the folder isn't there.
    pub async fn tar(&self) -> io::Result<Option<Bytes>> {
        Ok(self.built().await?.map(|built| built.tar.clone()))
    }

    /// The app folder's files, or `None` if there are none, or no
    /// `version` among them to tell readers what they'd update to.
    pub async fn app(&self) -> io::Result<Option<Arc<App>>> {
        Ok(self.built().await?.and_then(|built| built.app.clone()))
    }
}

/// Every file under `dir`, by path, but with `.kobo/Kobo.tgz` last: the
/// Kobo installs the plugin from it when it next starts, so the installer
/// writes it only once the rest is there.
fn build(dir: &Path) -> io::Result<Built> {
    let mut found = Vec::new();
    walk(dir, "", &mut found)?;
    if found.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no Kobo files"));
    }
    found.sort_by(|a, b| (a.0 == ".kobo/Kobo.tgz", &a.0).cmp(&(b.0 == ".kobo/Kobo.tgz", &b.0)));
    let entries: Vec<Entry<'_>> = found
        .iter()
        .map(|(path, mode, bytes)| Entry {
            path: path.clone(),
            mode: *mode,
            bytes,
        })
        .collect();
    let tar = Bytes::from(kobo_files::write(&entries).map_err(io::Error::other)?);
    let app = app(&tar)?;
    tracing::info!(
        files = entries.len(),
        bytes = tar.len(),
        version = app.as_ref().map(|app| app.manifest.version.as_str()),
        dir = %dir.display(),
        "Kobo files ready to install"
    );
    Ok(Built { tar, app })
}

/// The app folder's files in `tar`, with what they're checked against.
fn app(tar: &Bytes) -> io::Result<Option<Arc<App>>> {
    let mut files = BTreeMap::new();
    let mut manifest = Vec::new();
    for entry in kobo_files::read(tar).map_err(io::Error::other)? {
        let Some(name) = entry.path.strip_prefix(APP_FOLDER) else {
            continue;
        };
        if !sync_proto::is_kobo_file_name(name) {
            continue;
        }
        manifest.push(sync_proto::KoboFile {
            name: name.to_owned(),
            size: entry.bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(entry.bytes)),
        });
        files.insert(name.to_owned(), tar.slice_ref(entry.bytes));
    }
    let Some(version) = files
        .get("version")
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_owned())
        .filter(|version| !version.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(Arc::new(App {
        manifest: sync_proto::KoboFiles {
            version,
            files: manifest,
        },
        files,
    })))
}

fn walk(dir: &Path, under: &str, found: &mut Vec<(String, u32, Vec<u8>)>) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().into_string().map_err(|name| {
            io::Error::other(format!("{} isn't UTF-8", Path::new(&name).display()))
        })?;
        let path = if under.is_empty() {
            name
        } else {
            format!("{under}/{name}")
        };
        let kind = item.file_type()?;
        if kind.is_dir() {
            walk(&item.path(), &path, found)?;
        } else if kind.is_file() {
            found.push((path, mode(&item.metadata()?), fs::read(item.path())?));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode(_: &fs::Metadata) -> u32 {
    0o644
}

/// Makes a reader device for this user, and returns the secret it syncs
/// with. Only its hash is kept.
pub async fn install(db: &Db, public_url: &str, user_id: i64) -> Result<Installed, sqlx::Error> {
    let secret = generate_secret();
    db.add_reader_device(user_id, READER_NAME, &hash_secret(&secret), now())
        .await?;
    Ok(Installed {
        secret,
        server: public_url.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_folder_becomes_a_tar_with_the_plugin_last() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(".adds/simple-books");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(dir.path().join(".kobo")).unwrap();
        fs::write(dir.path().join(".kobo/Kobo.tgz"), b"plugin").unwrap();
        fs::write(app.join("reader-kobo"), b"reader").unwrap();
        fs::write(app.join("version"), b"v1\n").unwrap();
        fs::write(dir.path().join("zzz"), b"z").unwrap();

        let files = KoboFiles::new(dir.path().to_owned());
        let tar = files.tar().await.unwrap().unwrap();
        let entries = kobo_files::read(&tar).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".adds/simple-books/reader-kobo",
                ".adds/simple-books/version",
                "zzz",
                ".kobo/Kobo.tgz"
            ]
        );
        assert_eq!(entries[0].bytes, b"reader");
        // Read once, then kept.
        fs::write(app.join("version"), b"v2\n").unwrap();
        assert_eq!(files.tar().await.unwrap().unwrap(), tar);
    }

    #[tokio::test]
    async fn readers_get_the_app_folder_with_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join(".adds/simple-books");
        fs::create_dir_all(app.join("books")).unwrap();
        fs::create_dir_all(dir.path().join(".kobo")).unwrap();
        fs::write(dir.path().join(".kobo/Kobo.tgz"), b"plugin").unwrap();
        fs::write(app.join("reader-kobo"), b"reader").unwrap();
        fs::write(app.join("books/not-this"), b"x").unwrap();
        fs::write(app.join("empty"), b"").unwrap();

        let files = KoboFiles::new(dir.path().to_owned());
        assert!(
            files.app().await.unwrap().is_none(),
            "no version, no update"
        );

        fs::write(app.join("version"), b"v1\n").unwrap();
        let files = KoboFiles::new(dir.path().to_owned());
        let found = files.app().await.unwrap().unwrap();
        assert_eq!(found.manifest.version, "v1");
        let names: Vec<&str> = found
            .manifest
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["empty", "reader-kobo", "version"]);
        let reader = &found.manifest.files[1];
        assert_eq!(reader.size, 6);
        assert_eq!(reader.sha256, hex::encode(Sha256::digest(b"reader")));
        assert_eq!(found.file("reader-kobo").unwrap(), &b"reader"[..]);
        assert_eq!(found.file("empty").unwrap(), &b""[..]);
        assert!(found.file("Kobo.tgz").is_none());
    }

    #[tokio::test]
    async fn no_folder_is_none_and_is_looked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let files = KoboFiles::new(dir.path().join("missing"));
        assert!(files.tar().await.unwrap().is_none());
        fs::create_dir_all(dir.path().join("missing")).unwrap();
        assert!(files.tar().await.unwrap().is_none());
        fs::write(dir.path().join("missing/x"), b"x").unwrap();
        assert!(files.tar().await.unwrap().is_some());
    }
}
