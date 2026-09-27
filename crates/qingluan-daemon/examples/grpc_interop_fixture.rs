//! Test-only endpoint for `just terminal-grpc-interop` (S6) and
//! `just terminal-client-interop` (S7/S8).
//!
//! It serves the production `TerminalGrpcService` and socket lifecycle over a
//! small fake runtime. The fake supplies deterministic bigint/read/delay
//! cases without requiring cgroup delegation; it is not installed.
//!
//! Beyond the S6 surface (empty read pages, a 2 s `delay` send, bigint
//! identities on terminal `fixture`) the backend also serves the
//! client-facing scenarios: paged reads and tails with a stale-epoch
//! `CursorExpired` path on terminals other than `fixture`, `start`/`stop`
//! snapshots, scripted partial writes (`partial:<written>:<abort>`),
//! configurable delays (`delay:<ms>`), a lease TTL configurable through
//! `FIXTURE_LEASE_TTL_MS` (default 30 s), and the S8 event scenarios: a
//! deterministic per-session event log (seeded by `external_id`, with a
//! cleared-prefix session and a beyond-2^53 sequence session) plus dynamic
//! appends through `event:exit:<code>` / `event:signal:<n>` /
//! `event:closed:<end>` sends from a controlling client.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use qingluan_core::terminal as domain;
use qingluan_daemon::backend::TerminalBackend;
use qingluan_daemon::lease::{LeaseManager, MAX_LEASE_TTL};
use qingluan_daemon::service::TerminalGrpcService;
use qingluan_daemon::socket::BoundUnixSocket;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalServiceServer;
use qingluan_terminal::{RuntimeError, SendError, SendRejection};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

/// Log epoch served for terminals other than the S6 `fixture` terminal.
const CLIENT_EPOCH: &str = "client-fixture";
/// Fixed upper line bound of the paged fixture history (lines 1..=3;
/// history line numbers are 1-based).
const PAGED_END_LINE: u64 = 4;
/// A sequence strictly beyond 2^53, for bigint exactness over the wire.
const BIG_EVENT_SEQ: u64 = 9_007_199_254_740_993;

/// In-memory session event log mirroring the durable semantics the service
/// relies on: ordered committed events, a cumulative ack watermark, and an
/// optional pruned prefix that refuses earlier replays. `committed_floor`
/// keeps the legacy S6 acquire sentinel (committed = u64::MAX) for sessions
/// without a script.
#[derive(Default)]
struct EventLog {
    events: Vec<domain::SessionEvent>,
    acked: u64,
    pruned: u64,
    committed_floor: u64,
}

impl EventLog {
    fn committed(&self) -> u64 {
        self.events
            .last()
            .map(|event| event.event_seq.get())
            .unwrap_or(0)
            .max(self.committed_floor)
    }

    fn state(&self) -> domain::SessionEventState {
        domain::SessionEventState::new(self.pruned, self.acked, self.committed()).unwrap()
    }

    fn append(&mut self, session: &domain::SessionRef, payload: domain::SessionEventPayload) {
        let seq = self.committed() + 1;
        self.events.push(domain::SessionEvent {
            terminal: domain::TerminalRef {
                session: session.clone(),
                terminal_id: domain::TerminalId::new("client"),
            },
            event_seq: domain::EventSequence::new(seq).expect("sequence is non-zero"),
            payload,
        });
    }

    fn append_at(
        &mut self,
        session: &domain::SessionRef,
        seq: u64,
        terminal_id: &str,
        payload: domain::SessionEventPayload,
    ) {
        self.events.push(domain::SessionEvent {
            terminal: domain::TerminalRef {
                session: session.clone(),
                terminal_id: domain::TerminalId::new(terminal_id),
            },
            event_seq: domain::EventSequence::new(seq).expect("sequence is non-zero"),
            payload,
        });
    }
}

/// Deterministically seed one session's event log by `external_id`. Unknown
/// ids keep the legacy S6 bigint sentinel instead of a script.
fn seed_event_log(session: &domain::SessionRef) -> EventLog {
    let mut log = EventLog::default();
    match session.external_id.as_str() {
        // The S8 client scenarios: three replayable events.
        "events" => {
            log.append_at(session, 1, "client", payload_exit(0));
            log.append_at(
                session,
                2,
                "fixture",
                domain::SessionEventPayload::OutputClosed(domain::OutputEnd::Eof),
            );
            log.append_at(
                session,
                3,
                "client",
                domain::SessionEventPayload::ProcessExited(domain::ExitResult::Signal(9)),
            );
        }
        // A pruned prefix: replays before 5 are cleared, 6..=8 remain.
        "events-cleared" => {
            log.acked = 5;
            log.pruned = 5;
            log.append_at(session, 6, "client", payload_exit(1));
            log.append_at(session, 7, "client", payload_exit(2));
            log.append_at(session, 8, "client", payload_exit(3));
        }
        // A single event beyond 2^53: bigint exactness end to end.
        "events-big" => {
            log.append_at(session, BIG_EVENT_SEQ, "client", payload_exit(0));
        }
        _ => {
            log.committed_floor = u64::MAX;
        }
    }
    log
}

fn payload_exit(code: i32) -> domain::SessionEventPayload {
    domain::SessionEventPayload::ProcessExited(domain::ExitResult::ExitCode(code))
}

struct FixtureBackend {
    generation: AtomicU64,
    started_size: Mutex<Option<domain::TerminalSize>>,
    events: Mutex<HashMap<domain::SessionRef, EventLog>>,
}

#[async_trait]
impl TerminalBackend for FixtureBackend {
    fn advance_control_generation(
        &self,
        _session: &domain::SessionRef,
    ) -> Result<domain::ControlGeneration, RuntimeError> {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        domain::ControlGeneration::new(next).ok_or(RuntimeError::ControlGenerationExhausted)
    }

    async fn ensure_session(
        &self,
        session: &domain::SessionRef,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        let mut events = self.events.lock().expect("event log");
        let log = events
            .entry(session.clone())
            .or_insert_with(|| seed_event_log(session));
        Ok(log.state())
    }

    async fn events_after(
        &self,
        session: &domain::SessionRef,
        after_event_seq: u64,
    ) -> Result<domain::EventPage, RuntimeError> {
        // Seed on access as well as on acquire: a watch is lease-free and
        // may legitimately be the first touch of a scripted session.
        let mut events = self.events.lock().expect("event log");
        let log = events
            .entry(session.clone())
            .or_insert_with(|| seed_event_log(session));
        if after_event_seq < log.pruned {
            return Err(RuntimeError::EventRangeCleared {
                after_event_seq,
                pruned_through_seq: log.pruned,
                available_after_seq: log.pruned,
            });
        }
        Ok(domain::EventPage {
            state: log.state(),
            events: log
                .events
                .iter()
                .filter(|event| event.event_seq.get() > after_event_seq)
                .cloned()
                .collect(),
            next_after_seq: None,
        })
    }

    async fn ack_events(
        &self,
        session: &domain::SessionRef,
        up_to_seq: u64,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        let mut events = self.events.lock().expect("event log");
        let log = events
            .entry(session.clone())
            .or_insert_with(|| seed_event_log(session));
        let committed = log.committed();
        if up_to_seq > committed {
            return Err(RuntimeError::EventAckOutOfBounds {
                up_to_seq,
                last_committed_seq: committed,
            });
        }
        log.acked = log.acked.max(up_to_seq);
        Ok(log.state())
    }

    async fn start(
        &self,
        session: &domain::SessionRef,
        _generation: domain::ControlGeneration,
        spec: domain::StartSpec,
    ) -> Result<domain::TerminalRef, RuntimeError> {
        *self.started_size.lock().expect("size lock") = Some(spec.size);
        Ok(domain::TerminalRef {
            session: session.clone(),
            terminal_id: domain::TerminalId::new("client"),
        })
    }

    async fn send(
        &self,
        terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
        data: Vec<u8>,
    ) -> Result<domain::SendReceipt, SendError> {
        if data == b"delay" {
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(domain::SendReceipt::new(data.len() as u64));
        }
        if let Some(rest) = data.strip_prefix(b"delay:") {
            let millis = std::str::from_utf8(rest)
                .ok()
                .and_then(|text| text.parse::<u64>().ok())
                .unwrap_or(2_000);
            tokio::time::sleep(Duration::from_millis(millis)).await;
            return Ok(domain::SendReceipt::new(data.len() as u64));
        }
        if let Some(rest) = data.strip_prefix(b"partial:") {
            let script = String::from_utf8_lossy(rest).into_owned();
            let (written, abort) = script.split_once(':').unwrap_or((script.as_str(), ""));
            let written = written.parse::<u64>().unwrap_or(0);
            let abort = match abort {
                "stop_intent" => domain::WriteAbort::StopIntent,
                "control_lost" => domain::WriteAbort::ControlLost,
                "write_deadline" => domain::WriteAbort::WriteDeadline,
                "service_shutdown" => domain::WriteAbort::ServiceShutdown,
                "write_failed" => domain::WriteAbort::WriteFailed,
                _ => domain::WriteAbort::WriteFailed,
            };
            return Err(SendError::Partial(domain::PartialWrite::new(
                written, abort,
            )));
        }
        // S8 dynamic event appends: only a controlling client can send, so
        // these create new committed events in the session's log. A legacy
        // sentinel session (committed floor u64::MAX) has no script to
        // append to and refuses instead of overflowing the sequence.
        if let Some(rest) = data.strip_prefix(b"event:") {
            let script = String::from_utf8_lossy(rest).into_owned();
            let payload = match script.as_str() {
                s if s.starts_with("exit:") => {
                    let code = s["exit:".len()..].parse::<i32>().unwrap_or(0);
                    payload_exit(code)
                }
                s if s.starts_with("signal:") => {
                    let signal = s["signal:".len()..].parse::<i32>().unwrap_or(9);
                    domain::SessionEventPayload::ProcessExited(domain::ExitResult::Signal(signal))
                }
                "closed:eof" => domain::SessionEventPayload::OutputClosed(domain::OutputEnd::Eof),
                "closed:forced" => {
                    domain::SessionEventPayload::OutputClosed(domain::OutputEnd::ForcedClose)
                }
                _ => payload_exit(0),
            };
            let mut events = self.events.lock().expect("event log");
            let log = events
                .entry(terminal.session.clone())
                .or_insert_with(|| seed_event_log(&terminal.session));
            if log.committed_floor == u64::MAX {
                return Err(SendError::Rejected(SendRejection::Unknown));
            }
            log.append(&terminal.session, payload);
            return Ok(domain::SendReceipt::new(data.len() as u64));
        }
        Ok(domain::SendReceipt::new(data.len() as u64))
    }

    async fn stop_with_generation(
        &self,
        terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
    ) -> Result<domain::TerminalSnapshot, RuntimeError> {
        let size = self
            .started_size
            .lock()
            .expect("size lock")
            .unwrap_or(domain::TerminalSize {
                rows: 24,
                columns: 80,
            });
        Ok(domain::TerminalSnapshot {
            terminal: terminal.clone(),
            process: domain::ProcessState::Exited(domain::ExitResult::ExitCode(0)),
            output: domain::OutputState::Closed(domain::OutputEnd::Eof),
            stopping: false,
            size,
            retained_history: None,
        })
    }

    async fn log_identity(
        &self,
        terminal: &domain::TerminalRef,
    ) -> Result<domain::LogIdentity, RuntimeError> {
        let epoch = if terminal.terminal_id.as_str() == "fixture" {
            "fixture"
        } else {
            CLIENT_EPOCH
        };
        Ok(domain::LogIdentity {
            terminal: terminal.clone(),
            log_epoch: domain::LogEpoch::new(epoch),
        })
    }

    async fn read(
        &self,
        request: &domain::ReadRequest,
    ) -> Result<domain::ReadResult, RuntimeError> {
        if request.log().terminal.terminal_id.as_str() == "fixture" {
            // S6 behavior: an empty complete page with the maximal bound.
            let position = domain::HistoryPosition::new(u64::MAX, 0).unwrap();
            let cursor =
                domain::ReadCursor::new(request.log().clone(), position, u64::MAX).unwrap();
            let page = domain::ReadPage::new(Vec::new(), None, None, None, false).unwrap();
            return Ok(domain::ReadResult::new(cursor, page));
        }
        if request.log().log_epoch.as_str() != CLIENT_EPOCH {
            // A cursor minted against another epoch (or hand-built) is
            // refused with a readable earliest position, never re-anchored.
            return Err(RuntimeError::CursorExpired {
                earliest: Some(domain::HistoryPosition::new(1, 0).unwrap()),
                missing: None,
            });
        }
        let start = request
            .cursor()
            .map(|cursor| cursor.next())
            .unwrap_or_else(|| match request.start() {
                domain::ReadStart::At(position) => position,
                _ => domain::HistoryPosition::new(1, 0).unwrap(),
            });
        // Lines 1..=3 exist; PAGED_END_LINE is the exclusive end. Pages
        // serve at most two lines, so the first page continues with a
        // `next` cursor and the second completes the fixed range.
        let first = start.line().clamp(1, PAGED_END_LINE - 1);
        let last = first.saturating_add(2).min(PAGED_END_LINE);
        let fragments = (first..last)
            .map(|line| {
                domain::LineFragment::new(
                    domain::HistoryPosition::new(line, 0).unwrap(),
                    format!("line-{line}"),
                    false,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let cursor = domain::ReadCursor::new(
            request.log().clone(),
            domain::HistoryPosition::new(first, 0).unwrap(),
            PAGED_END_LINE - 1,
        )
        .unwrap();
        let page = if last >= PAGED_END_LINE {
            domain::ReadPage::new(fragments, None, None, None, false).unwrap()
        } else {
            let next = domain::ReadCursor::new(
                request.log().clone(),
                domain::HistoryPosition::new(last, 0).unwrap(),
                PAGED_END_LINE - 1,
            )
            .unwrap();
            domain::ReadPage::new(
                fragments,
                Some(next),
                Some(domain::ReadTruncation::LineBudget),
                None,
                false,
            )
            .unwrap()
        };
        Ok(domain::ReadResult::new(cursor, page))
    }

    async fn tail(
        &self,
        terminal: &domain::TerminalRef,
        _limits: domain::ReadLimits,
    ) -> Result<domain::TailView, RuntimeError> {
        let log = self.log_identity(terminal).await?;
        let (history, text) = if terminal.terminal_id.as_str() == "fixture" {
            let position = domain::HistoryPosition::new(u64::MAX, 0).unwrap();
            let cursor = domain::ReadCursor::new(log.clone(), position, u64::MAX).unwrap();
            let page = domain::ReadPage::new(Vec::new(), None, None, None, false).unwrap();
            (domain::ReadResult::new(cursor, page), "fixture-tail")
        } else {
            let position = domain::HistoryPosition::new(2, 0).unwrap();
            let cursor =
                domain::ReadCursor::new(log.clone(), position, PAGED_END_LINE - 1).unwrap();
            let fragments = (2..PAGED_END_LINE)
                .map(|line| {
                    domain::LineFragment::new(
                        domain::HistoryPosition::new(line, 0).unwrap(),
                        format!("line-{line}"),
                        false,
                        false,
                    )
                })
                .collect::<Vec<_>>();
            let page = domain::ReadPage::new(fragments, None, None, None, false).unwrap();
            (domain::ReadResult::new(cursor, page), "client-tail")
        };
        let tail = domain::TailSnapshot::new(
            log,
            domain::TailPosition::new(domain::TailId::new("tail-1"), 1, 0),
            text,
            false,
        );
        Ok(domain::TailView::new(history, tail))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os()
        .nth(1)
        .ok_or("usage: grpc_interop_fixture SOCKET")?;
    let bound = BoundUnixSocket::bind(std::path::Path::new(&socket))?;
    let (listener, _guard) = bound.into_parts();

    let ttl_ms = std::env::var("FIXTURE_LEASE_TTL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000);
    let ttl = Duration::from_millis(ttl_ms.min(MAX_LEASE_TTL.as_millis() as u64));

    let backend: Arc<dyn TerminalBackend> = Arc::new(FixtureBackend {
        generation: AtomicU64::new(1),
        started_size: Mutex::new(None),
        events: Mutex::new(HashMap::new()),
    });
    let leases = LeaseManager::new(backend.clone(), ttl)?;
    let service = TerminalGrpcService::new(backend, leases, "interop-fixture");

    println!("READY");
    Server::builder()
        .add_service(
            TerminalServiceServer::new(service)
                .max_decoding_message_size(1024 * 1024)
                .max_encoding_message_size(1024 * 1024),
        )
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
