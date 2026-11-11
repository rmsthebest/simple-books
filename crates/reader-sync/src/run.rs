//! One sync, from start to end.
//!
//! What should be on the reader is kept as state, not as a list of things to
//! do, so any step can fail and the next sync carries on:
//!
//! 1. **Places:** each local place that isn't what the server last confirmed
//!    is sent. Where the server has a newer one, that comes back to apply.
//! 2. **Changes:** pages of them, from where the last run ended. Each page
//!    updates the state, which is saved before the next is asked for.
//! 3. **Files:** every listed book whose file isn't the server's is
//!    downloaded, newest first, carrying on from any partial download, and
//!    checked against its hash before it replaces anything.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    sync::atomic::{AtomicBool, Ordering},
};

use sync_proto::Spot;

use crate::{
    Client, Error,
    state::{Paths, State, Synced},
};

/// What a sync did, for the reader to act on and show.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Places sent.
    pub sent: usize,
    /// Places from elsewhere that are newer than the reader's, by book. The
    /// reader moves to them.
    pub places: Vec<(String, Spot)>,
    /// Books whose details changed, such as a title edited on the website.
    pub changed: Vec<String>,
    /// Books now downloaded, new or made again.
    pub downloaded: Vec<String>,
    /// Books removed, whose files are gone.
    pub removed: Vec<String>,
    /// Books that couldn't be downloaded this time, and why. The next sync
    /// tries them again.
    pub failed: Vec<(String, String)>,
}

impl Report {
    /// Whether the library on the reader changed.
    pub fn library_changed(&self) -> bool {
        !(self.changed.is_empty() && self.downloaded.is_empty() && self.removed.is_empty())
    }
}

/// How far a sync has got, for a progress line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Sending,
    Checking,
    /// Downloading the `done + 1`th of `total` books.
    Downloading {
        done: usize,
        total: usize,
        title: String,
    },
}

/// Syncs once. `local` is where the reader is in each synced book it has
/// opened, by book id. Saves `state` to `paths.state` as it goes, so
/// stopping at any point loses nothing. Setting `stop` ends it at the next
/// chance with [`Error::Stopped`].
pub fn sync(
    client: &Client,
    secret: &str,
    paths: &Paths,
    state: &mut State,
    local: &BTreeMap<String, Spot>,
    progress: &mut dyn FnMut(Progress),
    stop: &AtomicBool,
) -> Result<Report, Error> {
    let stopped = || {
        if stop.load(Ordering::Relaxed) {
            Err(Error::Stopped)
        } else {
            Ok(())
        }
    };
    let mut report = Report::default();
    fs::create_dir_all(&paths.books)?;

    progress(Progress::Sending);
    send_places(client, secret, state, local, &mut report)?;
    state.save(&paths.state)?;

    progress(Progress::Checking);
    loop {
        stopped()?;
        let page = client.changes(secret, state.since)?;
        if page.full && state.full_run.is_none() {
            state.full_run = Some(BTreeSet::new());
        }
        for book in page.books {
            if let Some(listed) = state.full_run.as_mut() {
                listed.insert(book.id.clone());
            }
            let id = book.id.clone();
            match state.books.get_mut(&id) {
                Some(synced) if synced.book == book => {}
                Some(synced) => {
                    let details = |b: &sync_proto::Book| {
                        (
                            b.title.clone(),
                            b.authors.clone(),
                            b.series.clone(),
                            b.cover.clone(),
                        )
                    };
                    if details(&synced.book) != details(&book) {
                        report.changed.push(id);
                    }
                    synced.book = book;
                }
                None => {
                    state.books.insert(id, Synced { book, have: None });
                }
            }
        }
        for id in page.removed {
            remove(paths, state, &id, &mut report)?;
        }
        for place in page.places {
            let newer = local
                .get(&place.book)
                .is_none_or(|mine| place.spot.at > mine.at && place.spot != *mine);
            if newer && state.books.contains_key(&place.book) {
                report.places.push((place.book.clone(), place.spot));
            }
            state.server_places.insert(place.book, place.spot);
        }
        state.since = page.next;
        if !page.more
            && let Some(listed) = state.full_run.take()
        {
            let gone: Vec<String> = state
                .books
                .keys()
                .filter(|id| !listed.contains(*id))
                .cloned()
                .collect();
            for id in gone {
                remove(paths, state, &id, &mut report)?;
            }
        }
        state.save(&paths.state)?;
        if !page.more {
            break;
        }
    }

    download_all(client, secret, paths, state, &mut report, progress, stop)?;
    Ok(report)
}

/// Sends the places in `local` the server doesn't have yet, and nothing
/// else: what the reader does before it sleeps. Saves `state`.
pub fn send(
    client: &Client,
    secret: &str,
    paths: &Paths,
    state: &mut State,
    local: &BTreeMap<String, Spot>,
) -> Result<Report, Error> {
    let mut report = Report::default();
    send_places(client, secret, state, local, &mut report)?;
    state.save(&paths.state)?;
    Ok(report)
}

/// Whether any place in `local` is one the server doesn't have yet.
pub fn unsent(state: &State, local: &BTreeMap<String, Spot>) -> bool {
    local
        .iter()
        .any(|(id, spot)| needs_sending(state, id, spot))
}

/// A synced book's place that differs from the server's.
fn needs_sending(state: &State, id: &str, spot: &Spot) -> bool {
    state.books.contains_key(id) && state.server_places.get(id) != Some(spot)
}

fn send_places(
    client: &Client,
    secret: &str,
    state: &mut State,
    local: &BTreeMap<String, Spot>,
    report: &mut Report,
) -> Result<(), Error> {
    for (id, spot) in local {
        if !needs_sending(state, id, spot) {
            continue;
        }
        match client.put_place(secret, id, spot) {
            Ok(answer) => {
                report.sent += 1;
                if !answer.kept && answer.place != *spot {
                    report.places.push((id.clone(), answer.place));
                }
                state.server_places.insert(id.clone(), answer.place);
            }
            // Removed meanwhile: the changes say so.
            Err(Error::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn remove(paths: &Paths, state: &mut State, id: &str, report: &mut Report) -> io::Result<()> {
    let had = state.books.remove(id).is_some();
    state.server_places.remove(id);
    for path in [paths.book(id), paths.partial(id)] {
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    if had {
        report.removed.push(id.to_owned());
    }
    Ok(())
}

fn download_all(
    client: &Client,
    secret: &str,
    paths: &Paths,
    state: &mut State,
    report: &mut Report,
    progress: &mut dyn FnMut(Progress),
    stop: &AtomicBool,
) -> Result<(), Error> {
    // A file deleted by hand is fetched again.
    for (id, synced) in &mut state.books {
        if synced.have.is_some() && !paths.book(id).exists() {
            synced.have = None;
        }
    }
    let mut wanted: Vec<(String, i64)> = state
        .books
        .iter()
        .filter(|(_, synced)| {
            synced
                .book
                .file
                .as_ref()
                .is_some_and(|file| synced.have.as_ref() != Some(&file.sha256))
        })
        .map(|(id, synced)| (id.clone(), synced.book.added))
        .collect();
    wanted.sort_by_key(|(_, added)| std::cmp::Reverse(*added));

    let total = wanted.len();
    for (done, (id, _)) in wanted.into_iter().enumerate() {
        let Some(synced) = state.books.get(&id) else {
            continue;
        };
        let Some(file) = synced.book.file.clone() else {
            continue;
        };
        progress(Progress::Downloading {
            done,
            total,
            title: synced.book.title.clone(),
        });
        let partial = paths.partial(&id);
        match client.download(secret, &id, &file, &partial, stop) {
            Ok(()) => {
                fs::rename(&partial, paths.book(&id))?;
                if let Some(synced) = state.books.get_mut(&id) {
                    synced.have = Some(file.sha256);
                }
                state.save(&paths.state)?;
                report.downloaded.push(id);
            }
            Err(error @ (Error::NotFound | Error::Damaged(_) | Error::Server { .. })) => {
                report.failed.push((id, error.to_string()));
            }
            // Gone for all of them: the next sync carries on.
            Err(error @ Error::Network(_)) => {
                report.failed.push((id, error.to_string()));
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
