//! Protobuf/domain conversion and richer gRPC status construction.

use std::path::Path;
use std::time::Duration;

use prost::Message;
use prost_types::Any;
use qingluan_core::terminal as domain;
use qingluan_protocol::google::rpc::Status as RpcStatus;
use qingluan_protocol::terminal::v1 as pb;
use qingluan_terminal::{QuotaScope, RuntimeError, SendError, SendRejection};
use tonic::{Code, Status};

use crate::lease::LeaseError;

pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;
pub const MAX_STATUS_DETAILS_BYTES: usize = 8 * 1024;
pub const MAX_START_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_START_COLLECTION_BYTES: usize = 256 * 1024;
pub const MAX_START_ARGS: usize = 4096;
pub const MAX_ENV_ENTRIES: usize = 4096;
pub const MAX_CONTROL_TOKEN_BYTES: usize = 256;
pub const MAX_SEND_BYTES: usize = 256 * 1024;
const ERROR_DETAIL_TYPE_URL: &str = "type.googleapis.com/qingluan.terminal.v1.ErrorDetail";

pub fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn session(value: Option<pb::SessionRef>) -> Result<domain::SessionRef, Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("session is required"))?;
    validate_identity("session.source", &value.source)?;
    validate_identity("session.external_id", &value.external_id)?;
    Ok(domain::SessionRef {
        source: domain::SessionSource::new(value.source),
        external_id: domain::ExternalSessionId::new(value.external_id),
    })
}

pub fn terminal(value: Option<pb::TerminalRef>) -> Result<domain::TerminalRef, Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("terminal is required"))?;
    let session = session(value.session)?;
    validate_identity("terminal_id", &value.terminal_id)?;
    Ok(domain::TerminalRef {
        session,
        terminal_id: domain::TerminalId::new(value.terminal_id),
    })
}

pub fn terminal_for_session(
    session: domain::SessionRef,
    terminal_id: String,
) -> Result<domain::TerminalRef, Status> {
    validate_identity("terminal_id", &terminal_id)?;
    Ok(domain::TerminalRef {
        session,
        terminal_id: domain::TerminalId::new(terminal_id),
    })
}

pub fn control(value: Option<pb::ControlContext>) -> Result<(domain::SessionRef, String), Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("control context is required"))?;
    if value.control_token.is_empty() || value.control_token.len() > MAX_CONTROL_TOKEN_BYTES {
        return Err(control_expired());
    }
    Ok((session(value.session)?, value.control_token))
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > 1024 {
        return Err(Status::invalid_argument(format!("{field} is too long")));
    }
    Ok(())
}

pub fn size(value: Option<pb::TerminalSize>) -> Result<domain::TerminalSize, Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("terminal size is required"))?;
    let rows = u16::try_from(value.rows)
        .ok()
        .filter(|rows| *rows != 0)
        .ok_or_else(|| Status::invalid_argument("rows must be in 1..=65535"))?;
    let columns = u16::try_from(value.columns)
        .ok()
        .filter(|columns| *columns != 0)
        .ok_or_else(|| Status::invalid_argument("columns must be in 1..=65535"))?;
    Ok(domain::TerminalSize { rows, columns })
}

pub fn limits(value: Option<pb::QueryLimits>) -> Result<domain::ReadLimits, Status> {
    let Some(value) = value else {
        return Ok(domain::ReadLimits::default());
    };
    let max_lines = value
        .max_lines
        .unwrap_or(domain::ReadLimits::DEFAULT_MAX_LINES);
    let max_bytes = value
        .max_bytes
        .unwrap_or(domain::ReadLimits::DEFAULT_MAX_BYTES);
    domain::ReadLimits::new(max_lines, max_bytes)
        .map(domain::ReadLimits::clamped)
        .map_err(|error| Status::invalid_argument(error.to_string()))
}

pub fn start_spec(request: &mut pb::StartRequest) -> Result<domain::StartSpec, Status> {
    if request.program.is_empty()
        || request.program.len() > MAX_START_TEXT_BYTES
        || request.program.contains('\0')
    {
        return Err(Status::invalid_argument(
            "program must be non-empty and at most 4096 bytes",
        ));
    }
    if request.cwd.is_empty()
        || request.cwd.len() > MAX_START_TEXT_BYTES
        || request.cwd.contains('\0')
        || !Path::new(&request.cwd).is_absolute()
    {
        return Err(Status::invalid_argument(
            "cwd must be a non-empty absolute path of at most 4096 bytes",
        ));
    }
    if request.args.len() > MAX_START_ARGS {
        return Err(Status::invalid_argument("too many terminal arguments"));
    }
    let mut collection_bytes = 0usize;
    for arg in &request.args {
        if arg.len() > MAX_START_TEXT_BYTES || arg.contains('\0') {
            return Err(Status::invalid_argument("one argument exceeds 4096 bytes"));
        }
        collection_bytes = collection_bytes
            .checked_add(arg.len())
            .ok_or_else(|| Status::invalid_argument("argument bytes overflow"))?;
    }
    let env = request
        .env
        .take()
        .ok_or_else(|| Status::invalid_argument("environment snapshot is required"))?;
    if env.entries.len() > MAX_ENV_ENTRIES {
        return Err(Status::invalid_argument(
            "environment snapshot has too many entries",
        ));
    }
    let mut entries = Vec::with_capacity(env.entries.len());
    for entry in env.entries {
        if entry.name.is_empty()
            || entry.name.contains('=')
            || entry.name.contains('\0')
            || entry.value.contains('\0')
        {
            return Err(Status::invalid_argument("invalid environment entry"));
        }
        collection_bytes = collection_bytes
            .checked_add(entry.name.len())
            .and_then(|total| total.checked_add(entry.value.len()))
            .ok_or_else(|| Status::invalid_argument("environment bytes overflow"))?;
        entries.push((entry.name, entry.value));
    }
    if collection_bytes > MAX_START_COLLECTION_BYTES {
        return Err(Status::invalid_argument(
            "arguments and environment exceed 256 KiB",
        ));
    }
    Ok(domain::StartSpec::new(
        std::mem::take(&mut request.program),
        std::mem::take(&mut request.args),
        std::mem::take(&mut request.cwd),
        domain::EnvironmentSnapshot::new(entries),
        size(request.size.take())?,
    ))
}

pub fn read_cursor(value: pb::ReadCursor) -> Result<domain::ReadCursor, Status> {
    let log = log_identity(value.log)?;
    let next = history_position(value.next)?;
    domain::ReadCursor::new(log, next, value.end_line)
        .map_err(|error| Status::invalid_argument(error.to_string()))
}

pub fn history_position(
    value: Option<pb::HistoryPosition>,
) -> Result<domain::HistoryPosition, Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("history position is required"))?;
    domain::HistoryPosition::new(value.line, value.byte_offset)
        .map_err(|error| Status::invalid_argument(error.to_string()))
}

pub fn log_identity(value: Option<pb::LogIdentity>) -> Result<domain::LogIdentity, Status> {
    let value = value.ok_or_else(|| Status::invalid_argument("log identity is required"))?;
    validate_identity("log_epoch", &value.log_epoch)?;
    Ok(domain::LogIdentity {
        terminal: terminal(value.terminal)?,
        log_epoch: domain::LogEpoch::new(value.log_epoch),
    })
}

pub fn event_state(value: domain::SessionEventState) -> pb::SessionEventState {
    pb::SessionEventState {
        acked_through_seq: value.acked_through_seq(),
        last_committed_seq: value.last_committed_seq(),
        pruned_through_seq: value.pruned_through_seq(),
    }
}

/// Convert one domain lifecycle event into its wire shape.
///
/// Fail-closed: a payload this build cannot represent (a future
/// `#[non_exhaustive]` variant) is an error, never an event with a missing
/// payload — an unrepresentable event must not be published to a stream a
/// client could then acknowledge.
pub fn session_event(event: &domain::SessionEvent) -> Result<pb::SessionEvent, Status> {
    let payload = match event.payload {
        domain::SessionEventPayload::ProcessExited(result) => {
            let result = match result {
                domain::ExitResult::ExitCode(code) => pb::exit_result::Result::ExitCode(code),
                domain::ExitResult::Signal(signal) => pb::exit_result::Result::Signal(signal),
                _ => {
                    return Err(Status::internal(
                        "unsupported process exit result in session event",
                    ));
                }
            };
            pb::session_event::Payload::ProcessExited(pb::ExitResult {
                result: Some(result),
            })
        }
        domain::SessionEventPayload::OutputClosed(end) => {
            let end = match end {
                domain::OutputEnd::Eof => pb::output_end::End::Eof(pb::OutputEof {}),
                domain::OutputEnd::ForcedClose => {
                    pb::output_end::End::ForcedClose(pb::OutputForcedClose {})
                }
                domain::OutputEnd::ReadError => {
                    pb::output_end::End::ReadError(pb::OutputReadError {})
                }
                domain::OutputEnd::Interrupted => {
                    pb::output_end::End::Interrupted(pb::OutputInterrupted {})
                }
                _ => return Err(Status::internal("unsupported output end in session event")),
            };
            pb::session_event::Payload::OutputClosed(pb::OutputEnd { end: Some(end) })
        }
        _ => {
            return Err(Status::internal(
                "unsupported session event payload for this protocol version",
            ));
        }
    };
    Ok(pb::SessionEvent {
        session: Some(pb::SessionRef {
            source: event.terminal.session.source.as_str().to_owned(),
            external_id: event.terminal.session.external_id.as_str().to_owned(),
        }),
        event_seq: event.event_seq.get(),
        terminal_id: event.terminal.terminal_id.as_str().to_owned(),
        payload: Some(payload),
    })
}

/// Convert one domain event page into a watch batch. The batch is empty
/// exactly when the page carried no events (the initial watermark shape);
/// page ordering is preserved as-is.
pub fn watch_response(page: &domain::EventPage) -> Result<pb::WatchSessionEventsResponse, Status> {
    Ok(pb::WatchSessionEventsResponse {
        state: Some(event_state(page.state)),
        events: page
            .events
            .iter()
            .map(session_event)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn terminal_ref(value: &domain::TerminalRef) -> pb::TerminalRef {
    pb::TerminalRef {
        session: Some(pb::SessionRef {
            source: value.session.source.as_str().to_owned(),
            external_id: value.session.external_id.as_str().to_owned(),
        }),
        terminal_id: value.terminal_id.as_str().to_owned(),
    }
}

pub fn log(value: &domain::LogIdentity) -> pb::LogIdentity {
    pb::LogIdentity {
        terminal: Some(terminal_ref(&value.terminal)),
        log_epoch: value.log_epoch.as_str().to_owned(),
    }
}

pub fn position(value: domain::HistoryPosition) -> pb::HistoryPosition {
    pb::HistoryPosition {
        line: value.line(),
        byte_offset: value.byte_offset(),
    }
}

pub fn range(value: domain::HistoryRange) -> pb::HistoryRange {
    pb::HistoryRange {
        earliest: Some(position(value.earliest())),
        latest: Some(position(value.latest())),
    }
}

pub fn cursor(value: &domain::ReadCursor) -> pb::ReadCursor {
    pb::ReadCursor {
        log: Some(log(value.log())),
        next: Some(position(value.next())),
        end_line: value.end_line(),
    }
}

pub fn snapshot(value: &domain::TerminalSnapshot) -> Result<pb::TerminalSnapshot, Status> {
    let process = match value.process {
        domain::ProcessState::Running => pb::process_state::State::Running(pb::ProcessRunning {}),
        domain::ProcessState::Interrupted => {
            pb::process_state::State::Interrupted(pb::ProcessInterrupted {})
        }
        domain::ProcessState::Exited(result) => {
            let result = match result {
                domain::ExitResult::ExitCode(code) => pb::exit_result::Result::ExitCode(code),
                domain::ExitResult::Signal(signal) => pb::exit_result::Result::Signal(signal),
                _ => return Err(Status::internal("unsupported process exit result")),
            };
            pb::process_state::State::Exited(pb::ExitResult {
                result: Some(result),
            })
        }
        _ => return Err(Status::internal("unsupported process state")),
    };
    let output = match value.output {
        domain::OutputState::Open => pb::output_state::State::Open(pb::OutputOpen {}),
        domain::OutputState::Closed(end) => {
            let end = match end {
                domain::OutputEnd::Eof => pb::output_end::End::Eof(pb::OutputEof {}),
                domain::OutputEnd::ForcedClose => {
                    pb::output_end::End::ForcedClose(pb::OutputForcedClose {})
                }
                domain::OutputEnd::ReadError => {
                    pb::output_end::End::ReadError(pb::OutputReadError {})
                }
                domain::OutputEnd::Interrupted => {
                    pb::output_end::End::Interrupted(pb::OutputInterrupted {})
                }
                _ => return Err(Status::internal("unsupported output end")),
            };
            pb::output_state::State::Closed(pb::OutputEnd { end: Some(end) })
        }
        _ => return Err(Status::internal("unsupported output state")),
    };
    Ok(pb::TerminalSnapshot {
        terminal: Some(terminal_ref(&value.terminal)),
        process: Some(pb::ProcessState {
            state: Some(process),
        }),
        output: Some(pb::OutputState {
            state: Some(output),
        }),
        stopping: value.stopping,
        size: Some(pb::TerminalSize {
            rows: u32::from(value.size.rows),
            columns: u32::from(value.size.columns),
        }),
        retained_history: value.retained_history.map(range),
    })
}

pub fn read_response(value: &domain::ReadResult) -> Result<pb::ReadResponse, Status> {
    let page = value.page();
    Ok(pb::ReadResponse {
        cursor: Some(cursor(value.cursor())),
        fragments: page
            .fragments()
            .iter()
            .map(|fragment| pb::LineFragment {
                position: Some(position(fragment.position())),
                text: fragment.text().to_owned(),
                prefix_omitted: fragment.prefix_omitted(),
                suffix_remaining: fragment.suffix_remaining(),
            })
            .collect(),
        next: page.next().map(cursor),
        truncation: match page.truncation() {
            None => pb::ReadTruncation::Unspecified.into(),
            Some(domain::ReadTruncation::ByteBudget) => pb::ReadTruncation::ByteBudget.into(),
            Some(domain::ReadTruncation::LineBudget) => pb::ReadTruncation::LineBudget.into(),
            Some(domain::ReadTruncation::Gap) => pb::ReadTruncation::Gap.into(),
            Some(_) => return Err(Status::internal("unsupported read truncation")),
        },
        retained: page.retained().map(range),
        degraded: page.degraded(),
    })
}

pub fn tail_response(value: &domain::TailView) -> Result<pb::TailResponse, Status> {
    let tail = value.tail();
    Ok(pb::TailResponse {
        history: Some(read_response(value.history())?),
        tail: Some(pb::TailSnapshot {
            log: Some(log(tail.log())),
            position: Some(pb::TailPosition {
                tail_id: tail.position().tail_id().as_str().to_owned(),
                revision: tail.position().revision(),
                byte_offset: tail.position().byte_offset(),
            }),
            text: tail.text().to_owned(),
            truncated: tail.truncated(),
        }),
    })
}

pub fn lease_status(error: LeaseError) -> Status {
    match error {
        LeaseError::Busy { remaining } => rich_status(
            Code::FailedPrecondition,
            "session control is already held",
            pb::ErrorDetail {
                reason: pb::ErrorReason::ControlBusy.into(),
                payload: Some(pb::error_detail::Payload::ControlBusy(
                    pb::ControlBusyDetails {
                        remaining_ms: Some(duration_ms(remaining)),
                    },
                )),
            },
        ),
        LeaseError::Expired => control_expired(),
        LeaseError::Runtime(error) => runtime_status(error),
    }
}

pub fn control_expired() -> Status {
    rich_status(
        Code::FailedPrecondition,
        "control token is expired or invalid",
        pb::ErrorDetail {
            reason: pb::ErrorReason::ControlExpired.into(),
            payload: None,
        },
    )
}

pub fn runtime_status(error: RuntimeError) -> Status {
    match error {
        RuntimeError::UnknownTerminal(_) => Status::not_found("terminal not found"),
        RuntimeError::ControlLost(_) => control_expired(),
        RuntimeError::ControlGenerationExhausted => {
            Status::unavailable("control generation exhausted")
        }
        RuntimeError::NotWritable(terminal) => terminal_not_writable(&terminal),
        RuntimeError::QuotaExhausted { scope } => rich_status(
            Code::ResourceExhausted,
            "terminal quota exhausted",
            pb::ErrorDetail {
                reason: pb::ErrorReason::ResourceExhausted.into(),
                payload: Some(pb::error_detail::Payload::ResourceExhausted(
                    pb::ResourceExhaustedDetails {
                        kind: match scope {
                            QuotaScope::Session => pb::ResourceKind::SessionTerminals.into(),
                            QuotaScope::Global => pb::ResourceKind::GlobalTerminals.into(),
                        },
                    },
                )),
            },
        ),
        RuntimeError::StartRejected { .. } => {
            Status::invalid_argument("terminal start was rejected")
        }
        RuntimeError::CleanupIncomplete { .. }
        | RuntimeError::ShutdownIncomplete { .. }
        | RuntimeError::Io { .. } => Status::internal("terminal operation failed"),
        RuntimeError::Storage { .. } | RuntimeError::Cgroup { .. } | RuntimeError::Shutdown => {
            Status::unavailable("terminal service unavailable")
        }
        RuntimeError::CursorExpired { earliest, missing } => {
            cursor_expired(pb::QueryKind::Read, earliest, missing)
        }
        RuntimeError::QueryGap { range: missing } => {
            cursor_expired(pb::QueryKind::Read, None, Some(missing))
        }
        RuntimeError::InvalidQuery { .. } => Status::invalid_argument("invalid terminal query"),
        RuntimeError::EventRangeCleared {
            after_event_seq,
            pruned_through_seq,
            available_after_seq,
        } => rich_status(
            Code::FailedPrecondition,
            "requested event range was already cleared",
            pb::ErrorDetail {
                reason: pb::ErrorReason::EventRangeCleared.into(),
                payload: Some(pb::error_detail::Payload::EventRangeCleared(
                    pb::EventRangeClearedDetails {
                        after_event_seq,
                        pruned_through_seq,
                        available_after_seq,
                    },
                )),
            },
        ),
        RuntimeError::EventAckOutOfBounds { .. } => {
            Status::invalid_argument("event ack exceeds the committed event bound")
        }
        _ => Status::internal("terminal operation failed"),
    }
}

pub fn tail_status(error: RuntimeError) -> Status {
    match error {
        RuntimeError::CursorExpired { earliest, missing } => {
            cursor_expired(pb::QueryKind::Tail, earliest, missing)
        }
        RuntimeError::QueryGap { range: missing } => {
            cursor_expired(pb::QueryKind::Tail, None, Some(missing))
        }
        other => runtime_status(other),
    }
}

pub fn send_status(error: SendError, terminal: &domain::TerminalRef) -> Status {
    match error {
        SendError::Rejected(SendRejection::Oversize) => {
            Status::invalid_argument("send payload exceeds 256 KiB")
        }
        SendError::Rejected(SendRejection::QueueFull) => rich_status(
            Code::ResourceExhausted,
            "terminal input queue is full",
            pb::ErrorDetail {
                reason: pb::ErrorReason::ResourceExhausted.into(),
                payload: Some(pb::error_detail::Payload::ResourceExhausted(
                    pb::ResourceExhaustedDetails {
                        kind: pb::ResourceKind::InputQueue.into(),
                    },
                )),
            },
        ),
        SendError::Rejected(SendRejection::Stopped) => terminal_not_writable(terminal),
        SendError::Rejected(SendRejection::ControlLost) => control_expired(),
        SendError::Rejected(SendRejection::Unknown) => Status::not_found("terminal not found"),
        SendError::Partial(partial) => {
            let (code, abort) = match partial.reason {
                domain::WriteAbort::StopIntent => {
                    (Code::Cancelled, pb::WriteAbortReason::StopIntent)
                }
                domain::WriteAbort::ControlLost => {
                    (Code::FailedPrecondition, pb::WriteAbortReason::ControlLost)
                }
                domain::WriteAbort::WriteDeadline => {
                    (Code::DeadlineExceeded, pb::WriteAbortReason::WriteDeadline)
                }
                domain::WriteAbort::ServiceShutdown => {
                    (Code::Unavailable, pb::WriteAbortReason::ServiceShutdown)
                }
                domain::WriteAbort::WriteFailed => {
                    (Code::Internal, pb::WriteAbortReason::WriteFailed)
                }
                _ => (Code::Internal, pb::WriteAbortReason::Unspecified),
            };
            rich_status(
                code,
                "terminal input was only partially written",
                pb::ErrorDetail {
                    reason: pb::ErrorReason::PartialWrite.into(),
                    payload: Some(pb::error_detail::Payload::PartialWrite(
                        pb::PartialWriteDetails {
                            written_bytes: Some(partial.written_bytes),
                            abort: Some(abort.into()),
                        },
                    )),
                },
            )
        }
        _ => Status::internal("terminal send failed"),
    }
}

fn terminal_not_writable(terminal: &domain::TerminalRef) -> Status {
    rich_status(
        Code::FailedPrecondition,
        "terminal is not writable",
        pb::ErrorDetail {
            reason: pb::ErrorReason::TerminalNotWritable.into(),
            payload: Some(pb::error_detail::Payload::TerminalNotWritable(
                pb::TerminalNotWritableDetails {
                    terminal: Some(terminal_ref(terminal)),
                    snapshot: None,
                },
            )),
        },
    )
}

fn cursor_expired(
    kind: pb::QueryKind,
    earliest: Option<domain::HistoryPosition>,
    missing: Option<domain::HistoryRange>,
) -> Status {
    rich_status(
        Code::FailedPrecondition,
        "history cursor is no longer readable",
        pb::ErrorDetail {
            reason: pb::ErrorReason::CursorExpired.into(),
            payload: Some(pb::error_detail::Payload::CursorExpired(
                pb::CursorExpiredDetails {
                    kind: kind.into(),
                    earliest: earliest.map(position),
                    missing: missing.map(range),
                },
            )),
        },
    )
}

fn rich_status(code: Code, message: &'static str, detail: pb::ErrorDetail) -> Status {
    let any = Any {
        type_url: ERROR_DETAIL_TYPE_URL.to_owned(),
        value: detail.encode_to_vec(),
    };
    let carrier = RpcStatus {
        code: code as i32,
        message: message.to_owned(),
        details: vec![any],
    };
    let encoded = carrier.encode_to_vec();
    if encoded.len() > MAX_STATUS_DETAILS_BYTES {
        return Status::new(code, message);
    }
    Status::with_details(code, message, encoded.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingluan_core::terminal::{PartialWrite, WriteAbort};

    #[test]
    fn explicit_zero_limits_are_not_defaults() {
        assert!(limits(None).is_ok());
        let error = limits(Some(pb::QueryLimits {
            max_lines: Some(0),
            max_bytes: None,
        }))
        .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
    }

    #[test]
    fn rich_partial_write_keeps_known_zero_and_matching_codes() {
        let terminal = domain::TerminalRef {
            session: domain::SessionRef {
                source: domain::SessionSource::new("test"),
                external_id: domain::ExternalSessionId::new("session"),
            },
            terminal_id: domain::TerminalId::new("terminal"),
        };
        let status = send_status(
            SendError::Partial(PartialWrite::new(0, WriteAbort::StopIntent)),
            &terminal,
        );
        assert_eq!(status.code(), Code::Cancelled);
        let carrier = RpcStatus::decode(status.details()).unwrap();
        assert_eq!(carrier.code, Code::Cancelled as i32);
        assert_eq!(carrier.details.len(), 1);
        let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
        match detail.payload.unwrap() {
            pb::error_detail::Payload::PartialWrite(detail) => {
                assert_eq!(detail.written_bytes, Some(0));
                assert_eq!(detail.abort, Some(pb::WriteAbortReason::StopIntent.into()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn queue_exhaustion_is_typed() {
        let terminal = domain::TerminalRef {
            session: domain::SessionRef {
                source: domain::SessionSource::new("test"),
                external_id: domain::ExternalSessionId::new("session"),
            },
            terminal_id: domain::TerminalId::new("terminal"),
        };
        let status = send_status(SendError::Rejected(SendRejection::QueueFull), &terminal);
        assert_eq!(status.code(), Code::ResourceExhausted);
        let carrier = RpcStatus::decode(status.details()).unwrap();
        let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
        match detail.payload.unwrap() {
            pb::error_detail::Payload::ResourceExhausted(detail) => {
                assert_eq!(detail.kind, pb::ResourceKind::InputQueue as i32);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn tail_cursor_errors_keep_the_query_kind() {
        let earliest = domain::HistoryPosition::new(2, 0).unwrap();
        let status = tail_status(RuntimeError::CursorExpired {
            earliest: Some(earliest),
            missing: None,
        });
        let carrier = RpcStatus::decode(status.details()).unwrap();
        let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
        match detail.payload.unwrap() {
            pb::error_detail::Payload::CursorExpired(detail) => {
                assert_eq!(detail.kind, pb::QueryKind::Tail as i32);
                assert_eq!(detail.earliest.unwrap().line, 2);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn environment_presence_and_order_are_preserved() {
        let mut missing = pb::StartRequest {
            control: None,
            program: "/bin/true".into(),
            args: Vec::new(),
            cwd: "/".into(),
            env: None,
            size: Some(pb::TerminalSize {
                rows: 30,
                columns: 120,
            }),
        };
        assert_eq!(
            start_spec(&mut missing).unwrap_err().code(),
            Code::InvalidArgument
        );

        let mut explicit = pb::StartRequest {
            env: Some(pb::EnvironmentSnapshot {
                entries: vec![
                    pb::EnvironmentVariable {
                        name: "A".into(),
                        value: "1".into(),
                    },
                    pb::EnvironmentVariable {
                        name: "A".into(),
                        value: "2".into(),
                    },
                ],
            }),
            ..missing
        };
        let spec = start_spec(&mut explicit).unwrap();
        assert_eq!(
            spec.env.iter().collect::<Vec<_>>(),
            vec![("A", "1"), ("A", "2")]
        );
    }
}
