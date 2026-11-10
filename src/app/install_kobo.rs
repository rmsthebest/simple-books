//! Installs Simple Books on a Kobo plugged into this computer, all from the
//! browser: it downloads the files from `/device/kobo-files`, writes them
//! onto the Kobo's drive, and pairs the reader with this site. See
//! `server::reader::install` and `docs/reader-kobo.md`.
//!
//! The order matters. The reader's files go first, then its pairing and its
//! requests for the plugin (`install-boot`, `start-reader`), and last of all
//! `.kobo/Kobo.tgz`: once that's there, the Kobo installs the plugin when it
//! restarts after ejecting, and the plugin acts on the requests.
//!
//! Only Chromium browsers can write to a drive; elsewhere the island says
//! so.

use leptos::prelude::*;

// Only the browser's half makes the outcomes, so the server's build never sees them.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
enum Status {
    Idle,
    /// What it's doing now.
    Working(String),
    Done(Done),
    Failed(String),
}

#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Done {
    /// It was paired already, and kept that.
    kept_pairing: bool,
    /// It already started Simple Books when switched on.
    starts_already: bool,
}

#[island]
pub fn InstallKobo() -> impl IntoView {
    let (ready, set_ready) = signal(false);
    let (can_write, set_can_write) = signal(false);
    let (status, set_status) = signal(Status::Idle);

    Effect::new(move |_| {
        set_ready.set(true);
        #[cfg(feature = "hydrate")]
        set_can_write.set(crate::app::drive::can_write_to_drive());
        #[cfg(not(feature = "hydrate"))]
        let _ = set_can_write;
    });

    let working = move || matches!(status.get(), Status::Working(_));
    let on_install = move |_| {
        set_status.set(Status::Working("Choose your Kobo's drive.".to_owned()));
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            let progress = move |step: String| set_status.set(Status::Working(step));
            set_status.set(match browser::install(progress).await {
                Ok(Some(done)) => Status::Done(done),
                Ok(None) => Status::Idle,
                Err(message) => Status::Failed(message),
            });
        });
    };

    view! {
        <div class="connect install" data-hydrated=move || ready.get().to_string()>
            <h2>"Install Simple Books on your Kobo"</h2>
            <p>
                "Simple Books can take over your Kobo: its own home screen and reader, with "
                "fast page turns and no ads, syncing with this site. Plug your Kobo into this "
                "computer, choose its drive, then eject it. It restarts once and opens Simple Books."
            </p>
            <p class="muted">
                "Your Kobo's own software stays: on the Kobo, Settings, then Kobo, switches back to it "
                "or uninstalls Simple Books."
            </p>
            <div class:hidden=move || !ready.get()>
                <div class:hidden=move || !can_write.get()>
                    <button type="button" on:click=on_install disabled=working>
                        "Choose your Kobo's drive"
                    </button>
                    <p class="muted">"Look for the drive called KOBOeReader."</p>
                </div>
                <p class="muted" class:hidden=move || can_write.get()>
                    "Installing needs Chrome or Edge on a computer, which can write to your Kobo's drive. "
                    "A reader that's already installed pairs with a code, below."
                </p>
            </div>

            {move || match status.get() {
                Status::Idle => ().into_any(),
                Status::Working(step) => view! { <p role="status">{step}</p> }.into_any(),
                Status::Done(done) => view! {
                    <div class="flash" role="status">
                        <p>
                            <strong>"Done."</strong> " Now eject your Kobo, and wait until your computer "
                            "says it's safe before you unplug it. It restarts to finish installing, "
                            {if done.starts_already {
                                "and opens Simple Books as before."
                            } else {
                                "then opens Simple Books."
                            }}
                        </p>
                        <p>
                            {if done.kept_pairing {
                                "It was paired already, so it stays as it was."
                            } else {
                                "It's paired with this site, and syncs once it's on Wi-Fi."
                            }}
                        </p>
                        <p><a href="/device">"See your devices"</a></p>
                    </div>
                }
                .into_any(),
                Status::Failed(message) => view! {
                    <p class="flash error" role="alert">{message}</p>
                }
                .into_any(),
            }}
        </div>
    }
}

#[cfg(feature = "hydrate")]
mod browser {
    use serde::Deserialize;
    use wasm_bindgen::JsValue;

    use super::Done;
    use crate::app::drive::{
        Picked, fetch, has_file, made_directory, pick_drive, read_named, remove_file, subdirectory,
        write_file,
    };

    const CONFIG_NAME: &str = "Kobo eReader.conf";
    /// The plugin, which the Kobo installs when it next starts.
    const PLUGIN_PATH: &str = ".kobo/Kobo.tgz";
    const APP: [&str; 2] = [".adds", "simple-books"];
    const NOT_KOBO: &str =
        "That doesn't look like your Kobo's drive. Choose the one called KOBOeReader.";
    const NOT_SAVED: &str = "A file didn't save properly on your Kobo. Eject it, plug it in again \
                             and try once more.";
    /// What the request files say, for anyone looking.
    const REQUESTED: &[u8] = b"Asked for by the website's installer.\n";

    #[derive(Deserialize)]
    struct Installed {
        secret: String,
        server: String,
    }

    /// Installs, saying how it's going. `None` if no drive was chosen.
    pub async fn install(progress: impl Fn(String)) -> Result<Option<Done>, String> {
        let root = match pick_drive().await.map_err(describe)? {
            Picked::Drive(root) => root,
            Picked::Cancelled => return Ok(None),
            Picked::Refused => {
                return Err("This browser won't open that drive. Try choosing it again.".to_owned());
            }
        };

        progress("Checking it's a Kobo…".to_owned());
        let not_kobo = |_| NOT_KOBO.to_owned();
        let kobo = subdirectory(&root, ".kobo").await.map_err(not_kobo)?;
        let settings = subdirectory(&kobo, "Kobo").await.map_err(not_kobo)?;
        let config = read_named(&settings, CONFIG_NAME).await.map_err(not_kobo)?;
        if !kobo_conf::looks_like_config(&config) {
            return Err(NOT_KOBO.to_owned());
        }
        if has_file(&kobo, "Kobo.tgz").await {
            return Err(
                "Your Kobo has an update waiting to install. Eject it and let it restart \
                        and finish, then plug it in and try again."
                    .to_owned(),
            );
        }

        progress("Downloading Simple Books…".to_owned());
        let (status, tar) = fetch("GET", "/device/kobo-files").await.map_err(describe)?;
        answered(status, &tar)?;
        let entries = kobo_files::read(&tar)
            .map_err(|error| format!("The download was damaged ({error}). Try again."))?;
        let Some(plugin) = entries.iter().find(|e| e.path == PLUGIN_PATH) else {
            return Err("The download from this site is missing a part. Try again.".to_owned());
        };
        let files: Vec<_> = entries.iter().filter(|e| e.path != PLUGIN_PATH).collect();
        for (n, entry) in files.iter().enumerate() {
            let (folders, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
            let folders: Vec<&str> = folders.split('/').filter(|f| !f.is_empty()).collect();
            progress(format!("Copying {name} ({} of {})…", n + 1, files.len()));
            let dir = made_directory(&root, &folders).await.map_err(describe)?;
            if !write_file(&dir, name, entry.bytes)
                .await
                .map_err(describe)?
            {
                return Err(NOT_SAVED.to_owned());
            }
        }

        let app = made_directory(&root, &APP).await.map_err(describe)?;
        made_directory(&app, &["books"]).await.map_err(describe)?;
        // Left by an earlier uninstall, these would undo this one.
        for stale in ["no-plugin", "uninstall"] {
            remove_file(&app, stale).await.map_err(describe)?;
        }
        let kept_pairing = has_file(&app, "sync-secret").await;
        if !kept_pairing {
            progress("Pairing it with this site…".to_owned());
            let (status, body) = fetch("POST", "/device/kobo-install")
                .await
                .map_err(describe)?;
            answered(status, &body)?;
            let installed: Installed = serde_json::from_slice(&body)
                .map_err(|_| "This site's answer didn't make sense. Try again.".to_owned())?;
            let server = format!("{}\n", installed.server);
            for (name, bytes) in [
                ("server.txt", server.as_bytes()),
                ("sync-secret", installed.secret.as_bytes()),
            ] {
                if !write_file(&app, name, bytes).await.map_err(describe)? {
                    return Err(NOT_SAVED.to_owned());
                }
            }
        }
        // Simple Books starts at boot already, or the plugin is asked to
        // make it, and to start it this once.
        let starts_already = has_file(&app, "boot").await;
        if !starts_already {
            for name in ["install-boot", "start-reader"] {
                if !write_file(&app, name, REQUESTED).await.map_err(describe)? {
                    return Err(NOT_SAVED.to_owned());
                }
            }
        }

        progress("Finishing…".to_owned());
        if !write_file(&kobo, "Kobo.tgz", plugin.bytes)
            .await
            .map_err(describe)?
        {
            return Err(NOT_SAVED.to_owned());
        }
        Ok(Some(Done {
            kept_pairing,
            starts_already,
        }))
    }

    /// An error for anything but a success from this site.
    fn answered(status: u16, body: &[u8]) -> Result<(), String> {
        match status {
            200 => Ok(()),
            401 => Err("You've been signed out. Sign in again, then try once more.".to_owned()),
            404 => Err(format!(
                "This site can't install Simple Books: {}",
                String::from_utf8_lossy(body)
            )),
            _ => Err(format!(
                "This site couldn't do that (error {status}). Try again."
            )),
        }
    }

    /// Browser errors are written for developers, so they only reach the console.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "it goes straight into map_err"
    )]
    fn describe(error: JsValue) -> String {
        web_sys::console::error_1(&error);
        "The browser couldn't write to your Kobo. Eject it, plug it in again and try once more."
            .to_owned()
    }
}
