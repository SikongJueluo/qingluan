use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hyper_util::rt::TokioIo;
use prost::Message;
use qingluan_core::terminal as domain;
use qingluan_daemon::backend::TerminalBackend;
use qingluan_daemon::lease::LeaseManager;
use qingluan_daemon::service::TerminalGrpcService;
use qingluan_daemon::socket::BoundUnixSocket;
use qingluan_protocol::google::rpc::Status as RpcStatus;
use qingluan_protocol::terminal::v1 as pb;
use qingluan_protocol::terminal::v1::terminal_service_client::TerminalServiceClient;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalService;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalServiceServer;
use qingluan_terminal::{RuntimeError, SendError, SendRejection};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Endpoint, Server};
use tonic::Request;
use tower::service_fn;

/// Scripted per-session event log: committed events, a cumulative ack
/// watermark, and an optional pruned prefix. Mirrors the durable semantics
/// the service contract relies on (ordered pages, monotonic bounded ack,
/// cleared-prefix refusal).
#[derive(Default)]
struct EventScript {
    events: Vec<domain::SessionEvent>,
    acked: u64,
    pruned: u64,
}

impl EventScript {
    fn committed(&self) -> u64 {
        self.events
            .last()
            .map(|event| event.event_seq.get())
            .unwrap_or(0)
    }

    fn state(&self) -> domain::SessionEventState {
        domain::SessionEventState::new(self.pruned, self.acked, self.committed()).unwrap()
    }

    fn append(&mut self, session: &domain::SessionRef, payload: domain::SessionEventPayload) {
        let seq = self.committed() + 1;
        self.events.push(domain::SessionEvent {
            terminal: domain::TerminalRef {
                session: session.clone(),
                terminal_id: domain::TerminalId::new("terminal-fixture"),
            },
            event_seq: domain::EventSequence::new(seq).expect("sequence is non-zero"),
            payload,
        });
    }
}

fn scripted_event(
    session: &domain::SessionRef,
    seq: u64,
    payload: domain::SessionEventPayload,
) -> domain::SessionEvent {
    domain::SessionEvent {
        terminal: domain::TerminalRef {
            session: session.clone(),
            terminal_id: domain::TerminalId::new("terminal-fixture"),
        },
        event_seq: domain::EventSequence::new(seq).expect("sequence is non-zero"),
        payload,
    }
}

struct FakeBackend {
    generation: AtomicU64,
    starts: Mutex<Vec<domain::StartSpec>>,
    events: Mutex<HashMap<domain::SessionRef, EventScript>>,
    /// Counts `events_after` reads: the deterministic probe for proving an
    /// idle watch stops polling after its stream is dropped.
    event_reads: AtomicU64,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(1),
            starts: Mutex::new(Vec::new()),
            events: Mutex::new(HashMap::new()),
            event_reads: AtomicU64::new(0),
        }
    }

    /// Seed one session's event script with the given events and optional
    /// pruned prefix.
    fn seed_events(
        &self,
        session: &domain::SessionRef,
        events: Vec<domain::SessionEvent>,
        pruned: u64,
    ) {
        let acked = pruned;
        self.events.lock().unwrap().insert(
            session.clone(),
            EventScript {
                events,
                acked,
                pruned,
            },
        );
    }

    /// Append a new committed event (a live commit arriving mid-watch).
    fn commit_event(&self, session: &domain::SessionRef, payload: domain::SessionEventPayload) {
        self.events
            .lock()
            .unwrap()
            .get_mut(session)
            .expect("seeded session")
            .append(session, payload);
    }

    fn terminal(session: &domain::SessionRef) -> domain::TerminalRef {
        domain::TerminalRef {
            session: session.clone(),
            terminal_id: domain::TerminalId::new("terminal-fixture"),
        }
    }

    fn log(terminal: &domain::TerminalRef) -> domain::LogIdentity {
        domain::LogIdentity {
            terminal: terminal.clone(),
            log_epoch: domain::LogEpoch::new("epoch-fixture"),
        }
    }

    fn empty_read(log: domain::LogIdentity) -> domain::ReadResult {
        let position = domain::HistoryPosition::new(1, 0).unwrap();
        let cursor = domain::ReadCursor::new(log, position, 1).unwrap();
        let page = domain::ReadPage::new(Vec::new(), None, None, None, false).unwrap();
        domain::ReadResult::new(cursor, page)
    }
}

#[async_trait]
impl TerminalBackend for FakeBackend {
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
        let events = self.events.lock().unwrap();
        Ok(events
            .get(session)
            .map(|script| script.state())
            .unwrap_or_else(|| domain::SessionEventState::new(0, 0, 0).unwrap()))
    }

    async fn events_after(
        &self,
        session: &domain::SessionRef,
        after_event_seq: u64,
    ) -> Result<domain::EventPage, RuntimeError> {
        self.event_reads.fetch_add(1, Ordering::SeqCst);
        let events = self.events.lock().unwrap();
        let Some(script) = events.get(session) else {
            return Ok(domain::EventPage {
                state: domain::SessionEventState::new(0, 0, 0).unwrap(),
                events: Vec::new(),
                next_after_seq: None,
            });
        };
        if after_event_seq < script.pruned {
            return Err(RuntimeError::EventRangeCleared {
                after_event_seq,
                pruned_through_seq: script.pruned,
                available_after_seq: script.pruned,
            });
        }
        Ok(domain::EventPage {
            state: script.state(),
            events: script
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
        let mut events = self.events.lock().unwrap();
        let Some(script) = events.get_mut(session) else {
            return Ok(domain::SessionEventState::new(0, 0, 0).unwrap());
        };
        let committed = script.committed();
        if up_to_seq > committed {
            return Err(RuntimeError::EventAckOutOfBounds {
                up_to_seq,
                last_committed_seq: committed,
            });
        }
        script.acked = script.acked.max(up_to_seq);
        Ok(script.state())
    }

    async fn start(
        &self,
        session: &domain::SessionRef,
        _generation: domain::ControlGeneration,
        spec: domain::StartSpec,
    ) -> Result<domain::TerminalRef, RuntimeError> {
        self.starts.lock().unwrap().push(spec);
        Ok(Self::terminal(session))
    }

    async fn send(
        &self,
        _terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
        data: Vec<u8>,
    ) -> Result<domain::SendReceipt, SendError> {
        if data.len() > 256 * 1024 {
            return Err(SendError::Rejected(SendRejection::Oversize));
        }
        Ok(domain::SendReceipt::new(data.len() as u64))
    }

    async fn stop_with_generation(
        &self,
        terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
    ) -> Result<domain::TerminalSnapshot, RuntimeError> {
        Ok(domain::TerminalSnapshot {
            terminal: terminal.clone(),
            process: domain::ProcessState::Running,
            output: domain::OutputState::Open,
            stopping: true,
            size: domain::TerminalSize {
                rows: 30,
                columns: 120,
            },
            retained_history: None,
        })
    }

    async fn log_identity(
        &self,
        terminal: &domain::TerminalRef,
    ) -> Result<domain::LogIdentity, RuntimeError> {
        if terminal.terminal_id.as_str() == "missing" {
            return Err(RuntimeError::UnknownTerminal(terminal.clone()));
        }
        Ok(Self::log(terminal))
    }

    async fn read(
        &self,
        request: &domain::ReadRequest,
    ) -> Result<domain::ReadResult, RuntimeError> {
        Ok(Self::empty_read(request.log().clone()))
    }

    async fn tail(
        &self,
        terminal: &domain::TerminalRef,
        _limits: domain::ReadLimits,
    ) -> Result<domain::TailView, RuntimeError> {
        let log = Self::log(terminal);
        let history = Self::empty_read(log.clone());
        let tail = domain::TailSnapshot::new(
            log,
            domain::TailPosition::new(domain::TailId::new("tail-fixture"), 7, 0),
            "pending",
            false,
        );
        Ok(domain::TailView::new(history, tail))
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "qingluan-s6-grpc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session() -> pb::SessionRef {
    pb::SessionRef {
        source: "pi".into(),
        external_id: "s1".into(),
    }
}

fn pruned_session() -> pb::SessionRef {
    pb::SessionRef {
        source: "pi".into(),
        external_id: "pruned".into(),
    }
}

fn domain_session(external_id: &str) -> domain::SessionRef {
    domain::SessionRef {
        source: domain::SessionSource::new("pi"),
        external_id: domain::ExternalSessionId::new(external_id),
    }
}

fn control(token: &str) -> pb::ControlContext {
    pb::ControlContext {
        session: Some(session()),
        control_token: token.into(),
    }
}

#[tokio::test]
async fn generated_client_crosses_the_private_uds_and_preserves_presence_and_rich_errors() {
    let root = TempDir::new();
    let socket = root.0.join("daemon.sock");
    let bound = BoundUnixSocket::bind(&socket).unwrap();
    let (listener, guard) = bound.into_parts();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_events(
        &domain_session("s1"),
        vec![
            scripted_event(
                &domain_session("s1"),
                1,
                domain::SessionEventPayload::ProcessExited(domain::ExitResult::ExitCode(0)),
            ),
            scripted_event(
                &domain_session("s1"),
                2,
                domain::SessionEventPayload::OutputClosed(domain::OutputEnd::Eof),
            ),
            scripted_event(
                &domain_session("s1"),
                3,
                domain::SessionEventPayload::ProcessExited(domain::ExitResult::Signal(9)),
            ),
        ],
        0,
    );
    backend.seed_events(
        &domain_session("pruned"),
        vec![
            scripted_event(
                &domain_session("pruned"),
                6,
                domain::SessionEventPayload::ProcessExited(domain::ExitResult::ExitCode(1)),
            ),
            scripted_event(
                &domain_session("pruned"),
                7,
                domain::SessionEventPayload::OutputClosed(domain::OutputEnd::ForcedClose),
            ),
            scripted_event(
                &domain_session("pruned"),
                8,
                domain::SessionEventPayload::ProcessExited(domain::ExitResult::ExitCode(3)),
            ),
        ],
        5,
    );
    let backend_trait: Arc<dyn TerminalBackend> = backend.clone();
    let leases = LeaseManager::new(backend_trait.clone(), Duration::from_secs(30)).unwrap();
    let service = TerminalGrpcService::new(backend_trait, leases, "test");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(
                TerminalServiceServer::new(service)
                    .max_decoding_message_size(1024 * 1024)
                    .max_encoding_message_size(1024 * 1024),
            )
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let connect_path = socket.clone();
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_| {
            let path = connect_path.clone();
            async move { UnixStream::connect(path).await.map(TokioIo::new) }
        }))
        .await
        .unwrap();
    let mut client = TerminalServiceClient::new(channel);

    let info = client
        .get_server_info(pb::GetServerInfoRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.protocol_major, 1);
    assert!(
        info.capabilities
            .iter()
            .any(|value| value == "terminal.read.v1")
    );

    let acquired = client
        .acquire_control(pb::AcquireControlRequest {
            session: Some(session()),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(acquired.expires_in_ms, 30_000);
    let event_state = acquired.event_state.unwrap();
    assert_eq!(event_state.last_committed_seq, 3);
    assert_eq!(event_state.acked_through_seq, 0);
    let token = acquired.control_token;

    let busy = client
        .acquire_control(pb::AcquireControlRequest {
            session: Some(session()),
        })
        .await
        .unwrap_err();
    assert_eq!(busy.code(), tonic::Code::FailedPrecondition);
    let carrier = RpcStatus::decode(busy.details()).unwrap();
    assert_eq!(carrier.code, tonic::Code::FailedPrecondition as i32);
    let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.reason, pb::ErrorReason::ControlBusy as i32);

    let started = client
        .start(pb::StartRequest {
            control: Some(control(&token)),
            program: "/bin/true".into(),
            args: Vec::new(),
            cwd: "/".into(),
            env: Some(pb::EnvironmentSnapshot {
                entries: Vec::new(),
            }),
            size: Some(pb::TerminalSize {
                rows: 30,
                columns: 120,
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(started.terminal_id, "terminal-fixture");
    assert!(backend.starts.lock().unwrap()[0].env.is_empty());

    let sent = client
        .send(pb::SendRequest {
            control: Some(control(&token)),
            terminal_id: started.terminal_id.clone(),
            data: vec![0, 0xff, 0x80, b'A'],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sent.written_bytes, 4);

    let oversized = client
        .send(pb::SendRequest {
            control: Some(control(&token)),
            terminal_id: started.terminal_id.clone(),
            data: vec![0; 256 * 1024 + 1],
        })
        .await
        .unwrap_err();
    assert_eq!(oversized.code(), tonic::Code::InvalidArgument);

    let unknown_terminal = client
        .read(pb::ReadRequest {
            terminal: Some(pb::TerminalRef {
                session: Some(session()),
                terminal_id: "missing".into(),
            }),
            position: Some(pb::read_request::Position::Earliest(
                pb::ReadFromEarliest {},
            )),
            limits: None,
        })
        .await
        .unwrap_err();
    assert_eq!(unknown_terminal.code(), tonic::Code::NotFound);

    let missing_position = client
        .read(pb::ReadRequest {
            terminal: Some(pb::TerminalRef {
                session: Some(session()),
                terminal_id: started.terminal_id.clone(),
            }),
            position: None,
            limits: None,
        })
        .await
        .unwrap_err();
    assert_eq!(missing_position.code(), tonic::Code::InvalidArgument);

    let read = client
        .read(pb::ReadRequest {
            terminal: Some(pb::TerminalRef {
                session: Some(session()),
                terminal_id: started.terminal_id.clone(),
            }),
            position: Some(pb::read_request::Position::Earliest(
                pb::ReadFromEarliest {},
            )),
            limits: Some(pb::QueryLimits {
                max_lines: None,
                max_bytes: None,
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.cursor.unwrap().end_line, 1);
    assert!(read.fragments.is_empty());

    let tail = client
        .tail(pb::TailRequest {
            terminal: Some(pb::TerminalRef {
                session: Some(session()),
                terminal_id: started.terminal_id.clone(),
            }),
            limits: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(tail.tail.unwrap().text, "pending");

    // S8: watching is lease-free and replays ordered pages from the
    // explicit after position; full event identity and payload survive.
    let mut replay = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            session: Some(session()),
            after_event_seq: 0,
        })
        .await
        .unwrap()
        .into_inner();
    let batch = replay.message().await.unwrap().unwrap();
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.event_seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(batch.state.as_ref().unwrap().last_committed_seq, 3);
    assert_eq!(batch.state.as_ref().unwrap().acked_through_seq, 0);
    let output_event = &batch.events[1];
    assert_eq!(output_event.terminal_id, "terminal-fixture");
    assert_eq!(output_event.session.as_ref().unwrap().external_id, "s1");
    match output_event.payload.as_ref().unwrap() {
        pb::session_event::Payload::OutputClosed(end) => {
            assert!(matches!(end.end, Some(pb::output_end::End::Eof(_))));
        }
        other => panic!("unexpected event payload: {other:?}"),
    }
    let exit_event = &batch.events[2];
    match exit_event.payload.as_ref().unwrap() {
        pb::session_event::Payload::ProcessExited(result) => {
            assert!(matches!(
                result.result,
                Some(pb::exit_result::Result::Signal(9))
            ));
        }
        other => panic!("unexpected event payload: {other:?}"),
    }
    drop(replay);

    // The after position is exclusive: only event 3 follows event 2.
    let mut resumed = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            session: Some(session()),
            after_event_seq: 2,
        })
        .await
        .unwrap()
        .into_inner();
    let batch = resumed.message().await.unwrap().unwrap();
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.event_seq)
            .collect::<Vec<_>>(),
        vec![3]
    );
    drop(resumed);

    // A caught-up subscription first receives an empty batch that
    // establishes the current watermark.
    let mut idle = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            session: Some(session()),
            after_event_seq: 3,
        })
        .await
        .unwrap()
        .into_inner();
    let watermark = idle.message().await.unwrap().unwrap();
    assert!(watermark.events.is_empty());
    assert_eq!(watermark.state.as_ref().unwrap().last_committed_seq, 3);
    drop(idle);

    // Lease-gated cumulative ack: only the controlling client, monotonic,
    // harmless repeats, and a bound beyond the committed prefix refused.
    let acked = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(control(&token)),
            up_to_seq: 2,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(acked.state.as_ref().unwrap().acked_through_seq, 2);
    let repeated = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(control(&token)),
            up_to_seq: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(repeated.state.as_ref().unwrap().acked_through_seq, 2);
    let advanced = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(control(&token)),
            up_to_seq: 3,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(advanced.state.as_ref().unwrap().acked_through_seq, 3);
    assert_eq!(advanced.state.as_ref().unwrap().last_committed_seq, 3);
    let out_of_bounds = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(control(&token)),
            up_to_seq: 4,
        })
        .await
        .unwrap_err();
    assert_eq!(out_of_bounds.code(), tonic::Code::InvalidArgument);

    // A replay before the pruned prefix terminates the stream with the
    // typed cleared-history error and explicit recovery bounds.
    let mut cleared_watch = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            session: Some(pruned_session()),
            after_event_seq: 3,
        })
        .await
        .unwrap()
        .into_inner();
    let cleared = cleared_watch.message().await.unwrap_err();
    assert_eq!(cleared.code(), tonic::Code::FailedPrecondition);
    let carrier = RpcStatus::decode(cleared.details()).unwrap();
    assert_eq!(carrier.code, tonic::Code::FailedPrecondition as i32);
    let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.reason, pb::ErrorReason::EventRangeCleared as i32);
    match detail.payload.unwrap() {
        pb::error_detail::Payload::EventRangeCleared(details) => {
            assert_eq!(details.after_event_seq, 3);
            assert_eq!(details.pruned_through_seq, 5);
            assert_eq!(details.available_after_seq, 5);
        }
        other => panic!("unexpected error payload: {other:?}"),
    }
    drop(cleared_watch);

    // Resuming exactly at the recovered bound replays what remains.
    let mut recovered = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            session: Some(pruned_session()),
            after_event_seq: 5,
        })
        .await
        .unwrap()
        .into_inner();
    let batch = recovered.message().await.unwrap().unwrap();
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.event_seq)
            .collect::<Vec<_>>(),
        vec![6, 7, 8]
    );
    assert_eq!(batch.state.as_ref().unwrap().pruned_through_seq, 5);
    drop(recovered);

    let stopped = client
        .stop(pb::StopRequest {
            control: Some(control(&token)),
            terminal_id: started.terminal_id,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(stopped.snapshot.unwrap().stopping);

    client
        .release_control(pb::ReleaseControlRequest {
            control: Some(control(&token)),
        })
        .await
        .unwrap();
    let stale = client
        .renew_control(pb::RenewControlRequest {
            control: Some(control(&token)),
        })
        .await
        .unwrap_err();
    assert_eq!(stale.code(), tonic::Code::FailedPrecondition);

    // An ack through a released token is refused like every other
    // mutation: control_expired, never a silent acknowledgement.
    let stale_ack = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(control(&token)),
            up_to_seq: 3,
        })
        .await
        .unwrap_err();
    assert_eq!(stale_ack.code(), tonic::Code::FailedPrecondition);
    let carrier = RpcStatus::decode(stale_ack.details()).unwrap();
    let detail = pb::ErrorDetail::decode(carrier.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.reason, pb::ErrorReason::ControlExpired as i32);

    shutdown_tx.send(()).unwrap();
    server.await.unwrap();
    drop(guard);
    assert!(!socket.exists());
}

/// S8 follow semantics: an open watch picks up newly committed events on the
/// bounded idle poll, dropping the stream cancels following, a resubscription
/// from the last fully received position never replays delivered events, and
/// a watermark that moved while idle re-emits an empty batch.
#[tokio::test]
async fn watch_session_events_follows_new_commits_and_continues_after_cancellation() {
    let root = TempDir::new();
    let socket = root.0.join("daemon.sock");
    let bound = BoundUnixSocket::bind(&socket).unwrap();
    let (listener, guard) = bound.into_parts();

    let backend = Arc::new(FakeBackend::new());
    let session = domain_session("follow");
    backend.seed_events(&session, Vec::new(), 0);
    let backend_trait: Arc<dyn TerminalBackend> = backend.clone();
    let leases = LeaseManager::new(backend_trait.clone(), Duration::from_secs(30)).unwrap();
    let service = TerminalGrpcService::new(backend_trait, leases, "test");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(
                TerminalServiceServer::new(service)
                    .max_decoding_message_size(1024 * 1024)
                    .max_encoding_message_size(1024 * 1024),
            )
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let connect_path = socket.clone();
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_| {
            let path = connect_path.clone();
            async move { UnixStream::connect(path).await.map(TokioIo::new) }
        }))
        .await
        .unwrap();
    let mut client = TerminalServiceClient::new(channel);

    let watch_session = || pb::WatchSessionEventsRequest {
        session: Some(pb::SessionRef {
            source: session.source.as_str().into(),
            external_id: session.external_id.as_str().into(),
        }),
        after_event_seq: 0,
    };

    // Caught up from zero: the first batch is empty and establishes the
    // all-zero watermark.
    let mut watch = client
        .watch_session_events(watch_session())
        .await
        .unwrap()
        .into_inner();
    let watermark = watch.message().await.unwrap().unwrap();
    assert!(watermark.events.is_empty());
    assert_eq!(watermark.state.as_ref().unwrap().last_committed_seq, 0);

    // Commits while the stream is open are followed in order.
    backend.commit_event(
        &session,
        domain::SessionEventPayload::ProcessExited(domain::ExitResult::ExitCode(0)),
    );
    backend.commit_event(
        &session,
        domain::SessionEventPayload::OutputClosed(domain::OutputEnd::Eof),
    );
    let batch = watch.message().await.unwrap().unwrap();
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.event_seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(batch.state.as_ref().unwrap().last_committed_seq, 2);

    // Dropping the stream cancels following; commits made afterwards are
    // not pushed anywhere.
    drop(watch);
    backend.commit_event(
        &session,
        domain::SessionEventPayload::ProcessExited(domain::ExitResult::Signal(15)),
    );

    // Resubscribing from the last fully received position continues with
    // event 3 only — no replay of events the consumer already saw.
    let mut resumed = client
        .watch_session_events(pb::WatchSessionEventsRequest {
            after_event_seq: 2,
            ..watch_session()
        })
        .await
        .unwrap()
        .into_inner();
    let batch = resumed.message().await.unwrap().unwrap();
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.event_seq)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert_eq!(batch.state.as_ref().unwrap().acked_through_seq, 0);

    // A watermark that moved while idle (an ack from the controlling
    // client) re-emits an empty batch carrying the new watermarks.
    let acquired = client
        .acquire_control(pb::AcquireControlRequest {
            session: watch_session().session,
        })
        .await
        .unwrap()
        .into_inner();
    let token = acquired.control_token;
    let acked = client
        .ack_session_events(pb::AckSessionEventsRequest {
            control: Some(pb::ControlContext {
                session: watch_session().session,
                control_token: token.clone(),
            }),
            up_to_seq: 3,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(acked.state.as_ref().unwrap().acked_through_seq, 3);
    let moved = resumed.message().await.unwrap().unwrap();
    assert!(moved.events.is_empty());
    assert_eq!(moved.state.as_ref().unwrap().acked_through_seq, 3);
    assert_eq!(moved.state.as_ref().unwrap().last_committed_seq, 3);
    drop(resumed);

    client
        .release_control(pb::ReleaseControlRequest {
            control: Some(pb::ControlContext {
                session: watch_session().session,
                control_token: token,
            }),
        })
        .await
        .unwrap();

    shutdown_tx.send(()).unwrap();
    server.await.unwrap();
    drop(guard);
    assert!(!socket.exists());
}

/// S8 cancellation regression (deterministic, paused clock): an idle
/// (caught-up, watermark-stable) watch must stop reading the durable
/// store once the client drops the stream, proven with the backend's
/// event-read counter — before the fix the loop only noticed a dropped
/// receiver on its next send and polled forever while idle.
#[tokio::test(start_paused = true)]
async fn idle_watch_stops_polling_after_the_stream_is_dropped() {
    let backend = Arc::new(FakeBackend::new());
    let session = domain_session("idle-cancel");
    backend.seed_events(&session, Vec::new(), 0);
    let backend_trait: Arc<dyn TerminalBackend> = backend.clone();
    let leases = LeaseManager::new(backend_trait.clone(), Duration::from_secs(30)).unwrap();
    let service = TerminalGrpcService::new(backend_trait, leases, "test");

    let response = service
        .watch_session_events(Request::new(pb::WatchSessionEventsRequest {
            session: Some(pb::SessionRef {
                source: session.source.as_str().into(),
                external_id: session.external_id.as_str().into(),
            }),
            after_event_seq: 0,
        }))
        .await
        .unwrap();
    let mut stream = response.into_inner();
    // Caught up from zero: the first batch is the empty watermark batch.
    let watermark = stream.next().await.unwrap().unwrap();
    assert!(watermark.events.is_empty());
    assert!(watermark.state.as_ref().unwrap().last_committed_seq == 0);
    drop(stream);

    // Settle past several idle poll intervals and freeze the read count.
    for _ in 0..8 {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    let settled = backend.event_reads.load(Ordering::SeqCst);

    // Far more idle time than the poll interval must not produce another
    // backend read: the follow loop observed the closed channel.
    for _ in 0..16 {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    assert_eq!(
        backend.event_reads.load(Ordering::SeqCst),
        settled,
        "idle follow must stop reading events after the stream is dropped"
    );
}
