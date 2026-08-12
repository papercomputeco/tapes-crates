//! A stand-in harness, for testing the matrix runner itself.
//!
//! The runner has failure modes of its own, and they are the expensive kind:
//! they make a cell *look* green. A harness that sent its request and then died
//! is not a working composition, and a runner that stalls on a harness chatty
//! enough to fill a pipe reports a timeout that says nothing about the harness.
//! Neither can be exercised with a real harness binary — a real one is not
//! installed on every machine, and neither behaviour is one you can ask it for.
//!
//! This binary can be asked for both, on demand and in a millisecond, so the
//! runner's own regressions have somewhere to be caught. It models nothing about
//! how a real harness behaves and is not a mock harness in any general sense.
//! Two modes, both trivial:
//!
//! ```text
//! matrix-fake-harness --turn-then-exit <code>  one model call, then exit <code>
//! matrix-fake-harness --flood <bytes>          <bytes> to each of stdout and
//!                                              stderr, then exit 0
//! ```
//!
//! The modes are found by scanning argv rather than by reading position zero:
//! a one-shot recipe puts the wrapped launch plan's own flags in front of the
//! recipe's argv, so the mode is not reliably first and a positional parse would
//! break the moment a recipe grew a config flag.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::process::ExitCode;

use tapes_harnesses::launch::ANTHROPIC_BASE_URL_ENV;
use tapes_mock_upstream::upstream::PATH_ANTHROPIC_MESSAGES;

/// How much is written per `write_all`, in each direction.
const CHUNK: usize = 4096;

/// Exit code for a usage error — distinct from the codes a mode is asked to
/// exit with, so a test cannot mistake "the fake was misdriven" for "the fake
/// did what it was told".
const USAGE: u8 = 64;

/// Exit code for a mode that was driven correctly and still could not do its
/// job, chiefly a turn that never reached the upstream.
const CANNOT: u8 = 65;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--version") {
        println!("matrix-fake-harness 1");
        return ExitCode::SUCCESS;
    }
    if let Some(code) = value_after("--turn-then-exit", &args) {
        return turn_then_exit(code);
    }
    if let Some(bytes) = value_after("--flood", &args) {
        return flood(bytes);
    }

    eprintln!("matrix-fake-harness: no mode in {args:?}");
    ExitCode::from(USAGE)
}

/// The argument following `flag`, when `flag` is present.
fn value_after<'a>(flag: &str, args: &'a [String]) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).map(String::as_str)
}

/// Send one model call to the upstream the launch plan pointed at, then exit
/// with the code asked for.
///
/// The base URL is read from the same environment variable the real Claude
/// launch recipe sets, so this mode exercises the recipe's pointing rather than
/// a private agreement with the test.
fn turn_then_exit(code: &str) -> ExitCode {
    let Ok(code) = code.parse::<u8>() else {
        eprintln!("matrix-fake-harness: --turn-then-exit needs an exit code");
        return ExitCode::from(USAGE);
    };
    let Ok(base) = std::env::var(ANTHROPIC_BASE_URL_ENV) else {
        eprintln!("matrix-fake-harness: {ANTHROPIC_BASE_URL_ENV} is unset");
        return ExitCode::from(USAGE);
    };

    match post_turn(&base) {
        Ok(()) => {
            println!("matrix-fake-harness: turn sent, exiting {code}");
            ExitCode::from(code)
        }
        Err(err) => {
            eprintln!("matrix-fake-harness: {err}");
            ExitCode::from(CANNOT)
        }
    }
}

/// POST one non-streaming Messages request and read the answer to EOF.
///
/// Hand-rolled over `TcpStream` because this crate deliberately has no HTTP
/// client dependency — the server is hand-written for the same reason — and one
/// request against a loopback mock does not justify acquiring one.
fn post_turn(base: &str) -> Result<(), String> {
    let authority = base
        .trim_end_matches('/')
        .trim_start_matches("http://")
        .to_owned();
    let body = r#"{"model":"mock","max_tokens":16,"messages":[{"role":"user","content":"ok"}]}"#;
    let request = format!(
        "POST {PATH_ANTHROPIC_MESSAGES} HTTP/1.1\r\nhost: {authority}\r\ncontent-type: \
         application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );

    let mut stream =
        TcpStream::connect(&authority).map_err(|err| format!("connect {authority}: {err}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("send: {err}"))?;
    stream.flush().map_err(|err| format!("flush: {err}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|err| format!("read: {err}"))?;
    let response = String::from_utf8_lossy(&response);
    if response.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(format!("upstream answered {:?}", response.lines().next()))
    }
}

/// Write `bytes` to stdout *and* `bytes` to stderr, interleaved, then exit 0.
///
/// Interleaved rather than one stream after the other so the test is a
/// regression test for both pipes: a runner that drained only one would still
/// stall here, which is the point.
fn flood(bytes: &str) -> ExitCode {
    let Ok(total) = bytes.parse::<usize>() else {
        eprintln!("matrix-fake-harness: --flood needs a byte count");
        return ExitCode::from(USAGE);
    };

    let filler = vec![b'x'; CHUNK];
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let mut written = 0;
    while written < total {
        let take = CHUNK.min(total - written);
        if out.write_all(&filler[..take]).is_err() || err.write_all(&filler[..take]).is_err() {
            // Nowhere left to report this: the streams are what failed.
            return ExitCode::from(CANNOT);
        }
        written += take;
    }
    if out.flush().is_err() || err.flush().is_err() {
        return ExitCode::from(CANNOT);
    }
    ExitCode::SUCCESS
}
