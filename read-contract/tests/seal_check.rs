//! Negative controls for the contract seal check.
//!
//! The seal's job is to answer "are the vendored bytes the published bytes?".
//! Its most dangerous failure is not answering "no" — it is answering "yes"
//! when it never looked, which is what a skipped gate that exits 0 does. These
//! tests drive the script into exactly that situation and assert it blocks in
//! CI and, when it does skip, says out loud that it verified nothing.
//!
//! Both cases run without network: the release base is pointed at a closed
//! port, so the fetch fails immediately, and the checkout fallback is pointed
//! at an empty directory so a tapes checkout sitting beside the repository
//! (which is normal in a development grove) cannot rescue the run and make the
//! test pass for the wrong reason.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A release base that cannot answer: port 1 on the loopback interface, which
/// refuses immediately rather than hanging on a DNS or connect timeout.
const UNREACHABLE_BASE: &str = "http://127.0.0.1:1/releases/download";

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("scripts")
        .join("contracts-check.sh")
}

/// Run the seal check with gate 2 made unreachable, in the given strictness.
fn run_with_gate_two_unavailable(strict: &str) -> Output {
    let empty = tempfile::tempdir().expect("a temp dir for the absent-checkout case");
    Command::new("bash")
        .arg(script())
        // Inherited CI (this suite runs there) would otherwise decide
        // strictness for us and make one of these two cases untestable.
        .env("TAPES_CONTRACT_STRICT", strict)
        .env("TAPES_RELEASE_BASE", UNREACHABLE_BASE)
        .env("TAPES_FALLBACK_REPO", empty.path())
        .env_remove("TAPES_REPO")
        .env_remove("TAPES_CONTRACT_TAG")
        .output()
        .expect("the seal check script must be runnable")
}

#[test]
fn a_seal_that_could_not_read_its_input_fails_in_strict_mode() {
    // The whole point: CI asking "do the bytes match?" and getting a green
    // job back means the comparison happened. If it could not happen, the
    // only honest answer is a failure.
    let output = run_with_gate_two_unavailable("1");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "the seal must block when it verified nothing; it exited {:?}\nstdout: {}\nstderr: {stderr}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        stderr.contains("nothing authoritative ran"),
        "the failure must say why it blocked: {stderr}",
    );
}

#[test]
fn a_skipped_gate_says_it_verified_nothing_rather_than_reporting_success() {
    // Off a strict runner the skip stays — a developer offline still gets
    // gate 1 — but the run may not read as a verdict. The exit code alone
    // cannot carry that distinction, so the words have to.
    let output = run_with_gate_two_unavailable("0");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "a non-strict run keeps the skip: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("gate 2 DID NOT RUN"),
        "a skipped gate must say so unmistakably: {stdout}",
    );
    assert!(
        stdout.contains("NOT compared against the published"),
        "a skipped gate must say what it did not do: {stdout}",
    );
}

#[test]
fn gate_one_still_runs_when_gate_two_is_unavailable() {
    // The fingerprint check needs neither network nor a checkout, so losing
    // gate 2 must not cost it. If this regressed, an offline run would verify
    // nothing at all while still printing reassuring output.
    let output = run_with_gate_two_unavailable("0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("matches its recorded fingerprint"),
        "gate 1 must still run: {stdout}",
    );
}
