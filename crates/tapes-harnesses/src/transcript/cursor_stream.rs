//! Cursor CLI structured-stdout capture.
//!
//! Cursor CLI has no provider base-URL knob and no transcript directory this
//! crate can read. Its print mode writes newline-delimited JSON on stdout.
//! This module builds the `agent` argv for that mode and turns one saved
//! stream into a transcript ready for upload. The consumer runs `agent`, saves
//! its stdout, and uploads the file. [`load`] expects a private, same-user
//! directory.

use std::ffi::OsStr;
use std::fmt;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::value::RawValue;
use snafu::Snafu;

use crate::harness::HARNESS_ID_CURSOR;

use super::files::jsonl_to_records;
use super::payload::{IngestEnvelope, TranscriptPayload};

/// Session facts read from one Cursor `stream-json` run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CursorStreamSession {
    /// Cursor's session id from the initial system/init event.
    pub session_id: String,
    /// Working directory from the `system` / `init` event, when supplied.
    pub cwd: Option<String>,
}

/// One saved Cursor stream, validated and ready to upload.
///
/// Built by [`load`]. The session facts and the records come from the same
/// read of the file, so the payload cannot mix one stream's id with another's
/// bytes.
pub struct CursorTranscript {
    session: CursorStreamSession,
    records: Box<RawValue>,
}

impl CursorTranscript {
    /// Facts from the stream's init event.
    #[must_use]
    pub fn session(&self) -> &CursorStreamSession {
        &self.session
    }

    /// The ingest payload for this stream.
    ///
    /// Uses the Cursor harness id and this stream's session id and cwd.
    /// The caller supplies the org id and auth subject.
    #[must_use]
    pub fn payload<'a>(&'a self, org_id: &'a str, auth_subject: &'a str) -> TranscriptPayload<'a> {
        TranscriptPayload {
            session: IngestEnvelope {
                org_id,
                auth_subject,
                harness_id: HARNESS_ID_CURSOR,
                harness_session_id: &self.session.session_id,
                harness_version: None,
                cwd: self.session.cwd.as_deref(),
            },
            agent_id: None,
            agent_type: None,
            description: None,
            tool_use_id: None,
            kind: None,
            records: &self.records,
        }
    }
}

impl fmt::Debug for CursorTranscript {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CursorTranscript")
            .finish_non_exhaustive()
    }
}

/// Cursor argument or stream errors.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum CursorStreamError {
    /// The caller passed a flag this module sets itself.
    #[snafu(display("{flag} is set by the cursor capture and cannot be passed"))]
    ConflictingArgument {
        /// The flag, without its value.
        flag: &'static str,
    },
    /// The caller passed an option this module does not know.
    #[snafu(display("unsupported cursor option"))]
    UnsupportedArgument,
    /// A known option in a spelling this module does not parse, such as
    /// `--trust=false` or `-Hvalue`.
    #[snafu(display("unsupported spelling of {flag}"))]
    UnsupportedArgumentForm {
        /// The option, without its value.
        flag: String,
    },
    /// A known option is missing its value.
    #[snafu(display("{flag} needs a value"))]
    MissingArgumentValue {
        /// The option name.
        flag: &'static str,
    },
    /// The stream file could not be read.
    #[snafu(display("could not read the cursor stream at {}", path.display()))]
    Open {
        /// The file path.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// Reading a line failed.
    #[snafu(display("could not read line {line} of the cursor stream"))]
    Read {
        /// Line number, starting at 1.
        line: usize,
        /// The IO error.
        source: std::io::Error,
    },
    /// A complete non-blank line is not valid JSON.
    #[snafu(display("line {line} of the cursor stream is not JSON"))]
    Parse {
        /// Line number, starting at 1.
        line: usize,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
    /// A line parsed as JSON but is not a valid event.
    #[snafu(display("line {line} of the cursor stream {reason}"))]
    InvalidEvent {
        /// Line number, starting at 1.
        line: usize,
        /// Why, without quoting the line.
        reason: &'static str,
    },
    /// The stream has no complete init event.
    #[snafu(display("the cursor stream has no initial system/init event"))]
    MissingInitialEvent,
    /// The lines could not be joined into a JSON array.
    #[snafu(display("could not build the records array from {}", path.display()))]
    PrepareRecords {
        /// The file path.
        path: PathBuf,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// The spool directory could not be listed.
    #[snafu(display("could not list cursor stream files in {}", root.display()))]
    Discover {
        /// The directory.
        root: PathBuf,
        /// The IO error.
        source: std::io::Error,
    },
}

impl CursorStreamError {
    /// Whether the error reports invalid stream content.
    #[must_use]
    pub fn is_invalid_stream(&self) -> bool {
        matches!(
            self,
            Self::Parse { .. } | Self::InvalidEvent { .. } | Self::MissingInitialEvent
        )
    }
}

/// Build the `agent` argv for Cursor's `stream-json` print mode.
///
/// Known global options from `caller_args` come first, then
/// `--print --output-format stream-json agent --`, then the prompt. The `agent`
/// subcommand and the `--` keep a prompt such as `resume` from running another
/// Cursor command. A prompt that starts with `-` needs a `--` before it. This
/// never adds `--trust`.
///
/// The options are an allowlist because the planner must know which ones take
/// a value to find where the prompt starts. `--worktree` takes the next token
/// as its name unless that token starts with `-`, as Cursor parses it.
///
/// # Errors
///
/// The flags this function sets itself, `--resume`, `--continue`, `--persist`,
/// unknown options, `--help`, `--version`, `--list-models`, and a known option
/// with no value. Messages can name known options, never unknown tokens or values.
pub fn plan_args(caller_args: &[String]) -> Result<Vec<String>, CursorStreamError> {
    let mut planned = Vec::with_capacity(caller_args.len() + 6);
    let mut index = 0;
    while let Some(argument) = caller_args.get(index) {
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "-" || !argument.starts_with('-') {
            break;
        }
        if let Some(flag) = reserved_argument(argument) {
            return Err(CursorStreamError::ConflictingArgument { flag });
        }

        let (name, inline_value) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        let inline_on_short = inline_value.is_some() && !name.starts_with("--");
        if is_early_exit_option(name) {
            return Err(CursorStreamError::UnsupportedArgument);
        }
        if is_attached_short_value(name)
            || (inline_on_short && supported_global_option(name).is_some())
        {
            return Err(CursorStreamError::UnsupportedArgumentForm {
                flag: public_flag(argument),
            });
        }

        match supported_global_option(name) {
            Some(GlobalOption::Switch) if inline_value.is_none() => {
                planned.push(argument.clone());
                index += 1;
            }
            Some(GlobalOption::Switch) => {
                return Err(CursorStreamError::UnsupportedArgumentForm {
                    flag: public_flag(argument),
                });
            }
            Some(GlobalOption::OptionalValue(flag)) => {
                if inline_value == Some("") {
                    return Err(CursorStreamError::MissingArgumentValue { flag });
                }
                planned.push(argument.clone());
                index += 1;
                if inline_value.is_none()
                    && let Some(value) = caller_args.get(index)
                    && !value.starts_with('-')
                {
                    planned.push(value.clone());
                    index += 1;
                }
            }
            Some(GlobalOption::Value(flag)) => {
                if let Some(value) = inline_value {
                    if value.is_empty() {
                        return Err(CursorStreamError::MissingArgumentValue { flag });
                    }
                    planned.push(argument.clone());
                    index += 1;
                } else {
                    let Some(value) = caller_args.get(index + 1) else {
                        return Err(CursorStreamError::MissingArgumentValue { flag });
                    };
                    if value == "--" {
                        return Err(CursorStreamError::MissingArgumentValue { flag });
                    }
                    planned.push(argument.clone());
                    planned.push(value.clone());
                    index += 2;
                }
            }
            None => {
                return Err(CursorStreamError::UnsupportedArgument);
            }
        }
    }

    planned.extend([
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "agent".to_owned(),
    ]);
    if index < caller_args.len() {
        planned.push("--".to_owned());
        planned.extend(caller_args[index..].iter().cloned());
    }
    Ok(planned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalOption {
    Switch,
    Value(&'static str),
    OptionalValue(&'static str),
}

fn supported_global_option(name: &str) -> Option<GlobalOption> {
    match name {
        "-f"
        | "--force"
        | "--yolo"
        | "--auto-review"
        | "--plan"
        | "--approve-mcps"
        | "--trust"
        | "--skip-worktree-setup" => Some(GlobalOption::Switch),
        "--api-key" => Some(GlobalOption::Value("--api-key")),
        "-H" | "--header" => Some(GlobalOption::Value("--header")),
        "-e" | "--endpoint" => Some(GlobalOption::Value("--endpoint")),
        "--mode" => Some(GlobalOption::Value("--mode")),
        "--model" => Some(GlobalOption::Value("--model")),
        "--sandbox" => Some(GlobalOption::Value("--sandbox")),
        "--workspace" => Some(GlobalOption::Value("--workspace")),
        "--add-dir" => Some(GlobalOption::Value("--add-dir")),
        "--plugin-dir" => Some(GlobalOption::Value("--plugin-dir")),
        "--worktree-base" => Some(GlobalOption::Value("--worktree-base")),
        "-w" | "--worktree" => Some(GlobalOption::OptionalValue("--worktree")),
        _ => None,
    }
}

fn is_attached_short_value(name: &str) -> bool {
    name.len() > 2
        && !name.starts_with("--")
        && name
            .get(..2)
            .is_some_and(|prefix| supported_global_option(prefix).is_some())
}

fn is_early_exit_option(name: &str) -> bool {
    matches!(name, "-h" | "--help" | "-v" | "--version" | "--list-models")
}

fn public_flag(argument: &str) -> String {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    if name.starts_with("--") {
        return name.to_owned();
    }
    name.chars().take(2).collect()
}

fn reserved_argument(argument: &str) -> Option<&'static str> {
    match argument {
        "-p" | "--print" => Some("--print"),
        value if value.starts_with("--print=") || value.starts_with("-p=") => Some("--print"),
        "--output-format" => Some("--output-format"),
        value if value.starts_with("--output-format=") => Some("--output-format"),
        "--stream-partial-output" => Some("--stream-partial-output"),
        value if value.starts_with("--stream-partial-output=") => Some("--stream-partial-output"),
        "--resume" => Some("--resume"),
        value if value.starts_with("--resume=") => Some("--resume"),
        "--continue" => Some("--continue"),
        value if value.starts_with("--continue=") => Some("--continue"),
        // Runs the agent inside tmux, so its stdout would not reach us.
        "--persist" => Some("--persist"),
        value if value.starts_with("--persist=") => Some("--persist"),
        _ => None,
    }
}

/// Read a Cursor stream and return its session facts.
///
/// The first non-blank line must be the `system`/`init` event with a
/// non-empty string `session_id`. Unknown event types and fields are accepted,
/// because real builds emit events the docs do not list. A final `result`
/// event is not required, because a failed run stops early. After the init
/// event, a malformed last line with no trailing newline is an unfinished
/// write and is ignored. A malformed line that ends in a newline is an error.
///
/// # Errors
///
/// Reading fails, a complete line is not a JSON object, the first event is not
/// a valid `system`/`init`, a later event repeats the init, or two events have
/// different session ids. Messages never include line content or session ids.
pub fn inspect(mut reader: impl BufRead) -> Result<CursorStreamSession, CursorStreamError> {
    let mut session_id: Option<String> = None;
    let mut cwd = None;
    let mut line_number = 0;

    loop {
        let next_line = line_number + 1;
        let mut line = Vec::new();
        let bytes_read =
            reader
                .read_until(b'\n', &mut line)
                .map_err(|source| CursorStreamError::Read {
                    line: next_line,
                    source,
                })?;
        if bytes_read == 0 {
            break;
        }
        line_number = next_line;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match apply_event(&line, line_number, &mut session_id, &mut cwd) {
            Err(CursorStreamError::Parse { .. })
                if !line.ends_with(b"\n") && session_id.is_some() =>
            {
                break;
            }
            result => result?,
        }
    }

    Ok(CursorStreamSession {
        session_id: session_id.ok_or(CursorStreamError::MissingInitialEvent)?,
        cwd,
    })
}

fn apply_event(
    line: &[u8],
    line_number: usize,
    session_id: &mut Option<String>,
    cwd: &mut Option<String>,
) -> Result<(), CursorStreamError> {
    let event: serde_json::Value =
        serde_json::from_slice(line).map_err(|source| CursorStreamError::Parse {
            line: line_number,
            source,
        })?;
    let serde_json::Value::Object(event) = event else {
        return Err(CursorStreamError::InvalidEvent {
            line: line_number,
            reason: "is not a JSON event object",
        });
    };

    let is_initial = string_field(&event, "type") == Some("system")
        && string_field(&event, "subtype") == Some("init");

    if session_id.is_none() {
        if !is_initial {
            return Err(CursorStreamError::InvalidEvent {
                line: line_number,
                reason: "is not the initial system/init event",
            });
        }
        let candidate = string_field(&event, "session_id")
            .filter(|candidate| !candidate.trim().is_empty())
            .ok_or(CursorStreamError::InvalidEvent {
                line: line_number,
                reason: "has no nonempty string session_id",
            })?;
        *session_id = Some(candidate.to_owned());
        *cwd = string_field(&event, "cwd").map(str::to_owned);
        return Ok(());
    }

    if is_initial {
        return Err(CursorStreamError::InvalidEvent {
            line: line_number,
            reason: "repeats the system/init event",
        });
    }

    if let Some(candidate) =
        string_field(&event, "session_id").filter(|candidate| !candidate.trim().is_empty())
        && let Some(expected) = session_id.as_deref()
        && expected != candidate
    {
        return Err(CursorStreamError::InvalidEvent {
            line: line_number,
            reason: "switches session_id",
        });
    }
    Ok(())
}

fn string_field<'event>(
    event: &'event serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<&'event str> {
    event.get(name).and_then(serde_json::Value::as_str)
}

/// Load one saved Cursor stream for upload.
///
/// The file is read once, so [`inspect`] and the records use the same bytes. An
/// unfinished last line passes [`inspect`] and is left out of the records.
///
/// # Errors
///
/// The errors of [`inspect`], plus an error when the file cannot be read or
/// the lines cannot be joined into a JSON array.
pub fn load(path: &Path) -> Result<CursorTranscript, CursorStreamError> {
    let bytes = std::fs::read(path).map_err(|source| CursorStreamError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let session = inspect(std::io::Cursor::new(&bytes))?;
    let records = RawValue::from_string(jsonl_to_records(&bytes)).map_err(|source| {
        CursorStreamError::PrepareRecords {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(CursorTranscript { session, records })
}

/// List the `.jsonl` files directly under `root`, sorted by path.
///
/// Only regular files are returned, and nothing is parsed. Call [`load`] on
/// each path so one bad file does not block the others.
///
/// # Errors
///
/// Returns an error when `root` cannot be read or an entry's type cannot be
/// read.
pub fn discover_candidates(root: &Path) -> Result<Vec<PathBuf>, CursorStreamError> {
    let discover_error = |source| CursorStreamError::Discover {
        root: root.to_path_buf(),
        source,
    };
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).map_err(discover_error)? {
        let entry = entry.map_err(discover_error)?;
        let path = entry.path();
        // DirEntry::file_type does not follow symlinks.
        if entry.file_type().map_err(discover_error)?.is_file()
            && path.extension() == Some(OsStr::new("jsonl"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const OFFICIAL_STREAM: &str = concat!(
        r#"{"type":"system","subtype":"init","cwd":"/workspace/demo","session_id":"sid-1","model":"m"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user"},"session_id":"sid-1"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant"},"session_id":"sid-1"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"sid-1"}"#,
        "\n",
    );

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn plan_args_selects_documented_stream_json_and_the_explicit_agent_command() {
        let planned = plan_args(&args(&["--trust", "--model", "gpt-x", "hello", "there"])).unwrap();
        assert_eq!(
            planned,
            args(&[
                "--trust",
                "--model",
                "gpt-x",
                "--print",
                "--output-format",
                "stream-json",
                "agent",
                "--",
                "hello",
                "there",
            ]),
        );
        let planned = plan_args(&args(&["hi"])).unwrap();
        assert!(!planned.contains(&"--trust".to_owned()));
    }

    #[test]
    fn plan_args_keeps_command_names_and_option_like_tokens_as_prompt_text() {
        let planned = plan_args(&args(&["resume"])).unwrap();
        assert_eq!(
            planned,
            args(&[
                "--print",
                "--output-format",
                "stream-json",
                "agent",
                "--",
                "resume"
            ]),
        );
        let planned = plan_args(&args(&["explain", "--resume"])).unwrap();
        assert_eq!(planned[planned.len() - 2..], args(&["explain", "--resume"]));
        let planned = plan_args(&args(&["--", "--resume"])).unwrap();
        assert_eq!(planned[planned.len() - 2..], args(&["--", "--resume"]));
    }

    #[test]
    fn plan_args_refuses_stream_session_unknown_and_early_exit_options() {
        for reserved in [
            "--print",
            "-p",
            "-p=abc123",
            "--output-format",
            "--output-format=json",
            "--stream-partial-output",
            "--resume",
            "--resume=abc123",
            "--continue",
            "--persist",
        ] {
            let error = plan_args(&args(&[reserved, "hi"])).unwrap_err();
            assert!(
                matches!(error, CursorStreamError::ConflictingArgument { .. }),
                "{reserved}: {error}",
            );
            assert!(!error.to_string().contains("abc123"), "{error}");
        }
        for unsupported in ["--unknown-option", "--help", "--version", "--list-models"] {
            let error = plan_args(&args(&[unsupported, "hi"])).unwrap_err();
            assert!(
                matches!(error, CursorStreamError::UnsupportedArgument),
                "{unsupported}: {error}",
            );
        }
        let error = plan_args(&args(&["-éSECRET", "hi"])).unwrap_err();
        assert!(
            matches!(error, CursorStreamError::UnsupportedArgument),
            "{error}",
        );
        assert!(!error.to_string().contains("SECRET"), "{error}");
        for form in [
            "--trust=false",
            "-Hx-key:1",
            "-fH",
            "-e=http://x-key",
            "-H=x-key",
        ] {
            let error = plan_args(&args(&[form, "hi"])).unwrap_err();
            assert!(
                matches!(error, CursorStreamError::UnsupportedArgumentForm { .. }),
                "{form}: {error}",
            );
            assert!(!error.to_string().contains("x-key"), "{error}");
        }
    }

    #[test]
    fn plan_args_errors_do_not_echo_unknown_options() {
        for argument in ["--PRIVATE_CURSOR_PROMPT", "--SECRET=value", "-éSECRET"] {
            let error = plan_args(&args(&[argument, "prompt"])).unwrap_err();
            assert_eq!(error.to_string(), "unsupported cursor option");
            assert_eq!(format!("{error:?}"), "UnsupportedArgument");
        }
    }

    #[test]
    fn plan_args_takes_a_worktree_name_the_way_cursor_does() {
        let tail = |planned: &[String]| planned[..planned.len() - 6].to_vec();
        assert_eq!(
            tail(&plan_args(&args(&["--worktree", "feature", "hi"])).unwrap()),
            args(&["--worktree", "feature"]),
        );
        assert_eq!(
            tail(&plan_args(&args(&["-w", "--trust", "hi"])).unwrap()),
            args(&["-w", "--trust"]),
        );
        assert_eq!(
            tail(&plan_args(&args(&["--worktree=feature", "hi"])).unwrap()),
            args(&["--worktree=feature"]),
        );
        let planned = plan_args(&args(&["--worktree", "--", "hi"])).unwrap();
        assert_eq!(planned[0], "--worktree");
        assert_eq!(planned[planned.len() - 2..], args(&["--", "hi"]));
        assert!(matches!(
            plan_args(&args(&["--worktree=", "hi"])).unwrap_err(),
            CursorStreamError::MissingArgumentValue { .. },
        ));
    }

    #[test]
    fn plan_args_requires_values_without_leaking_them() {
        for missing in [
            &["--model"][..],
            &["--model", "--"][..],
            &["--api-key="][..],
        ] {
            let error = plan_args(&args(missing)).unwrap_err();
            assert!(
                matches!(error, CursorStreamError::MissingArgumentValue { .. }),
                "{missing:?}: {error}",
            );
        }
        let error = plan_args(&args(&["--api-key=secret-value", "--print", "hi"])).unwrap_err();
        assert!(!error.to_string().contains("secret-value"), "{error}");
    }

    #[test]
    fn inspect_recovers_identity_without_requiring_a_terminal_result() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","cwd":"/w","session_id":"sid-9"}"#,
            "\n",
            r#"{"type":"assistant","session_id":"sid-9"}"#,
            "\n",
        );
        let session = inspect(stream.as_bytes()).unwrap();
        assert_eq!(session.session_id, "sid-9");
        assert_eq!(session.cwd.as_deref(), Some("/w"));
    }

    #[test]
    fn inspect_accepts_unknown_event_kinds_and_additive_fields() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sid-2"}"#,
            "\n",
            r#"{"type":"thinking","delta":"...","session_id":"sid-2"}"#,
            "\n",
            r#"{"type":"totally-new-kind","payload":{"a":1}}"#,
            "\n",
        );
        assert_eq!(inspect(stream.as_bytes()).unwrap().session_id, "sid-2");
    }

    #[test]
    fn inspect_rejects_streams_that_violate_the_documented_shape() {
        let cases: &[(&str, &str)] = &[
            (r#"{"type":"assistant","session_id":"sid"}"#, "initial"),
            (r#"{"type":"system","subtype":"init"}"#, "session_id"),
            (
                r#"{"type":"system","subtype":"init","session_id":"  "}"#,
                "session_id",
            ),
            ("[1,2]", "object"),
            (
                concat!(
                    r#"{"type":"system","subtype":"init","session_id":"sid"}"#,
                    "\n",
                    r#"{"type":"system","subtype":"init","session_id":"sid"}"#,
                ),
                "repeats",
            ),
            (
                concat!(
                    r#"{"type":"system","subtype":"init","session_id":"sid"}"#,
                    "\n",
                    r#"{"type":"assistant","session_id":"other"}"#,
                    "\n",
                ),
                "switches",
            ),
        ];
        for (stream, expected) in cases {
            let error = inspect(stream.as_bytes()).unwrap_err();
            assert!(error.to_string().contains(expected), "{stream}: {error}");
            assert!(error.is_invalid_stream(), "{stream}");
        }
        assert!(matches!(
            inspect(b"\n  \n".as_slice()).unwrap_err(),
            CursorStreamError::MissingInitialEvent,
        ));
    }

    #[test]
    fn only_an_unterminated_malformed_tail_is_torn() {
        let init = r#"{"type":"system","subtype":"init","session_id":"sid-3"}"#;
        let torn = format!("{init}\n{{\"type\":\"assist");
        assert_eq!(inspect(torn.as_bytes()).unwrap().session_id, "sid-3");

        let torn_mid_codepoint = [torn.as_bytes(), &[0xE2, 0x82]].concat();
        assert_eq!(
            inspect(torn_mid_codepoint.as_slice()).unwrap().session_id,
            "sid-3"
        );

        let durable = format!("{init}\n{{\"type\":\"assist\n");
        assert!(matches!(
            inspect(durable.as_bytes()).unwrap_err(),
            CursorStreamError::Parse { line: 2, .. },
        ));

        assert!(inspect(b"{\"type\":\"sys".as_slice()).is_err());
    }

    #[test]
    fn load_prepares_the_main_upload_payload_from_one_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");
        std::fs::write(&path, OFFICIAL_STREAM).unwrap();

        let transcript = load(&path).unwrap();
        let payload = transcript.payload("org-1", "local:me");
        let body: Value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            body["session"],
            json!({
                "org_id": "org-1",
                "auth_subject": "local:me",
                "harness_id": "cursor",
                "harness_session_id": "sid-1",
                "cwd": "/workspace/demo",
            }),
        );
        let records = body["records"].as_array().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[3]["type"], "result");
    }

    #[test]
    fn load_omits_a_torn_tail_from_the_prepared_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");
        std::fs::write(&path, format!("{OFFICIAL_STREAM}{{\"type\":\"tor")).unwrap();

        let transcript = load(&path).unwrap();
        let body: Value = serde_json::to_value(transcript.payload("", "")).unwrap();
        assert_eq!(body["records"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn load_keeps_a_complete_last_line_that_has_no_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");
        std::fs::write(&path, OFFICIAL_STREAM.trim_end_matches('\n')).unwrap();

        let transcript = load(&path).unwrap();
        let body: Value = serde_json::to_value(transcript.payload("", "")).unwrap();
        let records = body["records"].as_array().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[3]["type"], "result");
    }

    #[test]
    fn load_reports_an_unreadable_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load(&dir.path().join("missing.jsonl")).unwrap_err(),
            CursorStreamError::Open { .. },
        ));
    }

    #[test]
    fn discover_candidates_returns_only_direct_regular_jsonl_files_in_path_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.jsonl"), "x").unwrap();
        std::fs::write(dir.path().join("a.jsonl"), "x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("nested.jsonl")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("a.jsonl"), dir.path().join("link.jsonl"))
            .unwrap();

        let names = discover_candidates(dir.path())
            .unwrap()
            .into_iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a.jsonl", "b.jsonl"]);

        assert!(matches!(
            discover_candidates(&dir.path().join("absent")).unwrap_err(),
            CursorStreamError::Discover { .. },
        ));
    }
}
