//! Keeps a page up to date while the server finishes something it shows, such as a
//! book being got ready for the Kobo, or a Hardcover sign-in. The page includes this
//! only while something is under way. It checks `/status` now and then, and once
//! that has changed, it reloads the page, unless someone is typing or uploading.

use leptos::prelude::*;

/// Seconds to wait before check number `check`: often at first, when things
/// usually finish, then less often. `None` once it's time to give up.
#[must_use]
pub fn delay_before(check: u32) -> Option<u64> {
    match check {
        0..40 => Some(3),    // the first two minutes
        40..140 => Some(15), // then 25 minutes more
        _ => None,
    }
}

/// The address to reload, without a `#fragment`, since going to the same address
/// with only a fragment would scroll instead of reloading.
#[must_use]
pub fn without_fragment(href: &str) -> &str {
    href.split_once('#').map_or(href, |(page, _)| page)
}

/// `current` is what `/status` said when the page was made.
#[island]
pub fn Watch(current: String) -> impl IntoView {
    #[cfg(feature = "hydrate")]
    {
        let current = std::rc::Rc::new(current);
        Effect::new(move |_| browser::check_after(std::rc::Rc::clone(&current), 0));
    }
    // On the server it's only passed along to the browser.
    #[cfg(not(feature = "hydrate"))]
    drop(current);
}

#[cfg(feature = "hydrate")]
mod browser {
    use std::{rc::Rc, time::Duration};

    use wasm_bindgen::{JsCast, closure::Closure};
    use web_sys::{HtmlInputElement, HtmlTextAreaElement, XmlHttpRequest};

    pub fn check_after(current: Rc<String>, check: u32) {
        let Some(seconds) = super::delay_before(check) else {
            return;
        };
        leptos::prelude::set_timeout(
            move || check_now(current, check),
            Duration::from_secs(seconds),
        );
    }

    fn check_now(current: Rc<String>, check: u32) {
        let Ok(xhr) = XmlHttpRequest::new() else {
            return;
        };
        if xhr.open("GET", "/status").is_err() {
            return;
        }
        let answered = xhr.clone();
        let again = Rc::clone(&current);
        let on_load = Closure::<dyn FnMut()>::new(move || {
            match answered.status() {
                Ok(200) => {}
                // Logged out: there's nothing more this page can learn.
                Ok(401) => return,
                _ => return check_after(Rc::clone(&current), check + 1),
            }
            let now = answered.response_text().ok().flatten().unwrap_or_default();
            if now == *current {
                check_after(Rc::clone(&current), check + 1);
            } else if busy() {
                // Reload once they've finished, if that's before it gives up.
                check_after(Rc::clone(&current), check + 1);
            } else {
                reload();
            }
        });
        let on_error =
            Closure::<dyn FnMut()>::new(move || check_after(Rc::clone(&again), check + 1));
        xhr.set_onload(Some(on_load.into_js_value().unchecked_ref()));
        xhr.set_onerror(Some(on_error.into_js_value().unchecked_ref()));
        // If it can't even be sent, there's no point trying again.
        let _ = xhr.send();
    }

    /// Whether a reload now would lose something: an upload under way or showing
    /// its results, or a form with changes not yet saved.
    fn busy() -> bool {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return true;
        };
        if document
            .query_selector("[data-busy='true']")
            .ok()
            .flatten()
            .is_some()
        {
            return true;
        }
        let Ok(fields) = document.query_selector_all("input, textarea") else {
            return true;
        };
        (0..fields.length())
            .filter_map(|at| fields.item(at))
            .any(|node| {
                if let Some(input) = node.dyn_ref::<HtmlInputElement>() {
                    match input.type_().as_str() {
                        "file" => input.files().is_some_and(|files| files.length() > 0),
                        "checkbox" | "radio" => input.checked() != input.default_checked(),
                        "hidden" | "submit" | "button" => false,
                        _ => input.value() != input.default_value(),
                    }
                } else if let Some(area) = node.dyn_ref::<HtmlTextAreaElement>() {
                    area.default_value()
                        .is_ok_and(|default| area.value() != default)
                } else {
                    false
                }
            })
    }

    fn reload() {
        let Some(location) = web_sys::window().map(|window| window.location()) else {
            return;
        };
        if let Ok(href) = location.href() {
            // A plain visit, so a page that answered a form is never sent again.
            let _ = location.replace(super::without_fragment(&href));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_often_then_less_then_stops() {
        assert_eq!(delay_before(0), Some(3));
        assert_eq!(delay_before(39), Some(3));
        assert_eq!(delay_before(40), Some(15));
        assert_eq!(delay_before(139), Some(15));
        assert_eq!(delay_before(140), None);
        let total: u64 = (0..).map_while(delay_before).sum();
        assert_eq!(total, 2 * 60 + 25 * 60);
    }

    #[test]
    fn reloads_the_page_not_a_fragment() {
        assert_eq!(
            without_fragment("https://x.y/books/1#top"),
            "https://x.y/books/1"
        );
        assert_eq!(
            without_fragment("https://x.y/?done=uploaded"),
            "https://x.y/?done=uploaded"
        );
    }
}
