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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tapes_harnesses::harness;
use tapes_mock_upstream::ingest::HARNESS_ID_UNKNOWN;
use tapes_mock_upstream::manifest::{Status, VersionManifest, probe, which};
use tapes_mock_upstream::recipe::{OneShotContext, OneShotRecipe, SCRIPTED_PROMPT, for_harness};
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

/// Run a command with an environment overlay, killing it if it overruns.
///
/// The environment is built from a deliberately narrow base rather than
/// inherited wholesale. An inherited environment would carry the developer's own
/// provider credentials and base-URL overrides straight into the cell — which
/// would both invalidate the result (a turn that reached a real provider is not
/// a turn against the mock) and risk spending real money.
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

    let deadline = Instant::now() + TURN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let output = child.wait_with_output().ok();
                return RunResult {
                    status: None,
                    stdout: output
                        .as_ref()
                        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                        .unwrap_or_default(),
                    stderr: output
                        .as_ref()
                        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
                        .unwrap_or_default(),
                    timed_out: true,
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                return RunResult {
                    status: None,
                    stdout: String::new(),
                    stderr: format!("wait failed: {err}"),
                    timed_out: false,
                };
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) => RunResult {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
        },
        Err(err) => RunResult {
            status: None,
            stdout: String::new(),
            stderr: format!("output failed: {err}"),
            timed_out: false,
        },
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

    Outcome::Passed
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

    let manifest_path = write_manifest(&manifest);
    print_table(&cells, &manifest, manifest_path.as_deref());

    let failures: Vec<&Cell> = cells
        .iter()
        .filter(|cell| matches!(cell.outcome, Outcome::Failed(_)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} matrix cell(s) failed:\n{}",
        failures.len(),
        failures
            .iter()
            .map(|cell| format!(
                "  {} / {}: {}",
                cell.column,
                cell.harness,
                cell.outcome.detail()
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Write the manifest beside the test binary, and return where it went.
fn write_manifest(manifest: &VersionManifest) -> Option<PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))?;
    let path = dir.join(MANIFEST_NAME);
    let json = manifest.to_json().ok()?;
    std::fs::write(&path, json).ok()?;
    Some(path)
}

/// Print the matrix, the versions it ran against, and every skip reason.
///
/// Printed unconditionally rather than only on failure: the reasons a cell did
/// not run are the most perishable information a run produces, and a green run
/// whose coverage nobody can see is how a matrix silently shrinks.
fn print_table(cells: &[Cell], manifest: &VersionManifest, manifest_path: Option<&Path>) {
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

    if let Some(path) = manifest_path {
        println!("\nmanifest: {}", path.display());
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
