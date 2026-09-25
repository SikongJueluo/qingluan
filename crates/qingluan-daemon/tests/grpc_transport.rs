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
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalServiceServer;
use qingluan_terminal::{RuntimeError, SendError, SendRejection};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Endpoint, Server};
use tower::service_fn;

struct FakeBackend {
    generation: AtomicU64,
    starts: Mutex<Vec<domain::StartSpec>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(1),
            starts: Mutex::new(Vec::new()),
        }
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
        _session: &domain::SessionRef,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        Ok(domain::SessionEventState::new(0, 0, 0).unwrap())
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
    assert_eq!(acquired.event_state.unwrap().last_committed_seq, 0);
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

    shutdown_tx.send(()).unwrap();
    server.await.unwrap();
    drop(guard);
    assert!(!socket.exists());
}
