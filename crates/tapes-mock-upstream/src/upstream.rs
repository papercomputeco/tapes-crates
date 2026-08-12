//! The mock provider upstream: one server that speaks enough of every provider
//! surface the registry's harnesses reach for to complete a single scripted
//! turn.
//!
//! # One mock, several surfaces
//!
//! The harnesses do not agree on a provider API. Claude Code speaks Anthropic
//! Messages; codex speaks OpenAI Responses (at two different paths depending on
//! whether it is on an API key or a ChatGPT plan); opencode and pi speak
//! whichever provider they were pointed at, which in practice means both of the
//! above plus OpenAI chat-completions. Rather than a mock per harness — which
//! is how the ad-hoc versions of this ended up duplicated and subtly divergent
//! — this is one server that routes on path, so any harness can be pointed at
//! one base URL and get a coherent answer.
//!
//! That matters beyond tidiness. The matrix asserts things about *composition*
//! — that a launched harness's turn is attributed, that the nonce did not leak
//! upstream — and those assertions read the requests the upstream received. One
//! recorder over all surfaces means one place to ask "what actually arrived",
//! whichever harness produced it.
//!
//! # Streaming is the point
//!
//! Every streaming surface here emits a full, well-formed event sequence with
//! the real event names and payload types, flushed one event at a time. The
//! mocks this consolidates emitted a three-event Anthropic fragment whose
//! `content_block_delta` had no `type` on its delta and no `index` on the event
//! — fine for a proxy that tees bytes, rejected by a real client. A mock a real
//! harness cannot parse cannot support a matrix whose whole premise is
//! launching real harnesses.

use std::sync::Arc;

use serde_json::json;

use crate::http::{Handler, MockServer, Request, Response, SseEvent};

/// The assistant text every scripted turn produces.
///
/// Short and constant on purpose: the matrix asserts on plumbing, not on model
/// behaviour, and a fixed reply keeps a turn's byte count stable enough that a
/// transcript assertion can be exact.
pub const SCRIPTED_REPLY: &str = "ok";

/// The model name the mock reports in its responses.
pub const SCRIPTED_MODEL: &str = "mock-upstream-1";

/// Anthropic's streaming Messages surface — what Claude Code appends to
/// `ANTHROPIC_BASE_URL`, and one of the two surfaces opencode and pi can be
/// pointed at.
pub const PATH_ANTHROPIC_MESSAGES: &str = "/v1/messages";

/// OpenAI's Responses surface — codex on an API key, and codex-app.
pub const PATH_OPENAI_RESPONSES: &str = "/v1/responses";

/// The ChatGPT-plan Codex backend's Responses surface. Same event shapes as
/// [`PATH_OPENAI_RESPONSES`], different path, which is exactly the kind of
/// difference a per-harness mock would get wrong in only one copy.
pub const PATH_CODEX_BACKEND_RESPONSES: &str = "/backend-api/codex/responses";

/// The path pi's `openai-codex` provider appends to whatever base URL it was
/// registered with.
///
/// A third spelling of the Responses surface, and the reason this mock routes on
/// path suffix rather than on an exact table: pi registers three providers at
/// one base URL and each composes its own path, so the set of paths one harness
/// can reach is not knowable from the harness's name alone.
pub const PATH_PI_CODEX_RESPONSES: &str = "/codex/responses";

/// OpenAI's chat-completions surface, for harnesses configured against it.
pub const PATH_OPENAI_CHAT_COMPLETIONS: &str = "/v1/chat/completions";

/// A running mock upstream.
#[derive(Debug)]
pub struct MockUpstream {
    server: MockServer,
}

impl MockUpstream {
    /// Start a mock upstream on an ephemeral loopback port.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the socket cannot be bound.
    pub fn start() -> std::io::Result<Self> {
        let handler: Handler = Arc::new(route);
        Ok(Self {
            server: MockServer::start(handler)?,
        })
    }

    /// The base URL a harness (or a capture proxy in front of one) should be
    /// pointed at. No trailing slash, because every harness appends its own
    /// path.
    #[must_use]
    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    /// The underlying server, for request inspection.
    #[must_use]
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// Every request the upstream received, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.server.requests()
    }

    /// The requests that were an actual model call, as opposed to a capability
    /// probe like `GET /v1/models` — which several harnesses make at startup
    /// and which would otherwise be miscounted as the turn.
    #[must_use]
    pub fn turn_requests(&self) -> Vec<Request> {
        self.server
            .requests()
            .into_iter()
            .filter(|request| is_turn_path(&request.path) && request.method == "POST")
            .collect()
    }

    /// Block until at least one model call has arrived, or `timeout` elapses.
    pub fn wait_for_turn(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if !self.turn_requests().is_empty() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Is this path one of the model-call surfaces, rather than a probe?
#[must_use]
pub fn is_turn_path(path: &str) -> bool {
    matches!(
        path,
        PATH_ANTHROPIC_MESSAGES
            | PATH_OPENAI_RESPONSES
            | PATH_CODEX_BACKEND_RESPONSES
            | PATH_OPENAI_CHAT_COMPLETIONS
    ) || path.ends_with(PATH_CODEX_BACKEND_RESPONSES)
        // A capture proxy that serves each provider on its own route prefixes
        // the harness's path; match the suffix so a labelled request still
        // reaches the right surface.
        || path.ends_with(PATH_ANTHROPIC_MESSAGES)
        || path.ends_with(PATH_OPENAI_RESPONSES)
        || path.ends_with(PATH_PI_CODEX_RESPONSES)
        || path.ends_with(PATH_OPENAI_CHAT_COMPLETIONS)
}

/// Route a request to the surface its path names.
fn route(request: &Request) -> Response {
    let path = request.path.as_str();
    let streaming = wants_stream(request);

    if request.method == "GET" && path.ends_with("/v1/models") {
        return Response::json(
            200,
            &json!({
                "object": "list",
                "data": [{
                    "id": SCRIPTED_MODEL,
                    "object": "model",
                    "created": 0,
                    "owned_by": "tapes-mock-upstream",
                }],
            }),
        );
    }

    if path.ends_with(PATH_ANTHROPIC_MESSAGES) {
        // `count_tokens` is a sibling of the messages path and is not a turn.
        if path.ends_with("/count_tokens") {
            return Response::json(200, &json!({ "input_tokens": 8 }));
        }
        return if streaming {
            Response::sse(anthropic_stream())
        } else {
            Response::json(200, &anthropic_complete())
        };
    }

    if path.ends_with(PATH_OPENAI_RESPONSES)
        || path.ends_with(PATH_CODEX_BACKEND_RESPONSES)
        || path.ends_with(PATH_PI_CODEX_RESPONSES)
    {
        return if streaming {
            Response::sse(openai_responses_stream())
        } else {
            Response::json(200, &openai_responses_complete())
        };
    }

    if path.ends_with(PATH_OPENAI_CHAT_COMPLETIONS) {
        return if streaming {
            Response::sse(openai_chat_stream())
        } else {
            Response::json(200, &openai_chat_complete())
        };
    }

    Response::json(
        404,
        &json!({
            "error": {
                "type": "not_found",
                "message": format!("tapes-mock-upstream does not serve {path}"),
            }
        }),
    )
}

/// Did the caller ask for a stream?
///
/// Read from the request body rather than assumed, because the same path serves
/// both: a harness that asks for a buffered response and receives SSE will fail
/// to parse it, and vice versa. Codex's Responses calls set `"stream": true`
/// the same way Anthropic's do.
fn wants_stream(request: &Request) -> bool {
    request
        .json()
        .and_then(|body| body.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// The Anthropic Messages streaming sequence, in full.
///
/// Every event a real turn emits, with the real `type` discriminants — including
/// the `content_block_start` / `content_block_stop` pair and the `message_delta`
/// carrying `stop_reason`, all of which the ad-hoc mocks omitted and a real
/// client requires.
fn anthropic_stream() -> Vec<SseEvent> {
    let message_id = "msg_mock_upstream_0001";
    vec![
        SseEvent::named(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": SCRIPTED_MODEL,
                    "content": [],
                    "stop_reason": serde_json::Value::Null,
                    "stop_sequence": serde_json::Value::Null,
                    "usage": { "input_tokens": 8, "output_tokens": 1 },
                },
            })
            .to_string(),
        ),
        SseEvent::named(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" },
            })
            .to_string(),
        ),
        // A real stream interleaves keepalives; a client that mishandles one
        // should fail here rather than in production.
        SseEvent::named("ping", json!({ "type": "ping" }).to_string()),
        SseEvent::named(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": SCRIPTED_REPLY },
            })
            .to_string(),
        ),
        SseEvent::named(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        SseEvent::named(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": serde_json::Value::Null },
                "usage": { "output_tokens": 1 },
            })
            .to_string(),
        ),
        SseEvent::named(
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]
}

/// The buffered Anthropic Messages response.
fn anthropic_complete() -> serde_json::Value {
    json!({
        "id": "msg_mock_upstream_0001",
        "type": "message",
        "role": "assistant",
        "model": SCRIPTED_MODEL,
        "content": [{ "type": "text", "text": SCRIPTED_REPLY }],
        "stop_reason": "end_turn",
        "stop_sequence": serde_json::Value::Null,
        "usage": { "input_tokens": 8, "output_tokens": 1 },
    })
}

/// The OpenAI Responses streaming sequence.
///
/// `sequence_number` increments across the whole stream, which some clients
/// check for gaps — so it is generated rather than hard-coded per event.
fn openai_responses_stream() -> Vec<SseEvent> {
    let response_id = "resp_mock_upstream_0001";
    let item_id = "msg_mock_upstream_item_0001";
    let in_progress = |status: &str| {
        json!({
            "id": response_id,
            "object": "response",
            "created_at": 0,
            "status": status,
            "model": SCRIPTED_MODEL,
            "output": [],
        })
    };

    let mut sequence = 0_u64;
    let mut next = |event: &str, mut body: serde_json::Value| {
        if let Some(object) = body.as_object_mut() {
            object.insert("sequence_number".to_owned(), json!(sequence));
        }
        sequence += 1;
        SseEvent::named(event, body.to_string())
    };

    vec![
        next(
            "response.created",
            json!({ "type": "response.created", "response": in_progress("in_progress") }),
        ),
        next(
            "response.in_progress",
            json!({ "type": "response.in_progress", "response": in_progress("in_progress") }),
        ),
        next(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        ),
        next(
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] },
            }),
        ),
        next(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "delta": SCRIPTED_REPLY,
            }),
        ),
        next(
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "text": SCRIPTED_REPLY,
            }),
        ),
        next(
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": SCRIPTED_REPLY,
                    "annotations": [],
                },
            }),
        ),
        next(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": SCRIPTED_REPLY,
                        "annotations": [],
                    }],
                },
            }),
        ),
        next(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": openai_responses_complete(),
            }),
        ),
    ]
}

/// The buffered OpenAI Responses body, and the `response` payload the
/// `response.completed` event carries.
fn openai_responses_complete() -> serde_json::Value {
    json!({
        "id": "resp_mock_upstream_0001",
        "object": "response",
        "created_at": 0,
        "status": "completed",
        "model": SCRIPTED_MODEL,
        "output": [{
            "id": "msg_mock_upstream_item_0001",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": SCRIPTED_REPLY,
                "annotations": [],
            }],
        }],
        "usage": {
            "input_tokens": 8,
            "output_tokens": 1,
            "total_tokens": 9,
        },
    })
}

/// The OpenAI chat-completions streaming sequence, terminated by the `[DONE]`
/// sentinel a client waits for.
fn openai_chat_stream() -> Vec<SseEvent> {
    let chunk = |delta: serde_json::Value, finish: serde_json::Value| {
        SseEvent::data_only(
            json!({
                "id": "chatcmpl_mock_upstream_0001",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": SCRIPTED_MODEL,
                "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
            })
            .to_string(),
        )
    };

    vec![
        chunk(
            json!({ "role": "assistant", "content": "" }),
            serde_json::Value::Null,
        ),
        chunk(
            json!({ "content": SCRIPTED_REPLY }),
            serde_json::Value::Null,
        ),
        chunk(json!({}), json!("stop")),
        SseEvent::data_only("[DONE]"),
    ]
}

/// The buffered chat-completions body.
fn openai_chat_complete() -> serde_json::Value {
    json!({
        "id": "chatcmpl_mock_upstream_0001",
        "object": "chat.completion",
        "created": 0,
        "model": SCRIPTED_MODEL,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": SCRIPTED_REPLY },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 1, "total_tokens": 9 },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    /// Drive one raw request against the mock and return the whole response.
    fn call(upstream: &MockUpstream, method: &str, path: &str, body: &str) -> String {
        let addr = upstream.server().addr();
        let mut stream = TcpStream::connect(addr).unwrap();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len(),
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// The Anthropic stream carries the full event set a real client needs —
    /// not the three-event fragment the ad-hoc mocks emitted.
    #[test]
    fn the_anthropic_stream_is_a_complete_well_typed_sequence() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            PATH_ANTHROPIC_MESSAGES,
            r#"{"model":"m","stream":true}"#,
        );

        for event in [
            "message_start",
            "content_block_start",
            "ping",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ] {
            assert!(raw.contains(&format!("event: {event}")), "missing {event}");
        }
        // The delta a real client parses is typed and indexed.
        assert!(raw.contains(r#""type":"text_delta""#));
        assert!(raw.contains(&format!(r#""text":"{SCRIPTED_REPLY}""#)));
        assert!(raw.contains("transfer-encoding: chunked"));
    }

    /// The same path serves a buffered response when the caller did not ask to
    /// stream. A harness that gets the wrong one fails to parse.
    #[test]
    fn a_non_streaming_request_gets_a_buffered_body() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            PATH_ANTHROPIC_MESSAGES,
            r#"{"model":"m"}"#,
        );
        assert!(raw.contains("content-type: application/json"));
        assert!(!raw.contains("text/event-stream"));
        assert!(raw.contains(r#""stop_reason":"end_turn""#));
    }

    /// The Responses stream numbers its events consecutively from zero.
    #[test]
    fn the_responses_stream_numbers_its_events_consecutively() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            PATH_OPENAI_RESPONSES,
            r#"{"model":"m","stream":true}"#,
        );
        for (index, event) in [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
        .iter()
        .enumerate()
        {
            assert!(raw.contains(&format!("event: {event}")), "missing {event}");
            assert!(
                raw.contains(&format!(r#""sequence_number":{index}"#)),
                "missing sequence_number {index}",
            );
        }
    }

    /// The ChatGPT-plan Codex backend path gets the same Responses shapes as
    /// the API-key path. Two paths, one implementation — the divergence the
    /// per-harness mocks had.
    #[test]
    fn the_codex_backend_path_serves_the_same_responses_shapes() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            PATH_CODEX_BACKEND_RESPONSES,
            r#"{"model":"m","stream":true}"#,
        );
        assert!(raw.contains("event: response.completed"));
        assert!(raw.contains(&format!(r#""delta":"{SCRIPTED_REPLY}""#)));
    }

    /// Chat-completions ends with the `[DONE]` sentinel; a client that waits
    /// for it would hang without one.
    #[test]
    fn the_chat_completions_stream_ends_with_done() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            PATH_OPENAI_CHAT_COMPLETIONS,
            r#"{"model":"m","stream":true}"#,
        );
        assert!(raw.contains(r#""object":"chat.completion.chunk""#));
        assert!(raw.contains(r#""finish_reason":"stop""#));
        assert!(raw.contains("data: [DONE]"));
    }

    /// A capture proxy serving each provider on its own route prefixes the
    /// harness's path. The mock must still find the surface, or a
    /// provider-routed matrix cell would 404 for reasons that have nothing to
    /// do with the harness.
    #[test]
    fn a_provider_prefixed_path_still_reaches_its_surface() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(
            &upstream,
            "POST",
            "/_tapes/provider/anthropic/v1/messages",
            r#"{"model":"m","stream":true}"#,
        );
        assert!(raw.contains("event: message_stop"));
    }

    /// A probe is recorded but is not a turn, so a harness's startup `GET
    /// /v1/models` cannot be miscounted as the model call the matrix waits for.
    #[test]
    fn a_models_probe_is_recorded_but_is_not_a_turn() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(&upstream, "GET", "/v1/models", "");
        assert!(raw.contains(SCRIPTED_MODEL));

        assert!(
            upstream
                .server()
                .wait_for_requests(1, Duration::from_secs(5))
        );
        assert_eq!(upstream.requests().len(), 1);
        assert!(upstream.turn_requests().is_empty());
    }

    /// An unserved path is a clean 404 with a message naming the path, so a
    /// recipe pointed at the wrong surface says so instead of hanging.
    #[test]
    fn an_unknown_path_is_a_named_404() {
        let upstream = MockUpstream::start().unwrap();
        let raw = call(&upstream, "POST", "/v1/nope", "{}");
        assert!(raw.starts_with("HTTP/1.1 404"));
        assert!(raw.contains("/v1/nope"));
    }
}
