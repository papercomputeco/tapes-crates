//! The tapes read contract — now a re-export of [`tapes_client`].
//!
//! # Why this crate still exists
//!
//! Everything that was here moved into `tapes-client`, which absorbed this
//! crate and the cassette client so the sealed and discovered halves of the
//! read surface stop being two implementations of one thing. The name survives
//! only so that consumers pinning `tapes-read-contract` keep compiling across
//! the move; there is no code here, and nothing new should be added.
//!
//! Consumers should depend on `tapes-client` directly. This shim is deleted
//! once they have.
//!
//! # What moved where
//!
//! | was | is |
//! | --- | -- |
//! | `contract` | [`tapes_client::core::contract`] |
//! | `coverage` | [`tapes_client::core::coverage`] |
//! | `invoke` | [`tapes_client::path`] |
//! | `error` | [`tapes_client::error`] |
//! | `transport` | [`tapes_client::transport`] and [`tapes_client::core::methods`] |
//!
//! The one item that did not survive the move is the `ReadTransport` /
//! `ReadOperations` pair. It was a second seam describing the same thing as the
//! cassette client's, which is precisely the duplication the merge exists to
//! remove; its replacement is [`tapes_client::transport::TapesTransport`], with
//! [`tapes_client::core::CoreClient`] as the call surface over it. Nothing
//! consumed the old pair, so nothing is re-exported under the old names rather
//! than a shim that would have to lie about the shape.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use tapes_client::core::{contract, coverage};
pub use tapes_client::error;
/// The URL builder, under the name this crate gave it.
pub use tapes_client::path as invoke;

pub use tapes_client::core::{
    CoreSurface, TAPES_API_YAML, call_for, call_for_with_body, core, ops,
};
pub use tapes_client::error::{Error, Result};
pub use tapes_client::path::{PathMode, call_url};

// Re-exported so a consumer naming a `Call` does not have to depend on another
// crate directly just to spell the type this one hands it.
pub use tapes_client::transport::Call;
