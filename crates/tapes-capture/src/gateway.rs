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

/// Environment variable declaring that the capture proxy serves each captured
/// provider on its own route, so an installed plugin must say which provider a
/// request belongs to.
///
/// Unset or empty means the proxy fronts a single upstream and a plugin
/// registers every captured provider at [`GATEWAY_URL_ENV`] unchanged. Any
/// other value means the plugin registers each provider at
/// [`provider_route`] instead.
///
/// It exists because a plugin may register more providers than a
/// single-upstream proxy can serve. pi's extension registers three, all at one
/// base URL: with one upstream behind it, a session on any provider other than
/// the one that upstream speaks is forwarded to a host that has never heard of
/// the route, and the harness fails outright. Which of those two shapes a proxy
/// is cannot be inferred from the address, so the launching client states it.
///
/// The default is the single-upstream shape on purpose: a client that predates
/// this variable sets nothing, and gets exactly the requests it got before.
pub const GATEWAY_PROVIDER_ROUTES_ENV: &str = "TAPES_GATEWAY_PROVIDER_ROUTES";

/// The value a client sets in [`GATEWAY_PROVIDER_ROUTES_ENV`] to ask for
/// per-provider routes.
///
/// A plugin treats *any* non-empty value as the request, so this is the
/// spelling to write rather than the only one accepted — one less way for a
/// client and an installed plugin to disagree.
pub const GATEWAY_PROVIDER_ROUTES_ON: &str = "1";

/// Path prefix under which a labelled request names its provider.
///
/// Underscore-led so it cannot collide with a provider API path: no upstream
/// schema this contract covers serves anything beneath `/_tapes`.
pub const GATEWAY_PROVIDER_ROUTE_PREFIX: &str = "/_tapes/provider";

/// The path a request for `provider` is sent to when per-provider routes are
/// on: [`GATEWAY_PROVIDER_ROUTE_PREFIX`], the provider name, then whatever path
/// the harness's client would have used on its own.
///
/// The prefix is a *base URL* suffix rather than a header because it has to
/// survive a harness client that composes its own paths — pi appends
/// `/v1/messages` to whatever base URL a provider was registered with, and
/// never consults a header the extension did not put on the request.
#[must_use]
pub fn provider_route(provider: &str) -> String {
    format!("{GATEWAY_PROVIDER_ROUTE_PREFIX}/{provider}")
}

/// Split a labelled request path into the provider it names and the path the
/// harness's client actually asked for.
///
/// `None` when the path carries no label at all, which a proxy must not read as
/// "any provider will do": it means the request came from something that does
/// not speak this half of the contract — an installed plugin older than the
/// launching client, most likely — and the provider it wanted is simply not
/// knowable from the request.
///
/// The remainder always begins with `/`, including for a bare
/// `/_tapes/provider/<name>`, so a caller can concatenate it onto an upstream
/// base without a second normalisation rule.
#[must_use]
pub fn split_provider_route(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(GATEWAY_PROVIDER_ROUTE_PREFIX)?;
    let rest = rest.strip_prefix('/')?;
    let end = rest.find('/').unwrap_or(rest.len());
    let (provider, remainder) = rest.split_at(end);
    if provider.is_empty() {
        return None;
    }
    Some((provider, if remainder.is_empty() { "/" } else { remainder }))
}

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

    /// A route this module builds is a route it takes apart again, for every
    /// path shape a harness client actually produces.
    #[test]
    fn a_built_route_round_trips_through_the_split() {
        for provider in ["anthropic", "openai", "openai-codex"] {
            for path in ["/v1/messages", "/v1/responses", "/v1/models?limit=1"] {
                let joined = format!("{}{path}", provider_route(provider));
                assert_eq!(
                    split_provider_route(&joined),
                    Some((provider, path)),
                    "round trip failed for {joined}"
                );
            }
        }
        // A client that appended nothing still names a provider, and the
        // remainder is a path rather than the empty string — so a caller can
        // concatenate it without a second rule for this case.
        assert_eq!(
            split_provider_route(&provider_route("anthropic")),
            Some(("anthropic", "/")),
        );
    }

    /// An unlabelled path is `None` rather than a guess. The proxy's whole
    /// reason for asking is that it cannot otherwise tell which upstream a
    /// request wants, and a default here would put the guess back.
    #[test]
    fn a_path_that_names_no_provider_resolves_to_nothing() {
        for path in [
            "/v1/messages",
            "/",
            // The prefix with no name after it names no provider.
            GATEWAY_PROVIDER_ROUTE_PREFIX,
            "/_tapes/provider/",
            // A *different* `_tapes` route is not a provider label; the proxy
            // serves its own paths under that namespace.
            "/_tapes/codex-app/lifecycle",
            // Prefix-of-a-longer-segment must not match: `providerX` is not
            // the provider route.
            "/_tapes/providerX/anthropic/v1/messages",
        ] {
            assert_eq!(
                split_provider_route(path),
                None,
                "{path} was read as labelled"
            );
        }
    }

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
