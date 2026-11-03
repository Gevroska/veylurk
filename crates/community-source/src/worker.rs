//! Private, bounded process protocol. No request/response body, URL, cookie,
//! credential or arbitrary diagnostic text is admitted to this transport.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::host::{FaultReport, Member, SourceFailure, SourceSample, ROLE_MASK};
use crate::sampling::Provenance;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

#[cfg(windows)]
mod process;
#[cfg(windows)]
pub(crate) use process::spawn;

const WORKER_ARGUMENT: &str = "--veylurk-private-community-worker-v1";
const VERSION: u32 = 1;
const FRAME_CAP: usize = 512 * 1024;
// Serde treats a syntactic &str field as borrowed even with deserialize_with.
// This transparent alias lets the explicit vocabulary decoder own the decision;
// no input buffer lifetime or leaked allocation becomes part of a public value.
pub(crate) type StructuralCode = &'static str;
pub(crate) const WEBVIEW_ENVIRONMENT_OVERRIDES: [&str; 6] = [
    "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
    "WEBVIEW2_USER_DATA_FOLDER",
    "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
    "WEBVIEW2_RELEASE_CHANNEL_PREFERENCE",
    "WEBVIEW2_CHANNEL_SEARCH_KIND",
    "WEBVIEW2_RELEASE_CHANNELS",
];

pub(crate) fn dispatch() -> Option<std::process::ExitCode> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARGUMENT)) {
        return None;
    }
    if arguments.next().is_some() {
        return Some(std::process::ExitCode::from(2));
    }
    #[cfg(windows)]
    return Some(process::run_worker());
    #[cfg(not(windows))]
    Some(std::process::ExitCode::from(2))
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum ParentFrame {
    Start {
        version: u32,
        profile_dir: PathBuf,
    },
    Control {
        cancel_version: u64,
        shutdown: bool,
    },
    Sample {
        ticket: u64,
        job_id: u64,
        channel: String,
        cancel_version: u64,
        queued_tick_ms: u64,
    },
    Login {
        publication: u64,
        cancel_version: u64,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
enum WorkerFrame {
    Pulse {
        native_progress: u64,
        diagnostics: FaultReport,
    },
    Ready,
    LoginShown,
    LoginFailed,
    Paused {
        #[serde(deserialize_with = "fixed_string")]
        code: StructuralCode,
    },
    Completed {
        ticket: u64,
        job_id: u64,
        channel: String,
        outcome: WireOutcome,
    },
    Stopped {
        cleanup_confirmed: bool,
        diagnostics: FaultReport,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
enum WireOutcome {
    Sample {
        sample: WireSample,
    },
    Failure {
        #[serde(deserialize_with = "fixed_string")]
        code: StructuralCode,
        pauses_community: bool,
    },
}

/// Account serialization is confined to this private pipe payload. Neither
/// SourceSample nor this type implements Debug, and no frame is ever logged.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireMember {
    account: String,
    roles: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireSample {
    received_at_unix_ms: i64,
    reported_chatters: Option<u64>,
    members: Vec<WireMember>,
    category_counts: [u64; 6],
    http_status: u16,
    provenance: Provenance,
}

impl WireSample {
    fn from_source(sample: SourceSample) -> Self {
        Self {
            received_at_unix_ms: sample.received_at_unix_ms,
            reported_chatters: sample.reported_chatters,
            members: sample
                .members
                .into_iter()
                .map(|m| WireMember {
                    account: m.account,
                    roles: m.roles,
                })
                .collect(),
            category_counts: sample.category_counts,
            http_status: sample.http_status,
            provenance: sample.provenance,
        }
    }
    fn into_source(self) -> io::Result<SourceSample> {
        let finite = [
            self.provenance.raw_encoded_data_length,
            self.provenance.request_monotonic_seconds,
            self.provenance.response_monotonic_seconds,
            self.provenance.finished_monotonic_seconds,
        ]
        .into_iter()
        .all(f64::is_finite)
            && [
                self.provenance.request_after_trigger_ms,
                self.provenance.request_to_response_ms,
                self.provenance.request_to_finished_ms,
                self.provenance.finished_minus_response_ms,
            ]
            .into_iter()
            .flatten()
            .all(f64::is_finite);
        let mut accounts = std::collections::HashSet::new();
        if self.members.len() > crate::MAX_MEMBERS
            || self.http_status != 200
            || !finite
            || self.received_at_unix_ms <= 0
            || self
                .category_counts
                .iter()
                .try_fold(0u64, |sum, n| sum.checked_add(*n))
                .is_none_or(|sum| sum > crate::MAX_MEMBERS as u64)
            || self.members.iter().any(|member| {
                crate::channel_login(&member.account).as_ref() != Some(&member.account)
                    || member.roles == 0
                    || member.roles & !ROLE_MASK != 0
                    || !accounts.insert(member.account.as_str())
            })
        {
            return Err(protocol_error());
        }
        Ok(SourceSample {
            received_at_unix_ms: self.received_at_unix_ms,
            reported_chatters: self.reported_chatters,
            members: self
                .members
                .into_iter()
                .map(|m| Member {
                    account: m.account,
                    roles: m.roles,
                })
                .collect(),
            category_counts: self.category_counts,
            http_status: self.http_status,
            provenance: self.provenance,
        })
    }
}

impl WireOutcome {
    fn from_source(outcome: Result<SourceSample, SourceFailure>) -> Self {
        match outcome {
            Ok(sample) => Self::Sample {
                sample: WireSample::from_source(sample),
            },
            Err(failure) => Self::Failure {
                code: failure.code,
                pauses_community: failure.pauses_community,
            },
        }
    }
    fn into_source(self) -> io::Result<Result<SourceSample, SourceFailure>> {
        match self {
            Self::Sample { sample } => sample.into_source().map(Ok),
            Self::Failure {
                code,
                pauses_community,
            } => Ok(Err(SourceFailure {
                code,
                pauses_community,
            })),
        }
    }
}

fn protocol_error() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut prefix = [0; 4];
    // Distinguish an orderly pipe end from a truncated prefix or payload.
    if reader.read(&mut prefix[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..])?;
    let size = u32::from_le_bytes(prefix) as usize;
    if size == 0 || size > FRAME_CAP {
        return Err(protocol_error());
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|_| protocol_error())
}

struct BoundedBuffer(Vec<u8>);
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > FRAME_CAP.saturating_sub(self.0.len()) {
            return Err(protocol_error());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut payload = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut payload, value).map_err(|_| protocol_error())?;
    writer.write_all(&(payload.0.len() as u32).to_le_bytes())?;
    writer.write_all(&payload.0)?;
    writer.flush()
}

/// Static strings are reconstructed from a fixed vocabulary, never leaked or
/// interned from worker input. Unknown diagnostic text fails the protocol closed.
pub(crate) fn fixed_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<&'static str, D::Error> {
    let value = String::deserialize(deserializer)?;
    FIXED_STRINGS
        .iter()
        .copied()
        .find(|known| *known == value)
        .ok_or_else(|| serde::de::Error::custom("unknown structural code"))
}

const FIXED_STRINGS: &[&str] = &[
    "",
    "aggregate_write_failed",
    "ambiguous_native_source",
    "ambiguous_source",
    "armed",
    "authentication",
    "authentication_denied",
    "autonomous_ready",
    "awaiting_native_community",
    "batch_limit",
    "batch_mismatch",
    "broadcasters",
    "browser_process_exited",
    "browser_process_failed",
    "browser_running",
    "cache_or_service_worker",
    "captcha",
    "challenge",
    "challenge_denied",
    "chatbots",
    "cleanup_unconfirmed",
    "circuits_completed_with_gaps",
    "circuits_completed_with_samples",
    "circuits_must_be_1_to_3",
    "closed",
    "closing",
    "community_control_unavailable",
    "community_panel_open",
    "community_panel_phase_mismatch",
    "community_panel_transition",
    "community_panel_unverified",
    "community_panel_waiting",
    "community_paused",
    "community_restore_clicked",
    "community_restore_control_unavailable",
    "community_restore_deadline",
    "community_restore_panel_mismatch",
    "community_trigger_deadline",
    "community_ui_action_failed",
    "community_ui_result_invalid",
    "controller_cleanup_unconfirmed",
    "controller_creation_failed",
    "count",
    "data",
    "denied",
    "duplicate_native_response",
    "enabling",
    "environment_creation_failed",
    "errors",
    "existing_profile_required",
    "exit_not_confirmed_within_5_seconds",
    "expected_autonomous_mode",
    "false",
    "finish_at_or_after_response_event",
    "finish_before_response_event",
    "finite_autonomous_popout_chat",
    "forbidden",
    "host_failed",
    "http_denied",
    "http_error",
    "idle",
    "idle_pause_guard_deadline",
    "initializing",
    "integrity",
    "integrity_denied",
    "invalid_channel",
    "invalid_circuits",
    "invalid_json",
    "invalid_native_request_time",
    "invalid_runtime",
    "invalid_shape",
    "invalid_trigger_deadline",
    "invalid_trigger_time",
    "kamet0",
    "local_render_check",
    "local_render_navigation_failed",
    "login",
    "manual_login_required",
    "member_limit",
    "missing_argument_value",
    "missing_chatters",
    "missing_native_frame",
    "missing_pending_sample",
    "missing_request_tracker",
    "missing_trigger_time",
    "moderators",
    "native_body_encoding_unsupported",
    "native_body_identity_mismatch",
    "native_body_identity_unavailable",
    "native_body_invalid",
    "native_body_unavailable",
    "native_candidate_limit",
    "native_community_clicked",
    "native_encoded_length_invalid_negative",
    "native_encoded_length_nonfinite",
    "native_event_count_limit",
    "native_event_invalid",
    "native_event_size_limit",
    "native_event_unavailable",
    "native_finished_before_request",
    "native_finished_timestamp_nonfinite",
    "native_frame_unavailable",
    "native_hide_unverified",
    "native_host_interrupted",
    "native_host_start_failed",
    "native_loading_failed",
    "native_minimize_unverified",
    "native_observer_failed",
    "native_request_body_unavailable",
    "native_request_id_limit",
    "native_response_identity_mismatch",
    "native_root_frame_mismatch",
    "native_sample_timeout",
    "native_source_observed",
    "native_twitch_community_webview2",
    "navigating",
    "navigation_changed",
    "navigation_failed",
    "navigation_ui_deadline",
    "network_disable_failed",
    "network_observer_enable_failed",
    "network_response",
    "no_browser_cleanup_required",
    "none",
    "not_started",
    "null",
    "observed",
    "observer_setup_failed",
    "operator_cancelled",
    "origin",
    "pause_or_readiness_lost",
    "player_reverification_deadline",
    "preparing",
    "profile_directory_unavailable",
    "rate_limit",
    "rate_limited",
    "ready",
    "remove_document_loaded",
    "remove_navigation_completed",
    "remove_navigation_starting",
    "remove_new_window",
    "remove_process_failed",
    "remove_protocol_event",
    "remove_source_changed",
    "request_size_limit",
    "request_stream_unavailable",
    "response_size_limit",
    "response_stream_unavailable",
    "reused_native_request_id",
    "run_deadline",
    "running",
    "runtime_missing",
    "runtime_must_be_60_to_600_seconds",
    "runtime_start_deadline",
    "runtime_start_failed",
    "sample_cancelled",
    "sample_complete",
    "sample_failed",
    "sample_queue_deadline",
    "sample_received",
    "sampling",
    "settings",
    "sign_in",
    "signed_in_ui_lost",
    "signup",
    "snake_case",
    "source_denied",
    "source_errors",
    "source_readiness_not_verified",
    "source_reverification_deadline",
    "source_unproved",
    "source_uri_unavailable",
    "staff",
    "stale_native_body_callback",
    "stale_sample_ticket",
    "starting",
    "system",
    "tab",
    "tmp",
    "too_many_requests",
    "true",
    "twitch_channel",
    "twitch_login",
    "twitch_popout_chat",
    "two_or_three_distinct_channels_required",
    "unauthenticated",
    "unauthorized",
    "unexpected_community_panel",
    "unexpected_popout_video",
    "unexpected_repeated_pause_action",
    "unknown",
    "unsupported_native_event",
    "unsupported_or_duplicate_argument",
    "verification",
    "verified_player_pause_lost",
    "viewers",
    "vips",
    "waiting_for_browser_exit",
    "waiting_ui",
    "webview2_environment_override",
    "worker_failed",
    "worker_protocol_failure",
    "worker_shutdown_timeout",
    "worker_stalled",
    "worker_start_failed",
    "wrong_page",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_oversized_malformed_and_partial_frames() {
        assert!(
            read_frame::<ParentFrame>(&mut &((FRAME_CAP as u32 + 1).to_le_bytes())[..]).is_err()
        );
        assert!(read_frame::<ParentFrame>(&mut &b"\x02\x00\x00\x00{}"[..]).is_err());
        assert!(read_frame::<ParentFrame>(&mut &b"\x10\x00\x00\x00{}"[..]).is_err());
        assert!(read_frame::<ParentFrame>(&mut &b"\x01\x00"[..]).is_err());
        assert!(read_frame::<ParentFrame>(&mut &b""[..]).unwrap().is_none());
    }
    #[test]
    fn oversized_writer_never_publishes_a_partial_frame() {
        let mut output = Vec::new();
        assert!(write_frame(&mut output, &"x".repeat(FRAME_CAP)).is_err());
        assert!(output.is_empty());
    }
    #[test]
    fn private_payload_preserves_receipt_time_and_provenance() {
        let sample = SourceSample {
            received_at_unix_ms: 123456,
            reported_chatters: Some(9),
            members: vec![Member {
                account: "private_account".into(),
                roles: 32,
            }],
            category_counts: [0, 0, 0, 0, 0, 1],
            http_status: 200,
            provenance: Provenance {
                request_after_trigger_verified: true,
                timing_order: "finish_at_or_after_response_event",
                transport: "network_response",
                server_freshness: "unknown",
                ..Default::default()
            },
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &WireOutcome::from_source(Ok(sample))).unwrap();
        let outcome = read_frame::<WireOutcome>(&mut bytes.as_slice())
            .unwrap()
            .unwrap();
        let Ok(received) = outcome.into_source().unwrap() else {
            panic!("sample lost")
        };
        assert_eq!(received.received_at_unix_ms, 123456);
        assert_eq!(received.members[0].account, "private_account");
        assert!(received.provenance.request_after_trigger_verified);
    }
}
