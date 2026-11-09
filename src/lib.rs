//! simple-books: a minimal self-hosted ebook server with Kobo sync.
// Leptos views are deeply nested types.
#![recursion_limit = "512"]

pub mod app;

#[cfg(feature = "ssr")]
pub mod server;

/// Entry point for the WASM bundle. Only `#[island]` components hydrate.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_islands();
}
