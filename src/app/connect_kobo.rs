//! Points a Kobo's config at this server. All of it happens in the browser: the file
//! holds Wi-Fi passwords, and it's never uploaded.
//!
//! Chromium browsers can write to the Kobo's drive directly. Everywhere else, the
//! patched file comes back as a download to copy over the original.

use leptos::prelude::*;

/// Where the file is, for the instructions and the direct route.
const CONFIG_PATH: &str = ".kobo/Kobo/Kobo eReader.conf";

// Only the browser's half makes the outcomes, so the server's build never sees them.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
enum Status {
    Idle,
    Working,
    /// Written straight to the drive.
    Written,
    /// A patched copy is ready to download, at this object URL.
    Download(String),
    Failed(String),
}

#[island]
pub fn ConnectKobo(endpoint: String) -> impl IntoView {
    let (ready, set_ready) = signal(false);
    let (can_write, set_can_write) = signal(false);
    let (status, set_status) = signal(Status::Idle);
    let endpoint = StoredValue::new(endpoint);

    Effect::new(move |_| {
        set_ready.set(true);
        #[cfg(feature = "hydrate")]
        set_can_write.set(browser::can_write_to_drive());
        #[cfg(not(feature = "hydrate"))]
        let _ = set_can_write;
    });

    let on_pick_drive = move |_| {
        set_status.set(Status::Working);
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_status.set(match browser::patch_drive(&endpoint.get_value()).await {
                Ok(browser::DriveOutcome::Written) => Status::Written,
                Ok(browser::DriveOutcome::Cancelled) => Status::Idle,
                Err(message) => Status::Failed(message),
            });
        });
    };

    let on_pick_file = move |event: leptos::ev::Event| {
        set_status.set(Status::Working);
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_status.set(
                match browser::patch_chosen_file(&event, &endpoint.get_value()).await {
                    Ok(Some(url)) => Status::Download(url),
                    Ok(None) => Status::Idle,
                    Err(message) => Status::Failed(message),
                },
            );
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (event, endpoint);
    };

    view! {
        <div class="connect" data-hydrated=move || ready.get().to_string()>
            <h2>"Connect your Kobo"</h2>
            <p>
                "Plug your Kobo into this computer. Your Kobo's settings file stays on this computer: "
                "it's changed here, in your browser, and never sent anywhere."
            </p>
            <p class="warning">
                "Your Kobo will sync with this site from now on, instead of any other server it uses."
            </p>

            <div class:hidden=move || !ready.get()>
                <div class:hidden=move || !can_write.get()>
                    <button type="button" on:click=on_pick_drive>"Choose your Kobo's drive"</button>
                    <p class="muted">"Look for the drive called KOBOeReader. A backup of the file is kept beside it."</p>
                    <p class="muted">"Or, if that doesn't work:"</p>
                </div>
                <label class="pick">
                    <span class="pick-label">"Choose the file " <code>"Kobo eReader.conf"</code></span>
                    <input type="file" accept=".conf" on:change=on_pick_file />
                </label>
                <p class="muted">
                    "It's in the folder " <code>".kobo/Kobo"</code> " on your Kobo's drive. "
                    "That folder is hidden: on a Mac press Cmd+Shift+. in the file picker, on Windows "
                    "type the path, and on Linux press Ctrl+H."
                </p>
            </div>

            {move || match status.get() {
                Status::Idle => ().into_any(),
                Status::Working => view! { <p role="status">"Working…"</p> }.into_any(),
                Status::Written => view! {
                    <div class="flash" role="status">
                        <p>
                            <strong>"Done."</strong> " Now eject your Kobo, and wait until your computer says "
                            "it's safe before you unplug it. Unplugging too soon can undo your Kobo's settings. "
                            "Then tap sync on it."
                        </p>
                        <p><a href="/device">"See whether it's connected"</a></p>
                    </div>
                }
                .into_any(),
                Status::Download(url) => view! {
                    <div class="flash" role="status">
                        <p>
                            <a class="button" href=url download="Kobo eReader.conf">"Download the changed file"</a>
                        </p>
                        <p>
                            "Copy it over " <code>{CONFIG_PATH}</code>
                            " on your Kobo's drive. Then eject your Kobo, wait until your computer says "
                            "it's safe, unplug it and tap sync on it."
                        </p>
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
    use js_sys::{Array, Uint8Array};
    use wasm_bindgen::{JsCast, JsValue};
    use web_sys::{Blob, BlobPropertyBag, File, HtmlInputElement, Url};

    pub use crate::app::drive::can_write_to_drive;
    use crate::app::drive::{
        Picked, call_async, pick_drive, read_bytes, read_named, subdirectory, write_file,
    };

    const BACKUP_NAME: &str = "Kobo eReader.conf.bak";
    const CONFIG_NAME: &str = "Kobo eReader.conf";
    const NOT_SAVED: &str = "The file didn't save properly on your Kobo. Choose the file instead.";

    pub enum DriveOutcome {
        Written,
        Cancelled,
    }

    /// An earlier backup is kept if it's a whole config: it's the last known good one,
    /// and the file itself may already be pointing here. An empty or broken one is
    /// replaced.
    async fn has_good_backup(dir: &JsValue) -> bool {
        read_named(dir, BACKUP_NAME)
            .await
            .is_ok_and(|bytes| kobo_conf::looks_like_config(&bytes))
    }

    pub async fn patch_drive(endpoint: &str) -> Result<DriveOutcome, String> {
        let root = match pick_drive().await.map_err(describe)? {
            Picked::Drive(root) => root,
            Picked::Cancelled => return Ok(DriveOutcome::Cancelled),
            Picked::Refused => {
                return Err(
                    "This browser won't open that drive. Choose the file instead.".to_owned(),
                );
            }
        };

        let not_kobo = |_| {
            "That doesn't look like your Kobo's drive. Choose the one called KOBOeReader."
                .to_owned()
        };
        let hidden = subdirectory(&root, ".kobo").await.map_err(not_kobo)?;
        let folder = subdirectory(&hidden, "Kobo").await.map_err(not_kobo)?;
        let handle = call_async(&folder, "getFileHandle", &[JsValue::from_str(CONFIG_NAME)])
            .await
            .map_err(not_kobo)?;
        let file: Blob = call_async(&handle, "getFile", &[])
            .await
            .map_err(describe)?
            .dyn_into()
            .map_err(describe)?;
        let original = read_bytes(&file).await.map_err(describe)?;
        let patched = kobo_conf::patch(&original, endpoint).map_err(|error| error.to_string())?;

        // The settings file is only touched once a good copy of it is safely beside it.
        if !has_good_backup(&folder).await
            && !write_file(&folder, BACKUP_NAME, &original)
                .await
                .map_err(describe)?
        {
            return Err(NOT_SAVED.to_owned());
        }
        if !write_file(&folder, CONFIG_NAME, &patched)
            .await
            .map_err(describe)?
        {
            return Err(format!(
                "{NOT_SAVED} If your Kobo's settings look wrong afterwards, rename \
                 {BACKUP_NAME} back to {CONFIG_NAME}."
            ));
        }
        Ok(DriveOutcome::Written)
    }

    pub async fn patch_chosen_file(
        event: &web_sys::Event,
        endpoint: &str,
    ) -> Result<Option<String>, String> {
        let input: HtmlInputElement = event
            .target()
            .and_then(|target| target.dyn_into().ok())
            .ok_or("No file was chosen")?;
        let Some(file): Option<File> = input.files().and_then(|files| files.get(0)) else {
            return Ok(None);
        };
        let original = read_bytes(&file).await.map_err(describe)?;
        let patched = kobo_conf::patch(&original, endpoint).map_err(|error| error.to_string())?;

        let bag = BlobPropertyBag::new();
        bag.set_type("application/octet-stream");
        let parts = Array::of1(&Uint8Array::from(patched.as_slice()));
        let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &bag).map_err(describe)?;
        Url::create_object_url_with_blob(&blob)
            .map(Some)
            .map_err(describe)
    }

    /// Browser errors are written for developers, so they only reach the console.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "it goes straight into map_err"
    )]
    fn describe(error: JsValue) -> String {
        web_sys::console::error_1(&error);
        "The browser couldn't do that. Choose the file instead.".to_owned()
    }
}
