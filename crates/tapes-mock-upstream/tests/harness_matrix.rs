//! The harness regression matrix, Tier 1: real harness binaries, real HTTP, no
//! cluster.
//!
//! # What this covers that nothing else did
//!
//! Every layer beneath this file already had tests, and every layer was green.
//! Launch recipes are unit-tested as pure functions; the envelope is checked
//! against a shared fixture corpus; attribution lanes are exercised with
//! synthetic session files. None of that launches a harness. The failures worth
//! catching live in the composition — a harness release changes a wire detail,
//! or a recipe points at a path the proxy does not serve — and a composition
//! nothing exercises is a composition that breaks quietly.
//!
//! So: for each registry harness, start a real mock provider and a real mock
//! ingest, launch the actual binary, and assert on what crossed the wire.
//!
//! # Two columns
//!
//! * **harness → mock.** The harness is pointed straight at the mock upstream.
//!   This needs nothing but the harness binary, so it runs wherever one is
//!   installed. It proves the launch recipe produces a configuration the
//!   harness accepts and that the harness's turn lands on the surface the
//!   recipe claimed.
//! * **harness → CLI → mock.** The harness is launched *through* a capture
//!   client, which proxies to the mock upstream and posts the captured turn to
//!   the mock ingest. This is where attribution, nonce handling, and envelope
//!   stamping actually happen, so it is where those are asserted. The clients
//!   live in other repositories, so their binaries are supplied by path.
//!
//! # Skips are loud
//!
//! A cell that cannot run reports a *skip with a reason*, in the printed table
//! and in the emitted manifest. Nothing is silently omitted. This is the
//! convention the whole file is built around: a matrix that quietly dropped the
//! cells it could not run would look the same whether it covered five harnesses
//! or one.
//!
//! # Running it
//!
//! ```text
//! cargo test -p tapes-mock-upstream --test harness_matrix -- --nocapture
//! TAPESCTL_BIN=/path/to/tapesctl cargo test -p tapes-mock-upstream --test harness_matrix -- --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tapes_harnesses::harness;
use tapes_mock_upstream::ingest::HARNESS_ID_UNKNOWN;
use tapes_mock_upstream::manifest::{Status, VersionManifest, probe, which};
use tapes_mock_upstream::recipe::{
    OneShotContext, OneShotRecipe, Pointing, SCRIPTED_PROMPT, Surface, ToleratedExit, for_harness,
};
use tapes_mock_upstream::{MockPair, TURN_TIMEOUT};

/// The capture clients this matrix can drive, and the variable naming each
/// one's binary.
///
/// By path rather than by `PATH` lookup because these live in other
/// repositories: a matrix run is nearly always testing a *specific* build of a
/// client — the one in the checkout next door — and finding some other copy
/// installed on the machine would be worse than finding none.
const CLIS: &[(&str, &str)] = &[("tapesctl", "TAPESCTL_BIN"), ("paper", "PAPER_BIN")];

/// Where the run writes its version manifest, relative to the target directory.
const MANIFEST_NAME: &str = "harness-matrix-manifest.json";

/// What became of one cell.
#[derive(Debug, Clone)]
enum Outcome {
    /// It ran and its assertions held.
    Passed,
    /// It did not run, for this reason.
    Skipped(String),
    /// It ran and something did not hold.
    Failed(String),
}

impl Outcome {
    fn marker(&self) -> &'static str {
        match self {
            Self::Passed => "pass",
            Self::Skipped(_) => "SKIP",
            Self::Failed(_) => "FAIL",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Passed => "",
            Self::Skipped(reason) | Self::Failed(reason) => reason,
        }
    }
}

/// One cell of the matrix.
#[derive(Debug, Clone)]
struct Cell {
    column: String,
    harness: String,
    outcome: Outcome,
}

/// What a harness process did.
struct RunResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl RunResult {
    /// A short description for a failure message. Truncated because a harness
    /// can be extremely chatty and a wall of output buries the assertion that
    /// actually failed.
    fn summary(&self) -> String {
        let tail = |text: &str| -> String {
            let trimmed = text.trim();
            let start = trimmed.len().saturating_sub(600);
            trimmed[start..].replace('\n', " | ")
        };
        format!(
            "exit={:?} timed_out={} stdout=[{}] stderr=[{}]",
            self.status,
            self.timed_out,
            tail(&self.stdout),
            tail(&self.stderr),
        )
    }
}

/// How long a pipe reader gets to finish once the process it was reading has
/// gone.
///
/// It ordinarily finishes instantly — the pipe reaches EOF as the last writer
/// closes. The bound covers the case where the harness left a grandchild holding
/// the write end, which is a real thing and must not hold a matrix run open
/// indefinitely. Whatever the reader collected by then is what the cell reports.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// One of a child's pipes, being drained on its own thread.
///
/// A child's stdout and stderr must be read *while* it runs, not after it
/// exits. A pipe holds on the order of 64 KiB; a process that writes past that
/// blocks inside `write` until somebody reads, and a runner that waits for the
/// exit first waits forever. Every harness this matrix launches is chatty enough
/// to reach that, and the symptom is the worst kind: the run reports a timeout,
/// which reads as a harness that hung when in fact the runner was the one not
/// listening.
struct Collected {
    /// What has been read so far. Shared with the reader thread so the bytes
    /// are still available if that thread outlives the wait.
    buffer: Arc<Mutex<Vec<u8>>>,
    /// The reader, until it is settled.
    reader: Option<JoinHandle<()>>,
}

impl Collected {
    /// Start draining `pipe` immediately, if there is one.
    fn drain(pipe: Option<impl Read + Send + 'static>) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let reader = pipe.map(|pipe| {
            let sink = Arc::clone(&buffer);
            std::thread::spawn(move || drain_into(pipe, &sink))
        });
        Self { buffer, reader }
    }

    /// Give the reader a bounded moment to reach EOF, then take what it has.
    ///
    /// A reader still running after the grace is holding a pipe some grandchild
    /// has not closed. It is left to finish into its own buffer and go away with
    /// the process: joining it would hang the whole matrix on a detail that can
    /// no longer change the cell's result.
    fn settle(&mut self) -> String {
        if let Some(reader) = self.reader.take() {
            let deadline = Instant::now() + DRAIN_GRACE;
            while !reader.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
        self.buffer
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }
}

/// Read `pipe` to EOF, appending everything it yields to `sink`.
///
/// A read error ends the drain the same way EOF does: the pipe is gone either
/// way, and a reader that spun on the error would burn a core for the rest of
/// the run.
fn drain_into(mut pipe: impl Read, sink: &Mutex<Vec<u8>>) {
    let mut chunk = [0_u8; 8192];
    while let Ok(read) = pipe.read(&mut chunk) {
        if read == 0 {
            break;
        }
        if let Ok(mut sink) = sink.lock() {
            sink.extend_from_slice(&chunk[..read]);
        }
    }
}

/// Run a command with an environment overlay, killing it if it overruns.
///
/// The environment is built from a deliberately narrow base rather than
/// inherited wholesale. An inherited environment would carry the developer's own
/// provider credentials and base-URL overrides straight into the cell — which
/// would both invalidate the result (a turn that reached a real provider is not
/// a turn against the mock) and risk spending real money.
///
/// Both pipes are drained concurrently for the whole life of the process. See
/// [`Collected`] for why that is a correctness requirement rather than a
/// convenience.
fn run(program: &Path, args: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> RunResult {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The minimum a modern harness needs to start at all. Everything else the
    // cell wants, it states.
    for passthrough in ["PATH", "LANG", "LC_ALL", "TZ", "TERM", "SHELL", "TMPDIR"] {
        if let Some(value) = std::env::var_os(passthrough) {
            command.env(passthrough, value);
        }
    }
    for (name, value) in env {
        command.env(name, value);
    }

    let Ok(mut child) = command.spawn() else {
        return RunResult {
            status: None,
            stdout: String::new(),
            stderr: format!("could not spawn {}", program.display()),
            timed_out: false,
        };
    };

    // Before the first wait, so nothing the child writes can ever be waiting on
    // a reader that has not started.
    let mut stdout = Collected::drain(child.stdout.take());
    let mut stderr = Collected::drain(child.stderr.take());

    let deadline = Instant::now() + TURN_TIMEOUT;
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                // Reap it, so the pipes' write ends close and the readers can
                // reach EOF rather than sitting out their grace.
                let _ = child.wait();
                break (None, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                return RunResult {
                    status: None,
                    stdout: stdout.settle(),
                    stderr: format!("wait failed: {err}"),
                    timed_out: false,
                };
            }
        }
    };

    RunResult {
        status,
        stdout: stdout.settle(),
        stderr: stderr.settle(),
        timed_out,
    }
}

/// The **harness → mock** cell.
///
/// Asserts the launch recipe produced a configuration the harness accepts, and
/// that the resulting turn landed on the surface the recipe declared. It cannot
/// assert attribution: with no capture client in the path there is nothing to
/// attribute, which is exactly why the second column exists.
fn harness_vs_mock(recipe: &OneShotRecipe, binary: &Path) -> Outcome {
    let Ok(sandbox) = tempfile::tempdir() else {
        return Outcome::Failed("could not create a sandbox directory".to_owned());
    };
    let Ok(pair) = MockPair::start() else {
        return Outcome::Failed("could not start the mock pair".to_owned());
    };

    let ctx = OneShotContext {
        endpoint: pair.upstream.base_url(),
        sandbox: sandbox.path().to_path_buf(),
        // No capture client, so no nonce: a plugin must read an unset variable
        // as "the launching client predates the contract" and simply not send
        // the echo, which this cell incidentally proves.
        nonce: None,
    };
    let plan = match recipe.plan(&ctx) {
        Ok(plan) => plan,
        Err(err) => return Outcome::Failed(format!("planning failed: {err}")),
    };

    let result = run(binary, &plan.args, &plan.env, &plan.cwd);
    if !pair.upstream.wait_for_turn(Duration::from_secs(5)) {
        return Outcome::Failed(format!(
            "no model call reached the mock upstream; {}",
            result.summary(),
        ));
    }

    let turns = pair.upstream.turn_requests();
    if !turns.iter().any(|turn| recipe.surface.accepts(&turn.path)) {
        return Outcome::Failed(format!(
            "no turn landed on {:?}; saw {:?}",
            recipe.surface.paths(),
            turns.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
        ));
    }

    // The second half of the cell. Asserted last so that a harness which failed
    // *and* sent nothing reports the more useful of the two facts first.
    if let Some(failure) = unclean_finish(recipe, &result) {
        return Outcome::Failed(failure);
    }

    Outcome::Passed
}

/// Did the process finish in a way a cell can accept?
///
/// Returns the failure message when it did not. A cell requires *both* halves —
/// the expected interaction and a clean finish — because a harness that sent its
/// request and then died has not shown the composition works; it has shown that
/// its first request was well formed. Accepting the interaction alone is how a
/// harness that starts crashing after every turn goes on reporting green, which
/// is precisely the quiet breakage this matrix exists to catch.
///
/// A timeout and a death by signal are never acceptable. The only tolerated
/// finish is an exact non-zero code the recipe names, with its reason — see
/// [`ToleratedExit`].
fn unclean_finish(recipe: &OneShotRecipe, result: &RunResult) -> Option<String> {
    if result.timed_out {
        return Some(format!(
            "the expected interaction happened but the process never exited (killed after \
             {TURN_TIMEOUT:?}); {}",
            result.summary(),
        ));
    }
    match result.status {
        Some(0) => None,
        Some(code) => match recipe.tolerated_exit {
            Some(tolerated) if tolerated.code == code => None,
            _ => Some(format!(
                "the expected interaction happened but the process exited {code}; {}",
                result.summary(),
            )),
        },
        // No code at all: killed by a signal, which is a crash however tidy the
        // wire traffic looked.
        None => Some(format!(
            "the expected interaction happened but the process exited without a status of its \
             own; {}",
            result.summary(),
        )),
    }
}

/// The **harness → CLI → mock** cell.
///
/// This is where the assertions that matter live, because this is the only
/// configuration in which any of them are even meaningful:
///
/// * *launched implies attributed* — a turn reached ingest carrying a harness id
///   and a session id, rather than the `unknown` sentinel;
/// * *envelope shape* — the id the turn was filed under is the harness that was
///   actually launched, not whichever one the request claimed;
/// * *nonce stripping* — the per-launch secret the client handed the harness did
///   not travel upstream. A leaked nonce is a real disclosure, and it is
///   invisible to every test that does not look at the upstream's request.
fn harness_via_cli(recipe: &OneShotRecipe, harness_binary: &Path, cli: &Path) -> Outcome {
    let Ok(sandbox) = tempfile::tempdir() else {
        return Outcome::Failed("could not create a sandbox directory".to_owned());
    };
    let Ok(pair) = MockPair::start() else {
        return Outcome::Failed("could not start the mock pair".to_owned());
    };

    // The client plans the harness's launch itself — that is the code under
    // test — so this cell supplies only the passthrough argv, not the recipe's
    // pointing environment.
    let mut args = vec![
        "start".to_owned(),
        recipe.harness_id.to_owned(),
        "--upstream".to_owned(),
        pair.upstream.base_url(),
        "--tapes-url".to_owned(),
        pair.ingest.base_url(),
    ];
    args.push("--".to_owned());
    for token in recipe.argv {
        args.push(if *token == "{prompt}" {
            SCRIPTED_PROMPT.to_owned()
        } else {
            (*token).to_owned()
        });
    }

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    // The same universal relocation the recipe's own plan applies, for the same
    // reason — and here it must be set on the *client*, because the client is
    // the process that both launches the harness and resolves the session files
    // the harness writes. Setting it on only one of them is how attribution
    // breaks while capture keeps working.
    let home = sandbox.path().join("home");
    if std::fs::create_dir_all(&home).is_err() {
        return Outcome::Failed("could not create the sandbox home".to_owned());
    }
    env.insert("HOME".to_owned(), home.display().to_string());

    // A harness captured by an in-harness extension needs that extension
    // installed before the client will launch it — the client refuses outright
    // otherwise, which is the correct behaviour and not something to work
    // around. Installing through the crate's own `PluginArtifact::install`
    // rather than through a `<client> plugin install` subcommand keeps this cell
    // portable across clients, whose installer command lines differ; exercising
    // each client's installer is a separate cell worth adding later.
    if let Some(entry) = harness::find(recipe.harness_id) {
        for artifact in entry.plugin_artifacts() {
            if let Err(err) = artifact.install(&home) {
                return Outcome::Failed(format!(
                    "could not install the {} capture plugin: {err}",
                    recipe.harness_id,
                ));
            }
        }
    }

    for (variable, subdir) in recipe.sandbox_env {
        let path = sandbox.path().join(subdir);
        if std::fs::create_dir_all(&path).is_err() {
            return Outcome::Failed(format!("could not create {}", path.display()));
        }
        env.insert((*variable).to_owned(), path.display().to_string());
    }
    for (variable, value) in recipe.extra_env {
        env.insert((*variable).to_owned(), (*value).to_owned());
    }
    // The harness binary must be findable by the client, which resolves it on
    // PATH the way a user's shell would.
    if let Some(dir) = harness_binary.parent() {
        let inherited = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".to_owned(), format!("{}:{inherited}", dir.display()));
    }

    let cwd = sandbox.path().join("cwd");
    if std::fs::create_dir_all(&cwd).is_err() {
        return Outcome::Failed("could not create the working directory".to_owned());
    }

    let result = run(cli, &args, &env, &cwd);

    // A client that does not offer this harness is a coverage gap, not a
    // capture failure, and reporting it as red would train readers to ignore a
    // red cell. It stays highly visible as a skip that quotes the client's own
    // refusal.
    //
    // Matching on the client's message is a narrow, deliberate heuristic, and it
    // is narrow on purpose: only a refusal that *names itself* a support gap is
    // downgraded. A crash, a panic, or any other non-zero exit stays a failure,
    // so a client that starts breaking cannot quietly become a skip.
    if let Some(reason) = declined_to_launch(recipe.harness_id, &result) {
        return Outcome::Skipped(reason);
    }

    if !pair.ingest.wait_for_turn(Duration::from_secs(10)) {
        return Outcome::Failed(format!(
            "no turn reached the mock ingest; {}",
            result.summary(),
        ));
    }

    // launched implies attributed.
    let turns = pair.ingest.landed_turns();
    let attributed: Vec<_> = turns
        .iter()
        .filter(|turn| turn.envelope.is_attributed())
        .collect();
    if attributed.is_empty() {
        let seen: Vec<_> = turns
            .iter()
            .map(|turn| turn.envelope.harness_id.clone())
            .collect();
        return Outcome::Failed(format!(
            "a launched harness produced only unattributed turns (harness ids {seen:?}); {}",
            result.summary(),
        ));
    }

    // Envelope shape: filed under the harness that was launched.
    if let Some(wrong) = attributed
        .iter()
        .find(|turn| turn.envelope.harness_id != recipe.harness_id)
    {
        return Outcome::Failed(format!(
            "a {} launch was filed under harness id {:?}",
            recipe.harness_id, wrong.envelope.harness_id,
        ));
    }

    // Nonce stripping: the per-launch secret must not reach the upstream.
    let nonce_header = tapes_capture::gateway::GATEWAY_NONCE_HEADER;
    if let Some(leaked) = pair
        .upstream
        .requests()
        .iter()
        .find(|request| request.header(nonce_header).is_some())
    {
        return Outcome::Failed(format!(
            "the capture nonce leaked upstream on {} {}",
            leaked.method, leaked.path,
        ));
    }

    // And the same second half this column's sibling requires. The process here
    // is the client rather than the harness, but a client that launched a
    // harness reports the harness's fate in its own exit status, so the recipe's
    // tolerance is the right one to consult. Without this, the refusal
    // heuristic above reads as though a non-zero exit stayed a failure while in
    // fact nothing checked it again.
    if let Some(failure) = unclean_finish(recipe, &result) {
        return Outcome::Failed(failure);
    }

    Outcome::Passed
}

/// Did the client refuse to launch this harness because it does not support it?
///
/// Returns the reason to report, or `None` when the run was not a
/// self-declared support gap — in which case the cell's ordinary assertions
/// apply and any problem stays a failure.
fn declined_to_launch(harness_id: &str, result: &RunResult) -> Option<String> {
    if result.status == Some(0) || result.timed_out {
        return None;
    }
    let output = format!("{} {}", result.stdout, result.stderr).to_lowercase();
    let declined = output.contains("unsupported harness")
        || (output.contains("unsupported") && output.contains(harness_id));
    declined.then(|| {
        format!(
            "the client does not support {harness_id}: {}",
            result.stderr.trim().replace('\n', " | "),
        )
    })
}

/// The whole matrix.
///
/// One test rather than one per cell: the cells share the expensive part
/// (launching real binaries), the table is only readable whole, and a run has to
/// emit exactly one manifest. Individual cell failures are reported by name in
/// the table and in the panic message, so the granularity a per-cell test would
/// buy is preserved where it matters.
#[test]
fn the_harness_matrix_holds() {
    let mut cells: Vec<Cell> = Vec::new();
    let mut manifest = VersionManifest::new();

    // Resolve the clients first: their availability decides whether the
    // composition column runs at all, and it is the same answer for every row.
    let mut clients: Vec<(String, PathBuf)> = Vec::new();
    for (name, variable) in CLIS {
        match std::env::var(variable)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(value) => match which(&value) {
                Some(path) => {
                    manifest.record_cli(*name, probe(&path.display().to_string(), &["--version"]));
                    clients.push(((*name).to_owned(), path));
                }
                None => manifest.record_cli(
                    *name,
                    Status::Skipped {
                        reason: format!("{variable}={value} does not name a file"),
                    },
                ),
            },
            None => manifest.record_cli(
                *name,
                Status::Skipped {
                    reason: format!(
                        "{variable} is unset; the {name} CLI lives in another repository, so its \
                         binary must be supplied by path",
                    ),
                },
            ),
        }
    }

    for harness in harness::REGISTRY {
        let Some(recipe) = for_harness(harness) else {
            panic!(
                "{} is in the registry with no one-shot recipe — the recipe table's own \
                 invariant test should have caught this",
                harness.id(),
            );
        };

        // A harness with no one-shot launch at all: one skip, every column.
        if let Some(reason) = recipe.unsupported {
            manifest.record_harness(
                harness.id(),
                Status::Skipped {
                    reason: reason.to_owned(),
                },
            );
            cells.push(Cell {
                column: "harness -> mock".to_owned(),
                harness: harness.id().to_owned(),
                outcome: Outcome::Skipped(reason.to_owned()),
            });
            for (name, _) in &clients {
                cells.push(Cell {
                    column: format!("harness -> {name} -> mock"),
                    harness: harness.id().to_owned(),
                    outcome: Outcome::Skipped(reason.to_owned()),
                });
            }
            continue;
        }

        let status = probe(recipe.binary, recipe.version_args);
        manifest.record_harness(harness.id(), status.clone());

        let Status::Ran { path, .. } = &status else {
            let reason = match &status {
                Status::Skipped { reason } => reason.clone(),
                Status::Ran { .. } => unreachable!(),
            };
            cells.push(Cell {
                column: "harness -> mock".to_owned(),
                harness: harness.id().to_owned(),
                outcome: Outcome::Skipped(reason.clone()),
            });
            for (name, _) in &clients {
                cells.push(Cell {
                    column: format!("harness -> {name} -> mock"),
                    harness: harness.id().to_owned(),
                    outcome: Outcome::Skipped(reason.clone()),
                });
            }
            continue;
        };

        cells.push(Cell {
            column: "harness -> mock".to_owned(),
            harness: harness.id().to_owned(),
            outcome: harness_vs_mock(recipe, path),
        });

        for (name, cli) in &clients {
            cells.push(Cell {
                column: format!("harness -> {name} -> mock"),
                harness: harness.id().to_owned(),
                outcome: harness_via_cli(recipe, path, cli),
            });
        }
    }

    // The composition column is absent entirely when no client was supplied.
    // Record that as its own visible row rather than letting the table simply
    // be shorter than a reader expects.
    if clients.is_empty() {
        cells.push(Cell {
            column: "harness -> CLI -> mock".to_owned(),
            harness: "(all)".to_owned(),
            outcome: Outcome::Skipped(
                "no capture client binary was supplied; set TAPESCTL_BIN or PAPER_BIN to run the \
                 composition column"
                    .to_owned(),
            ),
        });
    }

    let written = manifest_dir().and_then(|dir| write_manifest(&manifest, &dir));
    print_table(&cells, &manifest, &written);

    let mut failures: Vec<String> = cells
        .iter()
        .filter(|cell| matches!(cell.outcome, Outcome::Failed(_)))
        .map(|cell| {
            format!(
                "  {} / {}: {}",
                cell.column,
                cell.harness,
                cell.outcome.detail(),
            )
        })
        .collect();

    // A run that could not write its manifest is a failed run. The manifest is
    // an advertised output of every run and the drift watch's only input, so a
    // run that finishes without one has produced a green result nobody can date:
    // "we still work against the current release" and "we still work against
    // whatever was installed six weeks ago" become the same report. Collapsing
    // the error into a missing file made that outcome look exactly like a
    // successful run, which is the failure this file is built to refuse.
    if let Err(err) = &written {
        failures.push(format!("  manifest: {err}"));
    }

    assert!(
        failures.is_empty(),
        "{} matrix failure(s):\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Where the manifest goes: beside the test binary, which is the directory CI
/// uploads from.
fn manifest_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("the test binary's own path is unknown: {err}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))
}

/// Write the manifest into `dir`, and return where it went.
///
/// Every failure carries the underlying error rather than collapsing to an
/// absence: the caller turns this into a red run, and "the manifest is missing"
/// is not something a reader can act on without knowing whether it was the
/// serialisation, the directory, or the write that refused.
fn write_manifest(manifest: &VersionManifest, dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(MANIFEST_NAME);
    let json = manifest
        .to_json()
        .map_err(|err| format!("the manifest could not be serialised: {err}"))?;
    std::fs::write(&path, json).map_err(|err| {
        format!(
            "the manifest could not be written to {}: {err}",
            path.display()
        )
    })?;
    Ok(path)
}

/// Print the matrix, the versions it ran against, and every skip reason.
///
/// Printed unconditionally rather than only on failure: the reasons a cell did
/// not run are the most perishable information a run produces, and a green run
/// whose coverage nobody can see is how a matrix silently shrinks.
fn print_table(cells: &[Cell], manifest: &VersionManifest, written: &Result<PathBuf, String>) {
    println!("\n=== harness regression matrix (Tier 1) ===\n");

    let column_width = cells.iter().map(|c| c.column.len()).max().unwrap_or(10);
    let harness_width = cells.iter().map(|c| c.harness.len()).max().unwrap_or(10);
    for cell in cells {
        println!(
            "  {:<column_width$}  {:<harness_width$}  {}",
            cell.column,
            cell.harness,
            cell.outcome.marker(),
        );
    }

    println!("\n--- versions ---");
    for (name, version) in manifest.versions() {
        println!("  {name}: {version}");
    }

    let skips: Vec<&Cell> = cells
        .iter()
        .filter(|cell| matches!(cell.outcome, Outcome::Skipped(_)))
        .collect();
    if !skips.is_empty() {
        println!("\n--- skipped, and why ---");
        for cell in skips {
            println!(
                "  {} / {}\n      {}",
                cell.column,
                cell.harness,
                cell.outcome.detail(),
            );
        }
    }

    match written {
        Ok(path) => println!("\nmanifest: {}", path.display()),
        // Beside the table, not only in the panic message: the table is what a
        // reader scrolls to, and a run whose manifest is missing is a run whose
        // versions are unrecorded.
        Err(err) => println!("\nmanifest: NOT WRITTEN — {err}"),
    }
    println!();
}

/// The registry, the recipe table, and the matrix agree on which harnesses
/// exist.
///
/// Cheap, and it fails for a clear reason: a harness added to the registry
/// without a recipe would otherwise surface as a panic in the middle of a long
/// matrix run.
#[test]
fn the_matrix_covers_every_registry_harness() {
    for harness in harness::REGISTRY {
        assert!(
            for_harness(harness).is_some(),
            "{} has no one-shot recipe",
            harness.id(),
        );
    }
}

/// The unknown sentinel a refusal assertion checks for is the one the registry
/// and the envelope contract agree on, not a string spelled in this file.
#[test]
fn the_unknown_sentinel_matches_the_capture_contract() {
    assert_eq!(
        HARNESS_ID_UNKNOWN,
        tapes_capture::envelope::HARNESS_ID_UNKNOWN
    );
}

// ---------------------------------------------------------------------------
// The runner's own tests.
//
// Everything above launches harnesses; this launches the runner. Its bugs are
// the expensive kind because they make a cell *look* green — a pass awarded to a
// harness that died, a stall reported as somebody else's timeout — and they
// cannot be provoked with a real harness binary. The stand-in below can be asked
// for either on demand, so they have somewhere to be caught.
// ---------------------------------------------------------------------------

/// The stand-in harness, built by cargo alongside this test.
const FAKE_HARNESS: &str = env!("CARGO_BIN_EXE_matrix-fake-harness");

/// A recipe that launches the stand-in instead of a real harness.
///
/// It points through [`Pointing::Claude`] deliberately: the stand-in reads the
/// upstream out of the same environment variable the real Claude launch recipe
/// writes, so these tests exercise the runner against a genuine plan rather than
/// against a private arrangement between the test and the fake.
fn fake_recipe(
    argv: &'static [&'static str],
    tolerated_exit: Option<ToleratedExit>,
) -> OneShotRecipe {
    OneShotRecipe {
        harness_id: "matrix-fake-harness",
        binary: "matrix-fake-harness",
        version_args: &["--version"],
        argv,
        surface: Surface::AnthropicMessages,
        pointing: Pointing::Claude,
        sandbox_env: &[],
        extra_env: &[],
        unsupported: None,
        tolerated_exit,
    }
}

/// A cell is not a pass just because the request arrived.
///
/// The stand-in here does exactly what a broken harness release does: it sends a
/// well-formed turn to the surface the recipe named, and then dies. Every
/// interaction assertion holds; the composition does not work. Before the second
/// half of the cell existed, this was a green cell.
#[test]
fn a_harness_that_sends_its_turn_then_fails_is_not_a_pass() {
    let recipe = fake_recipe(&["--turn-then-exit", "1"], None);
    match harness_vs_mock(&recipe, Path::new(FAKE_HARNESS)) {
        Outcome::Failed(reason) => assert!(
            reason.contains("exited 1"),
            "the failure must name the exit status, got: {reason}",
        ),
        other => panic!("a harness that exited 1 must not report {other:?}"),
    }
}

/// And the ordinary path still passes: turn on the expected surface, clean exit.
#[test]
fn a_harness_that_sends_its_turn_and_exits_cleanly_passes() {
    let recipe = fake_recipe(&["--turn-then-exit", "0"], None);
    match harness_vs_mock(&recipe, Path::new(FAKE_HARNESS)) {
        Outcome::Passed => {}
        other => panic!("a clean one-shot turn must pass, got {other:?}"),
    }
}

/// A harness that legitimately exits non-zero says so on its own recipe, and
/// only that code is forgiven.
///
/// The point of the exception being per-recipe is that it cannot be reached by
/// accident: the same stand-in, exiting the same way, is a failure one test
/// above and a pass here, and the only difference is a reason somebody wrote
/// down.
#[test]
fn a_recipe_may_tolerate_the_one_exit_code_it_names() {
    let tolerated = Some(ToleratedExit {
        code: 3,
        reason: "the stand-in is asked for this exit, to prove the exception is per-recipe",
    });

    let matching = fake_recipe(&["--turn-then-exit", "3"], tolerated);
    match harness_vs_mock(&matching, Path::new(FAKE_HARNESS)) {
        Outcome::Passed => {}
        other => panic!("the tolerated exit must pass, got {other:?}"),
    }

    // A different non-zero code is still a failure: the tolerance is for one
    // stated exit, not for "non-zero is fine here".
    let other_code = fake_recipe(&["--turn-then-exit", "4"], tolerated);
    match harness_vs_mock(&other_code, Path::new(FAKE_HARNESS)) {
        Outcome::Failed(reason) => assert!(reason.contains("exited 4"), "got: {reason}"),
        other => panic!("an untolerated exit must fail, got {other:?}"),
    }
}

/// A harness chatty enough to fill both pipes still completes.
///
/// 256 KiB in each direction is comfortably past the ~64 KiB a pipe holds, so a
/// runner that waits for the exit before reading blocks the child in `write` and
/// then reports the timeout as the harness's fault. The byte counts are asserted
/// as well as the exit: draining is only correct if it also keeps what it read.
#[test]
fn a_chatty_harness_does_not_stall_the_runner() {
    const FLOOD: usize = 256 * 1024;

    let sandbox = tempfile::tempdir().unwrap();
    let result = run(
        Path::new(FAKE_HARNESS),
        &["--flood".to_owned(), FLOOD.to_string()],
        &BTreeMap::new(),
        sandbox.path(),
    );

    assert!(
        !result.timed_out,
        "a harness that writes {FLOOD} bytes to each pipe and exits must not time out",
    );
    assert_eq!(result.status, Some(0));
    assert_eq!(result.stdout.len(), FLOOD, "stdout was not fully collected");
    assert_eq!(result.stderr.len(), FLOOD, "stderr was not fully collected");
}

/// A manifest that cannot be written is reported, not swallowed.
///
/// The unwritable destination is a path that is a *file*, which fails the same
/// way on every platform this runs on and — unlike a read-only directory — still
/// fails when the tests run as root, which is the ordinary case in CI.
#[test]
fn a_manifest_that_cannot_be_written_is_reported() {
    let sandbox = tempfile::tempdir().unwrap();
    let blocker = sandbox.path().join("not-a-directory");
    std::fs::write(&blocker, "").unwrap();

    let error = write_manifest(&VersionManifest::new(), &blocker)
        .expect_err("writing into a file must not report success");
    assert!(
        error.contains(MANIFEST_NAME),
        "the error must name the file it could not write, got: {error}",
    );
}

/// And a manifest that can be written lands where the run says it did, holding
/// what the run recorded.
#[test]
fn a_written_manifest_lands_where_the_run_says_it_did() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut manifest = VersionManifest::new();
    manifest.record_harness(
        "claude",
        Status::Skipped {
            reason: "a stand-in row".to_owned(),
        },
    );

    let path = write_manifest(&manifest, sandbox.path()).unwrap();
    assert_eq!(path, sandbox.path().join(MANIFEST_NAME));

    let written: VersionManifest =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written, manifest);
}
