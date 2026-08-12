//! The mock ingest: what a captured turn is posted to, and the envelope reader
//! that decides what that turn was attributed to.
//!
//! # The reader is the corpus's, not ours
//!
//! [`read_envelope`] is not a convenience parser written to satisfy these
//! tests. It is checked, case by case, against the same vendored fixture corpus
//! the producer side is checked against — see `tests/envelope_corpus_reader.rs`.
//! That matters because the matrix's central assertion is *launched implies
//! attributed*, and "attributed" is only meaningful if this side reads the
//! envelope the way every other reader does. A mock with a lenient parser of
//! its own would report attribution that ingest, in the real system, would
//! reject — a green matrix over a broken composition, which is the exact
//! failure this whole exercise exists to catch.
//!
//! # Two places a turn's attribution can live
//!
//! A capture client posts a turn as JSON with a `session` object carrying the
//! attribution, and stamps the `X-Tapes-*` envelope on the *upstream* request.
//! A self-attributing harness's plugin, by contrast, puts the envelope in
//! headers. [`LandedTurn`] reads both and says which it found, so an assertion
//! can be specific about the path it is exercising rather than accidentally
//! passing on the other one.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use crate::http::{Handler, MockServer, Request, Response};

/// The path a captured turn is posted to.
pub const PATH_INGEST: &str = "/v1/ingest";

/// The path a transcript push is posted to.
pub const PATH_INGEST_TRANSCRIPT: &str = "/v1/ingest/transcript";

/// The harness-id sentinel for a turn nothing could attribute.
pub const HARNESS_ID_UNKNOWN: &str = "unknown";

/// An envelope as a reader recovers it from headers.
///
/// Field-for-field the modelled half of the fixture corpus's `envelope` object.
/// The server-trusted `x-paper-auth-*` headers are deliberately absent: they are
/// not a producer's to emit and not a client's to be believed about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// The harness that produced the turn, or [`HARNESS_ID_UNKNOWN`].
    pub harness_id: String,
    /// The harness-side session id, when one was claimed.
    pub harness_session_id: Option<String>,
    /// The harness's version string.
    pub harness_version: Option<String>,
    /// The harness's working directory, percent-decoded.
    pub cwd: Option<String>,
    /// The user-given session name, percent-decoded.
    pub name: Option<String>,
    /// The fork-parent's session id.
    pub parent_harness_session_id: Option<String>,
    /// Free-form harness metadata, base64url-decoded to JSON.
    pub harness_metadata: Option<serde_json::Value>,
}

impl Envelope {
    /// Was this turn attributed to a real harness session?
    ///
    /// Both halves are required on purpose. A harness id without a session id
    /// is the partial-envelope shape a forged or half-configured claim
    /// produces, and treating it as attributed would let precisely the case the
    /// matrix is meant to catch pass.
    #[must_use]
    pub fn is_attributed(&self) -> bool {
        self.harness_id != HARNESS_ID_UNKNOWN
            && self.harness_id.is_empty().eq(&false)
            && self.harness_session_id.is_some()
    }
}

/// Read an envelope out of a header map.
///
/// The rules are the corpus's, and each is there because a case pins it:
///
/// * a missing or empty `x-tapes-harness-id` reads as [`HARNESS_ID_UNKNOWN`]
///   (`unknown-missing-harness-id`, `unknown-empty-harness-id`);
/// * `cwd` and `session-name` are percent-decoded, and a value whose escapes
///   are malformed is kept verbatim rather than rejected
///   (`cwd-unicode`, `cwd-literal-plus`, `cwd-malformed-percent-encoding`);
/// * a decoded `cwd` or `session-name` holding any C0 control byte or DEL is
///   **refused outright** — the field lands absent rather than carrying the raw
///   bytes (`cwd-control-bytes-escaped`, `session-name-control-bytes-escaped`).
///   Percent-encoding stops a newline from forging a second header on the wire;
///   this stops the decoded newline from reaching storage. The guard is a
///   property of the decoder rather than of one field, which is why it is
///   applied by [`refuse_control_bytes`] to both;
/// * metadata is base64url **no-pad**; padding or non-base64 drops the field
///   without harming the rest of the envelope (`error-metadata-padded-base64`,
///   `error-metadata-invalid-base64`);
/// * metadata that decodes to a non-object is *kept* — refusing it is an ingest
///   policy decision, not a reader one (`error-metadata-not-object`);
/// * an empty header is an absent header (`error-parent-empty`).
#[must_use]
pub fn read_envelope(headers: &BTreeMap<String, String>) -> Envelope {
    use tapes_capture::envelope as wire;

    let field = |name: &str| -> Option<String> {
        headers
            .get(name)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let decoded = |name: &str| -> Option<String> {
        field(name)
            .map(|raw| {
                percent_encoding::percent_decode_str(&raw)
                    .decode_utf8()
                    .map_or(raw.clone(), |decoded| decoded.into_owned())
            })
            .and_then(refuse_control_bytes)
    };

    Envelope {
        harness_id: field(wire::X_TAPES_HARNESS_ID)
            .unwrap_or_else(|| HARNESS_ID_UNKNOWN.to_owned()),
        harness_session_id: field(wire::X_TAPES_HARNESS_SESSION_ID),
        harness_version: field(wire::X_TAPES_HARNESS_VERSION),
        cwd: decoded(wire::X_TAPES_CWD),
        name: decoded(wire::X_TAPES_SESSION_NAME),
        parent_harness_session_id: field(wire::X_TAPES_PARENT_HARNESS_SESSION_ID),
        harness_metadata: field(wire::X_TAPES_HARNESS_METADATA).and_then(|raw| {
            URL_SAFE_NO_PAD
                .decode(raw.as_bytes())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        }),
    }
}

/// Drop a decoded free-text field that carries a C0 control byte or DEL.
///
/// The two defences are layered and neither replaces the other. Percent-encoding
/// keeps a newline from forging a second header while the value is in transit;
/// this keeps the newline out of storage once the encoding has been undone.
/// Refusing the whole value rather than stripping the offending bytes is
/// deliberate — a stripped value is a plausible-looking path or name that nobody
/// supplied, and the corpus pins the empty result.
#[must_use]
pub fn refuse_control_bytes(value: String) -> Option<String> {
    let clean = !value
        .chars()
        .any(|c| (c as u32) < 0x20 || (c as u32) == 0x7F);
    clean.then_some(value)
}

/// Where a landed turn's attribution was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributionSource {
    /// From the posted turn's `session` object — how a capture client reports.
    TurnBody,
    /// From `X-Tapes-*` request headers — how a self-attributing harness's
    /// plugin reports.
    Headers,
    /// Neither carried attribution.
    None,
}

/// One turn as the mock ingest received it.
#[derive(Debug, Clone)]
pub struct LandedTurn {
    /// The request the turn arrived on, in full.
    pub request: Request,
    /// The attribution the turn carried, from whichever place carried it.
    pub envelope: Envelope,
    /// Which place that was.
    pub source: AttributionSource,
}

impl LandedTurn {
    /// Read a turn out of a request.
    fn from_request(request: &Request) -> Self {
        let header_envelope = read_envelope(&request.tapes_headers());

        // The body's `session` object wins when it carries a harness id: it is
        // the capture client's considered answer, produced *after* the ancestry
        // and nonce checks, whereas headers on this hop are whatever was sent.
        let body_envelope = request
            .json()
            .and_then(|body| body.get("session").cloned())
            .and_then(|session| envelope_from_session(&session));

        match body_envelope {
            Some(envelope) => Self {
                request: request.clone(),
                envelope,
                source: AttributionSource::TurnBody,
            },
            None if header_envelope.harness_id != HARNESS_ID_UNKNOWN => Self {
                request: request.clone(),
                envelope: header_envelope,
                source: AttributionSource::Headers,
            },
            None => Self {
                request: request.clone(),
                envelope: header_envelope,
                source: AttributionSource::None,
            },
        }
    }
}

/// Build an envelope from a posted turn's `session` object.
///
/// `None` when the object names no harness at all, so the caller can fall
/// through to the header path rather than reading a bare `unknown` as an answer.
fn envelope_from_session(session: &serde_json::Value) -> Option<Envelope> {
    let string = |key: &str| {
        session
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let harness_id = string("harness_id")?;
    Some(Envelope {
        harness_id,
        harness_session_id: string("harness_session_id"),
        harness_version: string("harness_version"),
        cwd: string("cwd"),
        name: string("name"),
        parent_harness_session_id: string("parent_harness_session_id"),
        harness_metadata: session.get("harness_metadata").cloned(),
    })
}

/// How the mock ingest should answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IngestPolicy {
    /// Accept every turn with `202`.
    #[default]
    AcceptAll,
    /// Accept unattributed turns and refuse attributed ones with `500`.
    ///
    /// Stages the drain-on-failure path: the client must notice its turn did
    /// not land rather than reporting a capture it never completed.
    RefuseAttributed,
}

/// A running mock ingest.
#[derive(Debug)]
pub struct MockIngest {
    server: MockServer,
}

impl MockIngest {
    /// Start a mock ingest that accepts everything.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the socket cannot be bound.
    pub fn start() -> std::io::Result<Self> {
        Self::with_policy(IngestPolicy::AcceptAll)
    }

    /// Start a mock ingest answering according to `policy`.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the socket cannot be bound.
    pub fn with_policy(policy: IngestPolicy) -> std::io::Result<Self> {
        let handler: Handler = Arc::new(move |request: &Request| respond(request, policy));
        Ok(Self {
            server: MockServer::start(handler)?,
        })
    }

    /// The base URL a capture client should post turns to, no trailing slash.
    #[must_use]
    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    /// The underlying server, for raw request inspection.
    #[must_use]
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// Every turn that landed, oldest first.
    #[must_use]
    pub fn landed_turns(&self) -> Vec<LandedTurn> {
        self.server
            .requests()
            .iter()
            .filter(|request| request.method == "POST" && request.path.ends_with(PATH_INGEST))
            .map(LandedTurn::from_request)
            .collect()
    }

    /// Every landed turn that carried a real session attribution.
    #[must_use]
    pub fn attributed_turns(&self) -> Vec<LandedTurn> {
        self.landed_turns()
            .into_iter()
            .filter(|turn| turn.envelope.is_attributed())
            .collect()
    }

    /// Block until at least one turn has landed, or `timeout` elapses.
    pub fn wait_for_turn(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !self.landed_turns().is_empty() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Answer one ingest request.
fn respond(request: &Request, policy: IngestPolicy) -> Response {
    if request.method != "POST" {
        return Response::json(
            404,
            &json!({ "error": "tapes-mock-upstream ingest accepts POST only" }),
        );
    }

    if request.path.ends_with(PATH_INGEST_TRANSCRIPT) {
        return Response::json(
            202,
            &json!({ "status": "accepted", "deduped": false, "records": 0 }),
        );
    }

    if !request.path.ends_with(PATH_INGEST) {
        return Response::json(
            404,
            &json!({ "error": format!("tapes-mock-upstream ingest does not serve {}", request.path) }),
        );
    }

    let turn = LandedTurn::from_request(request);
    if policy == IngestPolicy::RefuseAttributed && turn.envelope.is_attributed() {
        return Response::json(
            500,
            &json!({ "error": "ingest refuses attributed turns under this policy" }),
        );
    }

    Response::json(202, &json!({ "status": "accepted" }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn post(ingest: &MockIngest, path: &str, extra: &[(&str, &str)], body: &str) -> String {
        let addr = ingest.server().addr();
        let mut stream = TcpStream::connect(addr).unwrap();
        let mut request = format!(
            "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
            body.len(),
        );
        for (name, value) in extra {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// The attribution predicate needs both halves. A harness id with no
    /// session id is the partial-envelope shape a forged claim produces.
    #[test]
    fn a_harness_id_without_a_session_id_is_not_attributed() {
        let partial = read_envelope(&headers(&[("x-tapes-harness-id", "pi")]));
        assert_eq!(partial.harness_id, "pi");
        assert!(!partial.is_attributed());

        let complete = read_envelope(&headers(&[
            ("x-tapes-harness-id", "pi"),
            ("x-tapes-harness-session-id", "sid-1"),
        ]));
        assert!(complete.is_attributed());
    }

    /// A turn with no envelope at all reads as the unknown sentinel, never as
    /// an empty harness id.
    #[test]
    fn a_bare_turn_reads_as_unknown() {
        let envelope = read_envelope(&BTreeMap::new());
        assert_eq!(envelope.harness_id, HARNESS_ID_UNKNOWN);
        assert!(!envelope.is_attributed());
    }

    /// The body's `session` object is preferred over headers, and the source
    /// says so — a test asserting the client path must not pass because a
    /// header happened to be present.
    #[test]
    fn the_turn_body_wins_over_headers_and_names_its_source() {
        let ingest = MockIngest::start().unwrap();
        let raw = post(
            &ingest,
            PATH_INGEST,
            &[
                ("x-tapes-harness-id", "pi"),
                ("x-tapes-harness-session-id", "from-headers"),
            ],
            r#"{"session":{"harness_id":"claude","harness_session_id":"from-body"}}"#,
        );
        assert!(raw.starts_with("HTTP/1.1 202"));

        assert!(ingest.wait_for_turn(Duration::from_secs(5)));
        let turns = ingest.landed_turns();
        let turn = turns.first().expect("one turn landed");
        assert_eq!(turn.source, AttributionSource::TurnBody);
        assert_eq!(turn.envelope.harness_id, "claude");
        assert_eq!(
            turn.envelope.harness_session_id.as_deref(),
            Some("from-body"),
        );
    }

    /// With no body attribution, the headers are read instead — the
    /// self-attributing plugin's path.
    #[test]
    fn headers_are_read_when_the_body_names_no_harness() {
        let ingest = MockIngest::start().unwrap();
        post(
            &ingest,
            PATH_INGEST,
            &[
                ("x-tapes-harness-id", "opencode"),
                ("x-tapes-harness-session-id", "sid-9"),
            ],
            r#"{"raw_request":"e30="}"#,
        );

        assert!(ingest.wait_for_turn(Duration::from_secs(5)));
        let turns = ingest.landed_turns();
        let turn = turns.first().expect("one turn landed");
        assert_eq!(turn.source, AttributionSource::Headers);
        assert_eq!(turn.envelope.harness_id, "opencode");
        assert_eq!(ingest.attributed_turns().len(), 1);
    }

    /// The refusal policy answers 500 for an attributed turn and 202 for an
    /// unattributed one, so a drain-on-failure path can be staged.
    #[test]
    fn the_refusal_policy_only_refuses_attributed_turns() {
        let ingest = MockIngest::with_policy(IngestPolicy::RefuseAttributed).unwrap();

        let refused = post(
            &ingest,
            PATH_INGEST,
            &[],
            r#"{"session":{"harness_id":"claude","harness_session_id":"sid"}}"#,
        );
        assert!(refused.starts_with("HTTP/1.1 500"));

        let accepted = post(&ingest, PATH_INGEST, &[], r#"{"session":{}}"#);
        assert!(accepted.starts_with("HTTP/1.1 202"));
    }

    /// A transcript push is accepted and is not counted as a turn.
    #[test]
    fn a_transcript_push_is_not_a_turn() {
        let ingest = MockIngest::start().unwrap();
        let raw = post(&ingest, PATH_INGEST_TRANSCRIPT, &[], r#"{"records":[]}"#);
        assert!(raw.starts_with("HTTP/1.1 202"));

        assert!(ingest.server().wait_for_requests(1, Duration::from_secs(5)));
        assert!(ingest.landed_turns().is_empty());
    }

    /// A decoded newline in a free-text field is refused outright — the
    /// header-injection guard the corpus pins. Refusing rather than stripping
    /// matters: a stripped value is a plausible path nobody supplied.
    #[test]
    fn a_decoded_control_byte_refuses_the_whole_value() {
        let envelope = read_envelope(&headers(&[
            ("x-tapes-harness-id", "claude"),
            ("x-tapes-harness-session-id", "sid-1"),
            ("x-tapes-cwd", "/Users/matt%0Awith-injection:%20yes"),
            ("x-tapes-session-name", "release%0Ax-evil:%20yes"),
        ]));
        assert_eq!(envelope.cwd, None);
        assert_eq!(envelope.name, None);
        // The rest of the envelope survives the refusal.
        assert!(envelope.is_attributed());
    }

    /// The guard covers DEL as well as the C0 range, and leaves ordinary
    /// non-ASCII text alone.
    #[test]
    fn the_control_byte_guard_covers_del_but_not_unicode() {
        assert_eq!(
            refuse_control_bytes("/Users/松本/code".to_owned()).as_deref(),
            Some("/Users/松本/code")
        );
        assert_eq!(refuse_control_bytes("a\u{7F}b".to_owned()), None);
        assert_eq!(refuse_control_bytes("a\tb".to_owned()), None);
    }

    /// Malformed metadata drops only itself. The rest of the envelope must
    /// survive, or one bad field would look like a total attribution failure.
    #[test]
    fn malformed_metadata_drops_only_itself() {
        let envelope = read_envelope(&headers(&[
            ("x-tapes-harness-id", "claude"),
            ("x-tapes-harness-session-id", "sid-1"),
            ("x-tapes-harness-metadata", "!!! not base64url !!!"),
        ]));
        assert!(envelope.harness_metadata.is_none());
        assert!(envelope.is_attributed());
    }
}
