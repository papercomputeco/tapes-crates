//! Stand the mock pair up and leave it running, for manual work.
//!
//! Everything this does is a library call. That is the point: the mock a person
//! debugs by hand and the mock the matrix drives in-process are the same code,
//! so a harness that works against one works against the other. A binary with
//! its own routing would be a second implementation to keep in step.
//!
//! ```text
//! cargo run -p tapes-mock-upstream
//! ```
//!
//! It prints the two base URLs and the environment a harness needs, then blocks
//! until interrupted, reporting each request as it arrives.

use std::time::Duration;

use tapes_mock_upstream::MockPair;

fn main() -> std::io::Result<()> {
    let pair = MockPair::start()?;

    println!("tapes-mock-upstream — internal test support, not a product");
    println!();
    println!("  upstream  {}", pair.upstream.base_url());
    println!("  ingest    {}", pair.ingest.base_url());
    println!();
    println!("Point a harness at the upstream, for example:");
    println!(
        "  ANTHROPIC_BASE_URL={} claude -p 'Reply with exactly: ok'",
        pair.upstream.base_url(),
    );
    println!();
    println!("Serving. Ctrl-C to stop.");

    // Report each new request rather than dumping the log repeatedly: a manual
    // session wants to see the harness's traffic as it happens.
    let mut upstream_seen = 0_usize;
    let mut ingest_seen = 0_usize;
    loop {
        let upstream_requests = pair.upstream.requests();
        for request in upstream_requests.iter().skip(upstream_seen) {
            println!("upstream  {} {}", request.method, request.target);
            for (name, value) in request.tapes_headers() {
                println!("            {name}: {value}");
            }
        }
        upstream_seen = upstream_requests.len();

        let turns = pair.ingest.landed_turns();
        for turn in turns.iter().skip(ingest_seen) {
            println!(
                "ingest    turn harness_id={} session_id={:?} via {:?}",
                turn.envelope.harness_id, turn.envelope.harness_session_id, turn.source,
            );
        }
        ingest_seen = turns.len();

        std::thread::sleep(Duration::from_millis(200));
    }
}
