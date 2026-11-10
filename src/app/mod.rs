//! The web interface. Pages render only on the server, so they work with JavaScript
//! off. The `#[island]` components are the only parts that reach the browser.
#![forbid(unsafe_code)]

pub mod connect_kobo;
pub mod cover_picker;
#[cfg(feature = "hydrate")]
mod drive;
pub mod install_kobo;
pub mod upload_zone;
pub mod watch;

#[cfg(feature = "ssr")]
mod pages;

#[cfg(feature = "ssr")]
pub use pages::{App, shell};
