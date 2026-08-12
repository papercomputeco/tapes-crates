//! The Codex desktop app's lifecycle-hook evidence lane.
//!
//! The desktop app is a Codex host a consumer *configures* rather than
//! launches: provider traffic is redirected by the app's own `config.toml`,
//! and there is no launched PID for [`tapes_capture::peer_trust`] to
//! anchor on and no per-launch environment to carry a capture nonce. What the
//! app does offer is a hook surface — a plugin the user installs and trusts
//! runs a consumer-supplied command at session, prompt, stop, and subagent
//! lifecycle boundaries, feeding that command an allowlisted JSON description
//! of the boundary on stdin.
//!
//! This module owns the harness half of that contract: the shape of the JSON
//! Codex writes to the hook command, parsed into an allowlisted
//! [`LifecycleObservation`]. What a consumer *does* with an observation —
//! which process it reports to, how the report is authenticated, what runtime
//! state it updates — is deployment knowledge and stays with the consumer,
//! exactly as delivery and retry do for transcripts.
//!
//! # The identity vocabulary
//!
//! The fields here are the lifecycle-boundary spelling of the same identities
//! the wire lane reads from request headers:
//!
//! * `session_id` is the **root** Codex session — the identity
//!   [`crate::attribution::codex::session::CODEX_ROLLOUT_ID_HEADERS`] narrows
//!   to on a request, and the one a captured session is keyed by. On
//!   `SubagentStart`/`SubagentStop` it stays pinned to the root; the child is
//!   named separately.
//! * `agent_id` is the child thread's own identity — the lifecycle counterpart
//!   of a sub-thread request's `thread-id` header, the value
//!   [`tapes_capture::envelope::thread_id`] resolves and ingest lands in
//!   `meta.thread_id`.
//! * `turn_id` bounds one root turn; `SubagentStart`/`SubagentStop` carry the
//!   root turn their child ran under, which is what joins a child's wire
//!   traffic back to the prompt that spawned it.
//!
//! Every identifier is preserved as an exact, opaque string: matching is
//! equality against evidence from the other lanes, never interpretation.
//!
//! # Allowlist, not schema
//!
//! Hook payloads also carry the user's prompt, assistant output, and arbitrary
//! extension JSON. None of that may survive parsing — an observation exists to
//! attribute traffic, not to duplicate its content into consumer logs and
//! control sockets. [`parse_observation`] therefore deserializes into structs
//! that name only the allowlisted fields and silently discard the rest, and
//! the tests pin that sensitive neighbours do not outlive the parse.

use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};

/// The lifecycle events the hook contract subscribes to, in the order the
/// manifest template declares them.
///
/// These are Codex's own `hook_event_name` spellings. The list is the single
/// source for [`crate::plugin::codex_app`]'s manifest template — a test there
/// pins the template's keys to exactly this set, so the events a rendered
/// plugin subscribes to and the events [`parse_observation`] accepts cannot
/// drift apart.
pub const LIFECYCLE_EVENTS: &[&str] = &[
    "SessionStart",
    "SubagentStart",
    "SubagentStop",
    "UserPromptSubmit",
    "Stop",
];

/// One allowlisted lifecycle boundary, as reported by a hook invocation.
///
/// The common fields describe the *root* session the boundary belongs to;
/// [`Self::event`] carries the boundary-specific identity. Everything is kept
/// as the exact opaque string Codex supplied.
///
/// `Deserialize` as well as `Serialize`, because an observation is parsed in
/// one process and acted on in another: the hook Codex runs is a short-lived
/// child, and the capture that needs the boundary is the long-lived proxy. A
/// consumer that could only serialize had to declare a mirror of this type on
/// the receiving end and hand-maintain the translation, which is one
/// vocabulary in two spellings — exactly what this crate exists to prevent.
/// The round trip is part of the contract; the tests pin it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LifecycleObservation {
    /// The root Codex session id. Opaque; equality-matched against rollout
    /// `session_meta` ids and the wire lane's session evidence.
    pub session_id: String,
    /// The root session's rollout path, when the hook supplies one. Absence
    /// is normal and must read as "no evidence", not "no transcript".
    pub transcript_path: Option<String>,
    /// Working directory reported by Codex.
    pub cwd: String,
    /// Active model reported by Codex, when present.
    pub model: Option<String>,
    /// Which boundary this is, with its event-specific identity.
    pub event: LifecycleEvent,
}

/// The boundary-specific half of a [`LifecycleObservation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LifecycleEvent {
    /// A session began (or resumed, cleared, or compacted — see
    /// [`SessionStartSource`]).
    SessionStart {
        /// Why the session-start boundary fired.
        source: SessionStartSource,
    },
    /// The user submitted a prompt, opening root turn `turn_id`.
    UserPromptSubmit {
        /// The opened turn's opaque id.
        turn_id: String,
    },
    /// Root turn `turn_id` completed.
    Stop {
        /// The completed turn's opaque id.
        turn_id: String,
    },
    /// A subagent spawned under root turn `turn_id`.
    SubagentStart {
        /// The root turn the child runs under.
        turn_id: String,
        /// The child thread's own opaque id — the lifecycle counterpart of a
        /// sub-thread request's `thread-id` header.
        agent_id: String,
        /// Codex-reported agent type, opaque.
        agent_type: String,
    },
    /// A subagent finished under root turn `turn_id`.
    SubagentStop {
        /// The root turn the child ran under.
        turn_id: String,
        /// The child thread's own opaque id.
        agent_id: String,
        /// Codex-reported agent type, opaque.
        agent_type: String,
        /// The child's own rollout path, when Codex supplies one. A missing
        /// path is valid; request/transcript matching can still join later.
        agent_transcript_path: Option<String>,
    },
}

/// Codex-reported reason a `SessionStart` hook ran.
///
/// An allowlist like everything else here: a source spelling this crate does
/// not know is a parse error, not a passthrough — new lifecycle semantics
/// should be adopted deliberately, with this enum extended, rather than
/// flowing through as an uninterpreted string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    Compact,
}

/// The raw hook stdin shape: Codex tags the payload with `hook_event_name`
/// and flattens the common fields beside the event-specific ones. Unknown
/// fields — including the prompt and assistant output — are dropped here by
/// construction, because no struct names them.
#[derive(Deserialize)]
#[serde(tag = "hook_event_name")]
enum HookInput {
    SessionStart {
        session_id: String,
        transcript_path: Option<String>,
        cwd: String,
        model: Option<String>,
        source: SessionStartSource,
    },
    UserPromptSubmit {
        session_id: String,
        transcript_path: Option<String>,
        cwd: String,
        model: Option<String>,
        turn_id: String,
    },
    Stop {
        session_id: String,
        transcript_path: Option<String>,
        cwd: String,
        model: Option<String>,
        turn_id: String,
    },
    SubagentStart {
        session_id: String,
        transcript_path: Option<String>,
        cwd: String,
        model: Option<String>,
        turn_id: String,
        agent_id: String,
        agent_type: String,
    },
    SubagentStop {
        session_id: String,
        transcript_path: Option<String>,
        cwd: String,
        model: Option<String>,
        turn_id: String,
        agent_id: String,
        agent_type: String,
        agent_transcript_path: Option<String>,
    },
}

/// Parse one hook invocation's stdin into an allowlisted observation.
///
/// Fails on anything that is not a recognised lifecycle event — a hook
/// command receiving an event outside [`LIFECYCLE_EVENTS`], or a payload
/// missing a required identity field, has nothing safe to report and the
/// consumer's hook must stay non-blocking about it (surface a warning,
/// exit successfully).
pub fn parse_observation(input: &[u8]) -> Result<LifecycleObservation, ParseObservationError> {
    let parsed: HookInput =
        serde_json::from_slice(input).context(parse_observation_error::ParseSnafu)?;
    Ok(match parsed {
        HookInput::SessionStart {
            session_id,
            transcript_path,
            cwd,
            model,
            source,
        } => LifecycleObservation {
            session_id,
            transcript_path,
            cwd,
            model,
            event: LifecycleEvent::SessionStart { source },
        },
        HookInput::UserPromptSubmit {
            session_id,
            transcript_path,
            cwd,
            model,
            turn_id,
        } => LifecycleObservation {
            session_id,
            transcript_path,
            cwd,
            model,
            event: LifecycleEvent::UserPromptSubmit { turn_id },
        },
        HookInput::Stop {
            session_id,
            transcript_path,
            cwd,
            model,
            turn_id,
        } => LifecycleObservation {
            session_id,
            transcript_path,
            cwd,
            model,
            event: LifecycleEvent::Stop { turn_id },
        },
        HookInput::SubagentStart {
            session_id,
            transcript_path,
            cwd,
            model,
            turn_id,
            agent_id,
            agent_type,
        } => LifecycleObservation {
            session_id,
            transcript_path,
            cwd,
            model,
            event: LifecycleEvent::SubagentStart {
                turn_id,
                agent_id,
                agent_type,
            },
        },
        HookInput::SubagentStop {
            session_id,
            transcript_path,
            cwd,
            model,
            turn_id,
            agent_id,
            agent_type,
            agent_transcript_path,
        } => LifecycleObservation {
            session_id,
            transcript_path,
            cwd,
            model,
            event: LifecycleEvent::SubagentStop {
                turn_id,
                agent_id,
                agent_type,
                agent_transcript_path,
            },
        },
    })
}

/// Hook stdin could not be decoded into an allowlisted lifecycle observation.
#[derive(Debug, Snafu)]
#[snafu(module)]
#[non_exhaustive]
pub enum ParseObservationError {
    /// The input was not a recognised lifecycle event payload.
    #[snafu(display("hook input is not a recognised Codex lifecycle event"))]
    Parse { source: serde_json::Error },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_session_start_parses_with_its_source() {
        let input = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "session/opaque-🧭",
            "transcript_path": "/tmp/codex sessions/opaque.jsonl",
            "cwd": "/tmp/work tree",
            "model": "gpt-5-codex",
            "source": "startup"
        })
        .to_string();

        let observation = parse_observation(input.as_bytes()).unwrap();
        assert_eq!(observation.session_id, "session/opaque-🧭");
        assert_eq!(
            observation.transcript_path.as_deref(),
            Some("/tmp/codex sessions/opaque.jsonl")
        );
        assert_eq!(observation.cwd, "/tmp/work tree");
        assert_eq!(observation.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(
            observation.event,
            LifecycleEvent::SessionStart {
                source: SessionStartSource::Startup
            }
        );
    }

    /// The prompt is the canonical sensitive neighbour: it arrives in the same
    /// payload and must not survive into anything an observation serializes.
    #[test]
    fn a_prompt_submit_keeps_the_turn_id_and_discards_the_prompt() {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session/opaque-🧭",
            "transcript_path": "/tmp/codex sessions/opaque.jsonl",
            "cwd": "/tmp/work tree",
            "model": "gpt-5-codex",
            "turn_id": "turn/opaque-🧵",
            "prompt": "prompt-secret-must-never-escape"
        })
        .to_string();

        let observation = parse_observation(input.as_bytes()).unwrap();
        assert_eq!(
            observation.event,
            LifecycleEvent::UserPromptSubmit {
                turn_id: "turn/opaque-🧵".to_owned()
            }
        );
        let retained = serde_json::to_string(&observation).unwrap();
        assert!(!retained.contains("prompt-secret-must-never-escape"));
    }

    /// On subagent boundaries the root/child split is the whole point:
    /// `session_id` stays the parent, `agent_id` names the child, and
    /// assistant output plus arbitrary extension JSON disappear.
    #[test]
    fn a_subagent_stop_keeps_root_and_child_identity_and_nothing_else() {
        let input = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent/opaque-🧭",
            "transcript_path": "/tmp/codex sessions/parent.jsonl",
            "cwd": "/tmp/work tree",
            "model": "gpt-5-codex",
            "turn_id": "turn/opaque-🧵",
            "agent_id": "agent/opaque-🛡️",
            "agent_type": "guardian/custom",
            "agent_transcript_path": "/tmp/codex children/agent.jsonl",
            "last_assistant_message": "sensitive assistant output must disappear",
            "arbitrary": {"nested": "sensitive arbitrary value"}
        })
        .to_string();

        let observation = parse_observation(input.as_bytes()).unwrap();
        assert_eq!(observation.session_id, "parent/opaque-🧭");
        assert_eq!(
            observation.event,
            LifecycleEvent::SubagentStop {
                turn_id: "turn/opaque-🧵".to_owned(),
                agent_id: "agent/opaque-🛡️".to_owned(),
                agent_type: "guardian/custom".to_owned(),
                agent_transcript_path: Some("/tmp/codex children/agent.jsonl".to_owned()),
            }
        );
        let retained = serde_json::to_string(&observation).unwrap();
        assert!(!retained.contains("sensitive assistant output"));
        assert!(!retained.contains("sensitive arbitrary value"));
    }

    /// A missing child transcript path is a valid stop, not an error — the
    /// join can still happen through request/transcript matching later.
    #[test]
    fn a_subagent_stop_accepts_a_null_child_transcript_path() {
        let input = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "parent/opaque-🧭",
            "cwd": "/tmp/work tree",
            "turn_id": "turn/opaque-🧵",
            "agent_id": "agent/opaque-🛡️",
            "agent_type": "guardian/custom",
            "agent_transcript_path": null
        })
        .to_string();

        let observation = parse_observation(input.as_bytes()).unwrap();
        let LifecycleEvent::SubagentStop {
            agent_transcript_path,
            ..
        } = observation.event
        else {
            panic!("expected a subagent stop");
        };
        assert_eq!(agent_transcript_path, None);
        assert_eq!(observation.transcript_path, None);
        assert_eq!(observation.model, None);
    }

    #[test]
    fn a_subagent_start_keeps_the_parent_session_and_exact_child_fields() {
        let input = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "parent/opaque-🧭",
            "cwd": "/tmp/work tree",
            "turn_id": "turn/opaque-🧵",
            "agent_id": "agent/opaque-🛡️",
            "agent_type": "guardian/custom",
            "permission_mode": "default"
        })
        .to_string();

        let observation = parse_observation(input.as_bytes()).unwrap();
        assert_eq!(observation.session_id, "parent/opaque-🧭");
        assert_eq!(
            observation.event,
            LifecycleEvent::SubagentStart {
                turn_id: "turn/opaque-🧵".to_owned(),
                agent_id: "agent/opaque-🛡️".to_owned(),
                agent_type: "guardian/custom".to_owned(),
            }
        );
    }

    /// The allowlist is closed in both directions: an event this crate does
    /// not know, or a session-start source it does not know, is a parse
    /// error rather than a silently degraded observation.
    #[test]
    fn unknown_events_and_sources_are_refused() {
        let unknown_event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session/opaque",
            "cwd": "/tmp"
        })
        .to_string();
        assert!(parse_observation(unknown_event.as_bytes()).is_err());

        let unknown_source = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "session/opaque",
            "cwd": "/tmp",
            "source": "telepathy"
        })
        .to_string();
        assert!(parse_observation(unknown_source.as_bytes()).is_err());

        assert!(parse_observation(b"not json at all").is_err());
    }

    /// Every event the manifest template subscribes to must be one this
    /// parser accepts, spelled identically — [`LIFECYCLE_EVENTS`] is the
    /// bridge, so it has to cover the enum exactly.
    #[test]
    fn the_lifecycle_event_list_matches_the_parsed_events() {
        for event in LIFECYCLE_EVENTS {
            let mut payload = serde_json::json!({
                "hook_event_name": event,
                "session_id": "s",
                "cwd": "/tmp",
                "turn_id": "t",
                "agent_id": "a",
                "agent_type": "kind",
            });
            if *event == "SessionStart" {
                payload["source"] = serde_json::json!("startup");
            }
            assert!(
                parse_observation(payload.to_string().as_bytes()).is_ok(),
                "{event} is listed but not parseable"
            );
        }
        assert_eq!(LIFECYCLE_EVENTS.len(), 5);
    }

    /// Every boundary survives a serialize/deserialize round trip unchanged.
    ///
    /// The observation is produced by a short-lived hook process and consumed
    /// by a long-lived capture, so the transport is not optional — and a
    /// consumer that has to restate the vocabulary to cross that gap has a
    /// second place for it to drift. Asserting equality over the whole
    /// enum rather than field-by-field is what makes a newly added variant or
    /// field fail here instead of silently dropping in transit.
    #[test]
    fn an_observation_survives_a_round_trip_through_json() {
        for event in LIFECYCLE_EVENTS {
            let mut payload = serde_json::json!({
                "hook_event_name": event,
                "session_id": "s",
                "transcript_path": "/tmp/rollout.jsonl",
                "cwd": "/tmp",
                "model": "gpt-5",
                "turn_id": "t",
                "agent_id": "a",
                "agent_type": "kind",
                "agent_transcript_path": "/tmp/child.jsonl",
            });
            if *event == "SessionStart" {
                payload["source"] = serde_json::json!("compact");
            }
            let observation = parse_observation(payload.to_string().as_bytes()).unwrap();

            let wire = serde_json::to_string(&observation).unwrap();
            let returned: LifecycleObservation = serde_json::from_str(&wire).unwrap();

            assert_eq!(returned, observation, "{event} did not survive the trip");
        }
    }
}
