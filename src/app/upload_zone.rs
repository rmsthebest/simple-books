//! Getting books in. Without JavaScript this is a plain form that takes one file at
//! a time. With it, the whole page becomes a drop target with two halves, and files
//! go up two at a time with progress shown.

use leptos::prelude::*;

/// How one file is getting on.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Waiting,
    Sending(u32),
    /// Sent, and the server is reading it.
    Reading,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: u32,
    pub name: String,
    pub progress: Progress,
}

fn describe(progress: &Progress) -> String {
    match progress {
        Progress::Waiting => "Waiting".to_owned(),
        Progress::Sending(percent) => format!("Sending: {percent}%"),
        Progress::Reading => "Reading the book".to_owned(),
        Progress::Done(text) | Progress::Failed(text) => sentence(text),
    }
}

/// The server's reasons are written to follow other words, and here they stand alone.
fn sentence(text: &str) -> String {
    let mut letters = text.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "one form and its overlay, best read in one piece"
)]
#[island]
pub fn UploadZone() -> impl IntoView {
    // Becomes true once this has hydrated, which also proves that the WASM loaded
    // under the Content-Security-Policy.
    let (ready, set_ready) = signal(false);
    let (dragging, set_dragging) = signal(false);
    let (over, set_over) = signal(None::<bool>);
    let (share, set_share) = signal(false);
    let items = RwSignal::new(Vec::<Item>::new());

    Effect::new(move |_| set_ready.set(true));

    #[cfg(feature = "hydrate")]
    browser::watch_page_drags(set_dragging, set_over);

    let drop_on = move |shared: bool| {
        move |event: leptos::ev::DragEvent| {
            event.prevent_default();
            set_dragging.set(false);
            set_over.set(None);
            #[cfg(feature = "hydrate")]
            browser::dropped(&event, shared, items);
            #[cfg(not(feature = "hydrate"))]
            let _ = (shared, items);
        }
    };
    let hover = move |shared: bool| {
        move |event: leptos::ev::DragEvent| {
            event.prevent_default();
            set_over.set(Some(shared));
        }
    };
    let chosen = move |event: leptos::ev::Event| {
        #[cfg(feature = "hydrate")]
        browser::chosen(&event, share.get_untracked(), items);
        #[cfg(not(feature = "hydrate"))]
        let _ = (event, share, items);
    };

    view! {
        <section class="upload" data-hydrated=move || ready.get().to_string()>
            <form
                class="upload-form"
                method="post"
                action="/books/upload"
                enctype="multipart/form-data"
            >
                <label class="pick">
                    <span class="pick-label">
                        {move || if ready.get() { "Drop EPUBs anywhere, or choose them here" } else { "Choose an EPUB" }}
                    </span>
                    <input
                        type="file"
                        name="file"
                        accept=".epub,application/epub+zip"
                        multiple=move || ready.get()
                        required
                        on:change=chosen
                    />
                </label>
                <fieldset class="who">
                    <label>
                        <input
                            type="radio"
                            name="visibility"
                            value="private"
                            checked
                            on:change=move |_| set_share.set(false)
                        />
                        " Just for me"
                    </label>
                    <label>
                        <input
                            type="radio"
                            name="visibility"
                            value="shared"
                            on:change=move |_| set_share.set(true)
                        />
                        " Share with everyone"
                    </label>
                </fieldset>
                <button type="submit" class:hidden=move || ready.get()>"Upload"</button>
            </form>

            <ul class="uploads" class:hidden=move || items.with(Vec::is_empty) aria-live="polite">
                <For each=move || items.get() key=|item| (item.id, format!("{:?}", item.progress)) let:item>
                    <li class:failed=matches!(item.progress, Progress::Failed(_))>
                        <span class="file">{item.name.clone()}</span>
                        {match item.progress {
                            Progress::Sending(percent) => view! {
                                <progress max="100" value=percent.to_string()></progress>
                            }
                            .into_any(),
                            _ => ().into_any(),
                        }}
                        <span class="state">{describe(&item.progress)}</span>
                    </li>
                </For>
                <li class="uploads-end">
                    <a href="/">"Show my books"</a>
                </li>
            </ul>

            <div class="drop-overlay" class:active=move || dragging.get()>
                <div
                    class="drop-target"
                    class:over=move || over.get() == Some(false)
                    on:dragover=hover(false)
                    on:drop=drop_on(false)
                >
                    <strong>"Just for me"</strong>
                    <span>"Only you can see it"</span>
                </div>
                <div
                    class="drop-target"
                    class:over=move || over.get() == Some(true)
                    on:dragover=hover(true)
                    on:drop=drop_on(true)
                >
                    <strong>"Share with everyone"</strong>
                    <span>"Anyone here can add it to their Kobo"</span>
                </div>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_read_as_sentences_and_titles_stay_as_they_are() {
        let failed = Progress::Failed("this isn't an EPUB file".to_owned());
        assert_eq!(describe(&failed), "This isn't an EPUB file");
        let done = Progress::Done("Added émile & <b>".to_owned());
        assert_eq!(describe(&done), "Added émile & <b>");
        assert_eq!(sentence("émile"), "Émile");
        assert_eq!(sentence(""), "");
    }
}

#[cfg(feature = "hydrate")]
mod browser {
    use std::{cell::RefCell, collections::VecDeque};

    use leptos::{ev, prelude::*};
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};
    use web_sys::{File, FileList, FormData, HtmlInputElement, ProgressEvent, XmlHttpRequest};

    use super::{Item, Progress};

    /// The server's own limit per user. More would only be refused.
    const AT_ONCE: usize = 2;

    #[derive(Default)]
    struct Queue {
        waiting: VecDeque<(u32, File, bool)>,
        sending: usize,
        next_id: u32,
        /// Whether all of this batch were new books, so the list can go straight
        /// back to the library.
        all_added: bool,
    }

    thread_local! {
        static QUEUE: RefCell<Queue> = RefCell::default();
    }

    pub fn watch_page_drags(set_dragging: WriteSignal<bool>, set_over: WriteSignal<Option<bool>>) {
        let has_files = |event: &web_sys::DragEvent| {
            event
                .data_transfer()
                .is_some_and(|transfer| transfer.types().iter().any(|t| t == "Files"))
        };
        // Without this, a file dropped a little off target would replace the page.
        let handle = window_event_listener(ev::dragover, move |event| {
            if has_files(&event) {
                event.prevent_default();
                set_dragging.set(true);
            }
        });
        std::mem::forget(handle);
        let handle = window_event_listener(ev::drop, move |event| {
            event.prevent_default();
            set_dragging.set(false);
        });
        std::mem::forget(handle);
        let handle = window_event_listener(ev::dragleave, move |event| {
            // Only when leaving the window: moving between elements also fires this.
            if event.related_target().is_none() {
                set_dragging.set(false);
                set_over.set(None);
            }
        });
        std::mem::forget(handle);
    }

    fn files_of(list: Option<FileList>) -> Vec<File> {
        let Some(list) = list else {
            return Vec::new();
        };
        (0..list.length())
            .filter_map(|index| list.get(index))
            .collect()
    }

    pub fn dropped(event: &web_sys::DragEvent, shared: bool, items: RwSignal<Vec<Item>>) {
        let files = files_of(event.data_transfer().and_then(|transfer| transfer.files()));
        enqueue(files, shared, items);
    }

    pub fn chosen(event: &web_sys::Event, shared: bool, items: RwSignal<Vec<Item>>) {
        let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        enqueue(files_of(input.files()), shared, items);
        input.set_value("");
    }

    fn set(items: RwSignal<Vec<Item>>, id: u32, progress: Progress) {
        items.update(|items| {
            if let Some(item) = items.iter_mut().find(|item| item.id == id) {
                item.progress = progress;
            }
        });
    }

    fn enqueue(files: Vec<File>, shared: bool, items: RwSignal<Vec<Item>>) {
        for file in files {
            let name = file.name();
            let is_epub = name.to_lowercase().ends_with(".epub");
            let id = QUEUE.with_borrow_mut(|queue| {
                if queue.sending == 0 && queue.waiting.is_empty() {
                    queue.all_added = true;
                }
                queue.next_id += 1;
                let id = queue.next_id;
                if is_epub {
                    queue.waiting.push_back((id, file, shared));
                } else {
                    queue.all_added = false;
                }
                id
            });
            let progress = if is_epub {
                Progress::Waiting
            } else {
                Progress::Failed("Only EPUB files can be added".to_owned())
            };
            items.update(|items| items.push(Item { id, name, progress }));
        }
        pump(items);
    }

    fn pump(items: RwSignal<Vec<Item>>) {
        loop {
            let next = QUEUE.with_borrow_mut(|queue| {
                if queue.sending >= AT_ONCE {
                    return None;
                }
                let next = queue.waiting.pop_front();
                if next.is_some() {
                    queue.sending += 1;
                }
                next
            });
            let Some((id, file, shared)) = next else {
                return;
            };
            if send(id, &file, shared, items).is_err() {
                set(
                    items,
                    id,
                    Progress::Failed("Couldn't send this file".to_owned()),
                );
                finished(items, false);
            }
        }
    }

    fn finished(items: RwSignal<Vec<Item>>, added: bool) {
        let done = QUEUE.with_borrow_mut(|queue| {
            queue.sending = queue.sending.saturating_sub(1);
            queue.all_added &= added;
            (queue.sending == 0 && queue.waiting.is_empty()).then_some(queue.all_added)
        });
        // With nothing to read in the list, go and show the books.
        if done == Some(true)
            && let Some(window) = web_sys::window()
        {
            let _ = window.location().assign("/?done=uploaded");
        }
        pump(items);
    }

    /// `(what to say, whether it's a new book)`
    fn read_reply(xhr: &XmlHttpRequest) -> Result<(String, bool), String> {
        let status = xhr.status().unwrap_or(0);
        let body = xhr.response_text().ok().flatten().unwrap_or_default();
        let parsed: Option<serde_json::Value> = serde_json::from_str(&body).ok();
        let field = |name: &str| {
            parsed
                .as_ref()
                .and_then(|value| value.get(name))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        match status {
            200 | 201 => {
                let title = field("title").unwrap_or_default();
                match field("outcome").as_deref() {
                    Some("already_yours") => Ok((format!("You already have {title}"), false)),
                    Some("added_from_shared") => {
                        Ok((format!("Added {title} from the shared books"), true))
                    }
                    _ => Ok((format!("Added {title}"), true)),
                }
            }
            413 => Err(field("error")
                .unwrap_or_else(|| "That's larger than this server accepts".to_owned())),
            401 => Err("You've been logged out. Please reload this page.".to_owned()),
            _ => {
                Err(field("error")
                    .unwrap_or_else(|| "Something went wrong on the server".to_owned()))
            }
        }
    }

    fn send(id: u32, file: &File, shared: bool, items: RwSignal<Vec<Item>>) -> Result<(), JsValue> {
        let form = FormData::new()?;
        form.append_with_str("visibility", if shared { "shared" } else { "private" })?;
        form.append_with_blob_and_filename("file", file, &file.name())?;

        let xhr = XmlHttpRequest::new()?;
        xhr.open("POST", "/books/upload")?;
        xhr.set_request_header("Accept", "application/json")?;

        let on_progress = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
            if event.length_computable() && event.total() > 0.0 {
                let percent = (event.loaded() / event.total() * 100.0).clamp(0.0, 100.0) as u32;
                let progress = if percent >= 100 {
                    Progress::Reading
                } else {
                    Progress::Sending(percent)
                };
                set(items, id, progress);
            }
        });
        xhr.upload()?
            .set_onprogress(Some(on_progress.into_js_value().unchecked_ref()));

        let answered = xhr.clone();
        let on_load = Closure::<dyn FnMut()>::new(move || match read_reply(&answered) {
            Ok((text, added)) => {
                set(items, id, Progress::Done(text));
                finished(items, added);
            }
            Err(text) => {
                set(items, id, Progress::Failed(text));
                finished(items, false);
            }
        });
        xhr.set_onload(Some(on_load.into_js_value().unchecked_ref()));

        let on_error = Closure::<dyn FnMut()>::new(move || {
            set(
                items,
                id,
                Progress::Failed("The upload was interrupted. Please try again.".to_owned()),
            );
            finished(items, false);
        });
        xhr.set_onerror(Some(on_error.into_js_value().unchecked_ref()));

        set(items, id, Progress::Sending(0));
        xhr.send_with_opt_form_data(Some(&form))
    }
}
