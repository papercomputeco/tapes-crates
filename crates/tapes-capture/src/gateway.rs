//! The capture-gateway environment contract and the launch-nonce protocol.
//!
//! This is the wire/environment agreement between a launching capture client
//! and whatever runs inside the harness on the other end: which variables name
//! the proxy, which variable carries the per-launch secret, which header echoes
//! it back, and what "the echo matched" means.
//!
//! It is protocol, not artifact. Adding another harness does not change any
//! constant here — a new in-harness extension is written *against* this
//! contract. That is why it lives beside the peer-trust check that consumes
//! it rather than beside the plugin files that happen to be its first readers.

/// Environment variable naming the capture-proxy base URL an installed plugin
/// should send the harness's LLM traffic to.
///
/// Accepts a bare `host:port` or a full URL; the asset normalises it the same
/// way a launch recipe's proxy endpoint does. Unset means "not captured", and
/// an installed plugin must then leave the harness's own endpoints alone.
pub const GATEWAY_URL_ENV: &str = "TAPES_GATEWAY_URL";

/// Environment variable naming which upstream provider schema the capture proxy
/// is currently fronting (e.g. `anthropic`, `openai`).
///
/// Optional, and a display/diagnostic hint only: a plugin may surface it and may
/// warn when the user picks a model the proxy is not routing, but it must not
/// gate the redirect on it. A proxy that fronts one schema at a time is one
/// deployment shape, not a requirement of the contract.
pub const GATEWAY_SCHEMA_ENV: &str = "TAPES_GATEWAY_SCHEMA";

/// Environment variable carrying the per-launch capture nonce.
///
/// A self-attributing harness's `X-Tapes-*` envelope is a claim, and the
/// ancestry check ([`crate::peer_trust`]) cannot tell the harness
/// apart from the harness's *own subprocesses* — a command run by a shell tool
/// is a descendant of the launched PID too, and could otherwise stamp another
/// session's envelope. The launching consumer (tapesctl, paperd) generates a
/// fresh secret per capture, sets it in this variable for the harness process,
/// and requires it echoed back before believing any envelope. The value must
/// never be logged, forwarded upstream, or included in captured output.
///
/// An installed plugin must read this variable **once at load and delete it
/// from its process environment immediately**, before any tool can run:
/// subprocesses the harness later spawns inherit the harness's *current*
/// environment, so the deletion keeps them from receiving the secret at all —
/// it survives only in the plugin's own memory. With that in place the
/// residual exposure is exactly two channels, and no more should be claimed:
/// a same-UID process reading the harness's *original* environment out of
/// `/proc/<pid>/environ` on Linux (that file snapshots the environment at
/// `exec` and does not reflect the deletion), and anything the harness itself
/// chooses to pass along explicitly.
///
/// Unset means the launching client predates the nonce contract; an installed
/// plugin must then simply not send the header rather than fail.
pub const GATEWAY_NONCE_ENV: &str = "TAPES_GATEWAY_NONCE";

/// Request header in which an installed plugin echoes the capture nonce back
/// to the proxy that launched it.
///
/// Lower-case for the same reason the `X-Tapes-*` envelope names are: HTTP/2
/// lowercases header names on the wire, so the canonical spelling is the wire
/// spelling. The header is a private channel between the extension and its own
/// capture proxy — the proxy validates it against the value it generated and
/// **strips it before forwarding**, so the nonce never reaches an upstream and
/// never appears in a captured turn.
pub const GATEWAY_NONCE_HEADER: &str = "x-tapes-gateway-nonce";

/// Does a presented nonce match the one this capture generated?
///
/// Shared so both consumers enforce the same rule. Two properties matter:
///
/// * **An empty expectation never matches.** A consumer that failed to
///   generate a nonce must fail closed, not accept an empty echo.
/// * **The comparison is constant-time in the matching prefix.** The caller is
///   a loopback listener reachable by every local process; a byte-at-a-time
///   `==` would let one probe the secret through response timing. Length still
///   leaks, and may: nonce lengths are not secret.
#[must_use]
pub fn nonce_matches(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    if expected.is_empty() || expected.len() != presented.len() {
        return false;
    }
    expected
        .iter()
        .zip(presented)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Fail closed on an unset expectation, and match only the exact echo.
    #[test]
    fn nonce_matching_is_exact_and_never_matches_an_empty_expectation() {
        assert!(nonce_matches("abc123", "abc123"));
        assert!(!nonce_matches("abc123", "abc124"));
        assert!(!nonce_matches("abc123", "abc12"));
        assert!(!nonce_matches("abc123", ""));
        // A consumer that never generated a nonce must not accept an empty
        // echo as a match — that would turn a misconfiguration into a bypass.
        assert!(!nonce_matches("", ""));
        assert!(!nonce_matches("", "abc123"));
    }
}
