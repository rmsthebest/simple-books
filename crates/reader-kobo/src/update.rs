//! Updating to the version a sync downloaded, when Update is tapped in
//! Settings, Kobo (see [`reader_sync::update`]). The files go in place
//! here, and the Nickel plugin among them goes into `/usr/local/Kobo`.
//! The reader then exits with [`EXIT_UPDATE`], and `reader.sh` or
//! `boot.sh` starts it again: the new version. If that exits before home
//! is up, while `update-trying` is still there, the script puts the files
//! in `previous/` back and starts the old one. Once it is up, it says it
//! was updated until tapped.

use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

use crate::{in_nickel::PLUGIN, log, sys};

/// The reader's exit status asking for the new version to be started.
pub(crate) const EXIT_UPDATE: i32 = 91;
/// The Nickel plugin, as it comes in the app folder.
const PLUGIN_FILE: &str = "libsimplebooks.so";

/// Puts the downloaded update in place.
pub(crate) fn apply(dir: &Path) -> Result<(), String> {
    let applied = match reader_sync::update::apply(dir) {
        Ok(Some(applied)) => applied,
        Ok(None) => return Err("There's no update downloaded.".to_owned()),
        Err(error) => {
            log!("update: {error}");
            sys::sync();
            return Err(format!("The update didn't go in: {error}"));
        }
    };
    log!(
        "update: {} in place: {}",
        applied.version,
        applied.files.join(", ")
    );
    if applied.files.iter().any(|name| name == PLUGIN_FILE) {
        match install_plugin(dir) {
            Ok(()) => log!("update: the Nickel plugin is in place"),
            // Nickel goes on with the one it has.
            Err(error) => log!("update: the Nickel plugin: {error}"),
        }
    }
    sys::sync();
    Ok(())
}

/// Copies the app folder's plugin beside Nickel's and renames it over the
/// old one, which a running Nickel keeps using until it restarts.
fn install_plugin(dir: &Path) -> io::Result<()> {
    let new = format!("{PLUGIN}.new");
    fs::copy(dir.join(PLUGIN_FILE), &new)?;
    fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    fs::rename(&new, PLUGIN)
}

/// Home is up: an update being tried has worked. Returns the version it
/// brought, to say so on the screen.
pub(crate) fn settled(dir: &Path) -> Option<String> {
    let version = reader_sync::update::settled(dir)?;
    log!("update: {version} is up");
    sys::sync();
    Some(version)
}
