//! The tapes cassette client — now a re-export of [`tapes_client`].
//!
//! # Why this crate still exists
//!
//! Everything that was here moved into `tapes-client`, which absorbed this
//! crate and the read contract so the discovered and sealed halves of the read
//! surface stop being two implementations of one thing. The name survives only
//! so that consumers pinning `tapes-cassette-client` keep compiling across the
//! move; there is no machinery here, and nothing new should be added.
//!
//! Consumers should depend on `tapes-client` directly. This shim is deleted
//! once they have.
//!
//! # What moved where
//!
//! | was | is |
//! | --- | -- |
//! | `cache` | [`tapes_client::cassettes::cache`] |
//! | `command` | [`tapes_client::cli`] |
//! | `discovery` | [`tapes_client::cassettes::discovery`] |
//! | `spec` | [`tapes_client::cassettes::spec`] |
//! | `invoke` | [`tapes_client::path`] and [`tapes_client::cassettes::invoke`] |
//! | `transport` | [`tapes_client::transport`] and [`tapes_client::http`] |

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;

pub use tapes_client::cassettes::{cache, discovery, spec};
/// The clap surfaces, under the name this crate gave them.
pub use tapes_client::cli as command;

pub use tapes_client::cassettes::{CacheConfig, Discovery, DiscoveryEntry};
pub use tapes_client::cassettes::{Cassette, Location, Method, Param, ReducerConfig, Surface};
pub use tapes_client::http::DirectHttp;
pub use tapes_client::transport::{Call, SpecFetch, SpecTransport};

pub use error::{Error, Result};

/// Building a call's URL, under the name and arity this crate gave it.
///
/// The merged crate's builder takes a [`tapes_client::PathMode`], because a
/// client mounted under a gateway prefix and one addressed at a server's root
/// are not the same join. This crate only ever performed the root-absolute one,
/// so that is what is preserved here.
pub mod invoke {
    pub use tapes_client::transport::Call;

    /// Build the URL for one described call against a base.
    pub fn call_url(base: &url::Url, call: &Call<'_>) -> tapes_client::Result<url::Url> {
        tapes_client::path::call_url(base, call, tapes_client::PathMode::Direct)
    }
}

/// The transport seam, under the module name this crate gave it.
pub mod transport {
    pub use tapes_client::http::DirectHttp;
    pub use tapes_client::transport::{SpecFetch, SpecTransport};
}
