//! A very small blocking HTTP/1.1 server, and the request/response types the
//! mock upstream and mock ingest are both written against.
//!
//! # Why this is hand-written
//!
//! The mocks this replaces were `wiremock::ResponseTemplate::set_body_string`
//! with a `text/event-stream` content-type bolted on: the whole "stream"
//! arrived in one write, already complete, before the client read a byte. That
//! is enough to satisfy a proxy that tees bytes without looking at them, and it
//! is *not* enough to satisfy a real harness binary, which is the thing this
//! matrix exists to launch. A buffered body cannot exercise incremental parse,
//! cannot show that a client tolerates an event split across reads, and cannot
//! distinguish a mock that ends its stream properly from one that merely stops
//! talking.
//!
//! Getting per-event flushes back means owning the socket writes, so this owns
//! the socket writes. Everything else here — the request parser, the chunked
//! encoder — is the minimum needed to make that legal HTTP/1.1.
//!
//! # What it deliberately does not do
//!
//! No TLS, no HTTP/2, no keep-alive pipelining, no request routing beyond what
//! a handler closure does for itself. Harnesses reach a capture proxy over
//! plaintext loopback HTTP/1.1, so that is the whole surface that needs to
//! exist. A mock that grew TLS would be a mock nobody could debug with `curl`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long a connection may sit idle mid-request before the server gives up on
/// it. Bounded so a wedged harness cannot hold a server thread for a whole CI
/// run; generous enough that a slow debug-build client is not cut off.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the accept loop wakes to notice a shutdown request. The listener
/// is non-blocking, so this is purely the idle poll interval — small enough
/// that `drop` returns promptly, large enough not to spin a core.
const ACCEPT_POLL: Duration = Duration::from_millis(5);

/// One request as the server parsed it.
///
/// Header names are lower-cased on the way in. That is not a normalisation
/// convenience: HTTP/2 lower-cases on the wire and the `X-Tapes-*` contract is
/// written in lower case for exactly that reason, so a mock that preserved the
/// sender's casing would let a test pass against a spelling the real contract
/// does not use.
#[derive(Debug, Clone)]
pub struct Request {
    /// The request method, upper-cased (`GET`, `POST`, …).
    pub method: String,
    /// The request target, including any query string.
    pub target: String,
    /// The path portion of [`Self::target`], query string removed.
    pub path: String,
    /// Every header, lower-cased name to value. A repeated header keeps the
    /// last value — no header in this contract is legally repeated.
    pub headers: BTreeMap<String, String>,
    /// The request body, exactly as it arrived.
    pub body: Vec<u8>,
}

impl Request {
    /// The value of `name`, which must already be lower-case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// The body parsed as JSON, or `None` when it is absent or not JSON.
    #[must_use]
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// Every header whose name carries the `X-Tapes-*` prefix.
    ///
    /// This is the envelope as it arrived, before any interpretation — the
    /// ingest validator and the "did the nonce leak upstream" assertion both
    /// start here.
    #[must_use]
    pub fn tapes_headers(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .filter(|(name, _)| name.starts_with(tapes_capture::envelope::HEADER_PREFIX))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// One server-sent event, as bytes on the wire.
///
/// Kept as a named event plus a data payload rather than as a formatted string
/// so a recipe cannot accidentally write an event that is missing its blank-line
/// terminator — [`SseEvent::encode`] is the only place that framing is spelled.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// The `event:` name. `None` emits a data-only event, which is what the
    /// OpenAI chat-completions stream uses.
    pub event: Option<String>,
    /// The `data:` payload. Written verbatim, so a caller controls whether it
    /// is JSON, `[DONE]`, or anything else.
    pub data: String,
}

impl SseEvent {
    /// A named event carrying a JSON payload.
    #[must_use]
    pub fn named(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
        }
    }

    /// A data-only event, with no `event:` line.
    #[must_use]
    pub fn data_only(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
        }
    }

    /// The event's bytes, framing and all: an optional `event:` line, a `data:`
    /// line, and the blank line that terminates the event.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::new();
        if let Some(name) = &self.event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        // A multi-line payload becomes one `data:` line per line, which is what
        // the SSE spec requires and what a real provider emits for pretty JSON.
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out.into_bytes()
    }
}

/// What a handler wants written back.
#[derive(Debug, Clone)]
pub enum Body {
    /// A complete body, written in one go with a `Content-Length`.
    Bytes(Vec<u8>),
    /// A stream of events, each flushed as its own chunk.
    ///
    /// This is the variant that justifies the file. The events are written with
    /// `Transfer-Encoding: chunked`, one chunk per event, with a flush between
    /// each — so a client genuinely observes the stream arriving in pieces.
    Sse(Vec<SseEvent>),
}

/// A response a handler returns.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Response headers. `Content-Length` / `Transfer-Encoding` are supplied by
    /// the writer and must not be set here.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

impl Response {
    /// A JSON response with the given status.
    #[must_use]
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Body::Bytes(value.to_string().into_bytes()),
        }
    }

    /// A plain-text response with the given status.
    #[must_use]
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
            body: Body::Bytes(body.into().into_bytes()),
        }
    }

    /// A streaming `text/event-stream` response.
    #[must_use]
    pub fn sse(events: Vec<SseEvent>) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("content-type".to_owned(), "text/event-stream".to_owned()),
                ("cache-control".to_owned(), "no-cache".to_owned()),
            ],
            body: Body::Sse(events),
        }
    }
}

/// What a handler is: a request in, a response out.
pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

/// A running mock server on loopback.
///
/// Dropping it stops the accept loop and joins the accept thread, so a test
/// that lets one go out of scope releases the port. The ad-hoc mocks this
/// replaces leaked their server task deliberately and paid for it with a
/// documented cross-test port-stealing bug; a server whose lifetime is its
/// value's lifetime does not have that failure mode.
#[derive(Debug)]
pub struct MockServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    log: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    /// Bind an ephemeral loopback port and start serving `handler`.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the socket cannot be bound or put
    /// into non-blocking mode.
    pub fn start(handler: Handler) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let log: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));

        let accept = {
            let shutdown = Arc::clone(&shutdown);
            let log = Arc::clone(&log);
            std::thread::spawn(move || accept_loop(&listener, &handler, &log, &shutdown))
        };

        Ok(Self {
            addr,
            shutdown,
            accept: Some(accept),
            log,
        })
    }

    /// The address the server is listening on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The server's base URL, with no trailing slash.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request the server has finished handling, oldest first.
    ///
    /// Returns an empty vector if the log mutex was poisoned by a panicking
    /// handler — a poisoned log is a test-support failure, and reporting "no
    /// requests" lets the assertion that was going to run fail with its own
    /// message rather than being replaced by an unwrap panic here.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().map(|log| log.clone()).unwrap_or_default()
    }

    /// Block until at least `count` requests have been recorded, or `timeout`
    /// elapses. Returns whether the count was reached.
    ///
    /// Polling rather than signalling keeps the server free of a condvar it
    /// would otherwise need only for tests; the interval is short enough that
    /// the wait does not show up in a run's wall clock.
    pub fn wait_for_requests(&self, count: usize, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.requests().len() >= count {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.accept.take() {
            // The accept loop polls, so it observes the flag within
            // `ACCEPT_POLL` without needing to be woken by a dummy connection.
            let _ = handle.join();
        }
    }
}

/// Accept connections until `shutdown` is set or the listener dies.
fn accept_loop(
    listener: &TcpListener,
    handler: &Handler,
    log: &Arc<Mutex<Vec<Request>>>,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::Relaxed) {
        if !accept_once(listener, handler, log) {
            break;
        }
    }
}

/// Take one turn of the accept loop.
///
/// Returns whether the loop should continue: a would-block is the ordinary idle
/// case and keeps going after a short sleep, while any other error means the
/// listener is gone and there is nothing left to accept.
fn accept_once(listener: &TcpListener, handler: &Handler, log: &Arc<Mutex<Vec<Request>>>) -> bool {
    match listener.accept() {
        Ok((stream, _peer)) => {
            let handler = Arc::clone(handler);
            let log = Arc::clone(log);
            // Detached on purpose: a connection outlives at most one request,
            // and the accept loop must not block on a slow client.
            std::thread::spawn(move || serve_connection(stream, &handler, &log));
            true
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            std::thread::sleep(ACCEPT_POLL);
            true
        }
        Err(_) => false,
    }
}

/// Read one request, hand it to `handler`, write the response, record it.
fn serve_connection(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>) {
    // The listener is non-blocking so the accept loop can poll for shutdown, and
    // on BSD-derived platforms — macOS included — `accept` hands back a socket
    // that has *inherited* that flag. POSIX leaves the accepted socket's status
    // flags unspecified and Linux happens to clear them, so a server that omits
    // this line reads fine on Linux and drops connections on macOS. Restore
    // blocking mode explicitly rather than relying on either behaviour.
    //
    // The read timeout below is not a substitute: `SO_RCVTIMEO` only bounds a
    // *blocking* read, so on an inherited non-blocking socket every read returns
    // `WouldBlock` immediately instead of waiting for the client's bytes.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_nodelay(true);

    let Some(request) = read_request(&stream) else {
        return;
    };

    let response = handler(&request);
    let _ = write_response(&mut stream, &response);
    let _ = stream.flush();
    // Half-close so a client reading to EOF sees the stream end rather than
    // waiting out its own timeout.
    let _ = stream.shutdown(Shutdown::Write);

    if let Ok(mut log) = log.lock() {
        log.push(request);
    }
}

/// Parse a request line, headers, and body from `stream`.
///
/// `None` for anything malformed: a mock that guessed at a broken request would
/// turn a harness's protocol bug into a passing test.
fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let target = parts.next()?.to_owned();

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let body = read_body(&mut reader, &headers)?;

    let path = target
        .split_once('?')
        .map_or(target.as_str(), |(path, _)| path)
        .to_owned();

    Some(Request {
        method,
        target,
        path,
        headers,
        body,
    })
}

/// Read the body according to `Content-Length` or `Transfer-Encoding: chunked`.
///
/// Both shapes are supported because harness HTTP clients differ: most send a
/// length for a JSON POST, but a client streaming a request body sends chunks,
/// and a mock that only understood one would appear to hang for the other.
fn read_body(
    reader: &mut BufReader<&TcpStream>,
    headers: &BTreeMap<String, String>,
) -> Option<Vec<u8>> {
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));

    if chunked {
        let mut body = Vec::new();
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).ok()? == 0 {
                break;
            }
            let size_line = size_line.trim();
            // A chunk extension (`;name=value`) is legal and ignorable.
            let size_hex = size_line.split(';').next().unwrap_or(size_line);
            let size = usize::from_str_radix(size_hex, 16).ok()?;
            if size == 0 {
                // Consume the trailer section's terminating blank line.
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer);
                break;
            }
            let mut chunk = vec![0_u8; size];
            reader.read_exact(&mut chunk).ok()?;
            body.extend_from_slice(&chunk);
            // Each chunk is followed by CRLF.
            let mut crlf = [0_u8; 2];
            reader.read_exact(&mut crlf).ok()?;
        }
        return Some(body);
    }

    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(body)
}

/// Write the status line, headers, and body.
fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let reason = reason_phrase(response.status);
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }

    match &response.body {
        Body::Bytes(bytes) => {
            head.push_str(&format!("content-length: {}\r\n", bytes.len()));
            head.push_str("connection: close\r\n\r\n");
            stream.write_all(head.as_bytes())?;
            stream.write_all(bytes)?;
            stream.flush()?;
        }
        Body::Sse(events) => {
            head.push_str("transfer-encoding: chunked\r\n");
            head.push_str("connection: close\r\n\r\n");
            stream.write_all(head.as_bytes())?;
            stream.flush()?;

            // One chunk per event, flushed. This is the whole point: a client
            // reading this socket observes events arriving separately, exactly
            // as it would against a real provider.
            for event in events {
                write_chunk(stream, &event.encode())?;
                stream.flush()?;
            }
            // The terminating zero-length chunk.
            stream.write_all(b"0\r\n\r\n")?;
            stream.flush()?;
        }
    }
    Ok(())
}

/// Write one chunked-encoding chunk.
fn write_chunk(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(format!("{:x}\r\n", payload.len()).as_bytes())?;
    stream.write_all(payload)?;
    stream.write_all(b"\r\n")?;
    Ok(())
}

/// A reason phrase for the statuses these mocks return. Not exhaustive — an
/// unknown status gets a generic phrase, which every client ignores anyway.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The framing an SSE event puts on the wire, terminator included. This is
    /// the byte-level contract a real harness parses, so it is pinned here
    /// rather than left to the shape of whatever recipe happens to call it.
    #[test]
    fn a_named_event_encodes_with_its_blank_line_terminator() {
        let event = SseEvent::named("message_stop", r#"{"type":"message_stop"}"#);
        assert_eq!(
            String::from_utf8(event.encode()).unwrap(),
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
    }

    /// A data-only event omits the `event:` line entirely — the shape the
    /// OpenAI chat-completions stream uses, including for its `[DONE]`
    /// sentinel.
    #[test]
    fn a_data_only_event_omits_the_event_line() {
        assert_eq!(
            String::from_utf8(SseEvent::data_only("[DONE]").encode()).unwrap(),
            "data: [DONE]\n\n",
        );
    }

    /// A payload containing newlines becomes one `data:` line per line. A mock
    /// that emitted a raw embedded newline would terminate the event early and
    /// hand the client a truncated JSON document.
    #[test]
    fn a_multi_line_payload_becomes_one_data_line_each() {
        assert_eq!(
            String::from_utf8(SseEvent::data_only("{\n  \"a\": 1\n}").encode()).unwrap(),
            "data: {\ndata:   \"a\": 1\ndata: }\n\n",
        );
    }

    /// Header names are lower-cased on the way in, because the `X-Tapes-*`
    /// contract is written in the wire spelling HTTP/2 forces.
    #[test]
    fn header_names_are_lowercased_and_tapes_headers_are_selectable() {
        let server = MockServer::start(Arc::new(|_req: &Request| Response::text(200, "ok")))
            .expect("the mock server binds a loopback port");
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(
                b"POST /v1/messages HTTP/1.1\r\n\
                  Host: localhost\r\n\
                  X-Tapes-Harness-Id: claude\r\n\
                  Content-Length: 2\r\n\r\n{}",
            )
            .unwrap();
        stream.flush().unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);

        assert!(server.wait_for_requests(1, Duration::from_secs(5)));
        let seen = server.requests();
        let request = seen.first().expect("one request was recorded");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(request.header("x-tapes-harness-id"), Some("claude"));
        assert_eq!(request.tapes_headers().len(), 1);
        assert_eq!(request.body, b"{}");
    }

    /// A query string stays on `target` and is stripped from `path`, so a
    /// handler can route on the path without re-parsing.
    #[test]
    fn the_query_string_is_split_off_the_path() {
        let server = MockServer::start(Arc::new(|_req: &Request| Response::text(200, "ok")))
            .expect("the mock server binds a loopback port");
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(b"GET /v1/models?limit=1 HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);

        assert!(server.wait_for_requests(1, Duration::from_secs(5)));
        let seen = server.requests();
        let request = seen.first().expect("one request was recorded");
        assert_eq!(request.path, "/v1/models");
        assert_eq!(request.target, "/v1/models?limit=1");
    }

    /// A chunked request body is reassembled. Harness HTTP clients differ on
    /// whether they send a length or stream the body, and a mock that only
    /// understood one would appear to hang for the other.
    #[test]
    fn a_chunked_request_body_is_reassembled() {
        let server = MockServer::start(Arc::new(|_req: &Request| Response::text(200, "ok")))
            .expect("the mock server binds a loopback port");
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(
                b"POST /v1/messages HTTP/1.1\r\n\
                  Host: x\r\n\
                  Transfer-Encoding: chunked\r\n\r\n\
                  5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
            )
            .unwrap();
        stream.flush().unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);

        assert!(server.wait_for_requests(1, Duration::from_secs(5)));
        let seen = server.requests();
        assert_eq!(
            seen.first().expect("one request was recorded").body,
            b"hello world",
        );
    }

    /// The SSE path really does chunk: each event is its own chunk on the wire,
    /// which is the property the buffered mocks could not provide.
    #[test]
    fn an_sse_response_writes_one_chunk_per_event() {
        let server = MockServer::start(Arc::new(|_req: &Request| {
            Response::sse(vec![SseEvent::named("a", "1"), SseEvent::named("b", "2")])
        }))
        .expect("the mock server binds a loopback port");

        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let raw = String::from_utf8(raw).unwrap();

        assert!(raw.contains("transfer-encoding: chunked"));
        assert!(raw.contains("content-type: text/event-stream"));
        // Two events, two chunk headers of their exact sizes, then the
        // terminating zero chunk.
        let first = SseEvent::named("a", "1").encode();
        assert!(raw.contains(&format!("{:x}\r\nevent: a\ndata: 1\n\n\r\n", first.len())));
        assert!(raw.ends_with("0\r\n\r\n"));
    }

    /// A client that connects and only then sends its request is still served.
    ///
    /// This is the interleaving that made every socket test in this crate flaky
    /// on macOS CI: the accept loop's listener is non-blocking, BSD-derived
    /// `accept` hands that flag to the accepted socket, and the connection's
    /// first read then failed with `WouldBlock` instead of waiting — which
    /// [`read_request`] reads as "malformed" and answers by closing without a
    /// byte written. The client saw an empty response, or a reset if its request
    /// had landed in the receive queue by the time the server closed.
    ///
    /// The delay below is what makes the test deterministic rather than what
    /// makes it pass: it holds the first byte back until the accept has
    /// certainly happened (`ACCEPT_POLL` is 5ms), so the server is forced
    /// through the ordering that used to lose the request. Remove the
    /// `set_nonblocking(false)` in `serve_connection` and this fails every run.
    #[test]
    fn a_request_sent_after_the_accept_is_still_served() {
        let server = MockServer::start(Arc::new(|_req: &Request| Response::text(200, "ok")))
            .expect("the mock server binds a loopback port");
        let mut stream = TcpStream::connect(server.addr()).unwrap();

        std::thread::sleep(Duration::from_millis(200));

        stream
            .write_all(b"GET /late HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();

        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.starts_with("HTTP/1.1 200"), "empty or partial: {raw:?}");
        assert!(raw.ends_with("ok"));
        assert!(server.wait_for_requests(1, Duration::from_secs(5)));
    }

    /// Dropping the server frees the port, so a later test can bind again. The
    /// mocks this replaces leaked their server task and paid for it with a
    /// documented cross-test port-stealing bug.
    #[test]
    fn dropping_the_server_releases_its_port() {
        let addr = {
            let server = MockServer::start(Arc::new(|_req: &Request| Response::text(200, "ok")))
                .expect("the mock server binds a loopback port");
            server.addr()
        };
        // The accept loop is joined by `drop`, so the listener is closed by the
        // time we get here and the address is bindable again.
        let rebound = TcpListener::bind(addr);
        assert!(rebound.is_ok(), "the port should be free after drop");
    }
}
