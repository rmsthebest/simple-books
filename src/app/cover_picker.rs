//! The "change the cover" form. Choosing an image uploads it straight away. Without
//! JavaScript it's a plain form with its own button, so that still works.

use leptos::prelude::*;

#[island]
pub fn CoverPicker(action: String) -> impl IntoView {
    let (ready, set_ready) = signal(false);
    Effect::new(move |_| set_ready.set(true));

    let on_change = move |event: leptos::ev::Event| {
        #[cfg(feature = "hydrate")]
        {
            use wasm_bindgen::JsCast;
            let input = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok());
            let chosen = input
                .as_ref()
                .and_then(web_sys::HtmlInputElement::files)
                .is_some_and(|files| files.length() > 0);
            if let (true, Some(form)) = (chosen, input.and_then(|input| input.form())) {
                // requestSubmit, unlike submit, checks the form first, as a click would.
                if form.request_submit().is_err() {
                    set_ready.set(false);
                }
            }
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = event;
    };

    view! {
        <form class="inline" method="post" action=action enctype="multipart/form-data">
            <input type="file" name="file" accept="image/jpeg,image/png,image/webp" required
                aria-label="Choose a cover image" on:change=on_change />
            <button class="quiet" class:hidden=move || ready.get() type="submit">"Upload cover"</button>
        </form>
    }
}
