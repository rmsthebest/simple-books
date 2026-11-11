//! The simple-books reader's side of its sync with the server, over the
//! protocol in `sync-proto`. Blocking, so it runs on its own thread while the
//! reader carries on.
//!
//! - [`Client`] makes the requests.
//! - [`sync`] runs one sync against a [`State`] kept on disk.
//! - [`Paths`] says where the files are, and keeps the secret private.
//! - [`update`] updates the reader itself to the app the server has.
//!
//! The reader decides when to sync and what to do with the [`Report`]: apply
//! places from elsewhere, and look at its books again.

mod client;
mod run;
mod state;
pub mod update;

pub use client::{Client, Error};
pub use run::{Progress, Report, send, sync, unsent};
pub use state::{Paths, State, Synced};
pub use sync_proto::{Book, KoboFile, KoboFiles, PairStart, PairStatus, Spot, Status};
