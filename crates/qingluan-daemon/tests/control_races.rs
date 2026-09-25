use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use qingluan_core::terminal as domain;
use qingluan_daemon::backend::TerminalBackend;
use qingluan_daemon::lease::LeaseManager;
use qingluan_daemon::service::TerminalGrpcService;
use qingluan_protocol::terminal::v1 as pb;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalService;
use qingluan_terminal::{RuntimeError, SendError, SendRejection};
use tokio::sync::{Barrier, Semaphore};
use tonic::{Code, Request, Status};

#[derive(Clone, Copy)]
enum Operation {
    Start,
    Send,
    Stop,
}

struct BoundaryBackend {
    generation: AtomicU64,
    entered: Barrier,
    proceed: Semaphore,
    effects: AtomicU64,
}

impl BoundaryBackend {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(1),
            entered: Barrier::new(2),
            proceed: Semaphore::new(0),
            effects: AtomicU64::new(0),
        }
    }

    async fn boundary(&self, generation: domain::ControlGeneration) -> bool {
        self.entered.wait().await;
        self.proceed.acquire().await.unwrap().forget();
        if self.generation.load(Ordering::SeqCst) == generation.get() {
            self.effects.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    fn terminal(session: &domain::SessionRef) -> domain::TerminalRef {
        domain::TerminalRef {
            session: session.clone(),
            terminal_id: domain::TerminalId::new("race-terminal"),
        }
    }
}

#[async_trait]
impl TerminalBackend for BoundaryBackend {
    fn advance_control_generation(
        &self,
        _session: &domain::SessionRef,
    ) -> Result<domain::ControlGeneration, RuntimeError> {
        let value = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        domain::ControlGeneration::new(value).ok_or(RuntimeError::ControlGenerationExhausted)
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
        generation: domain::ControlGeneration,
        _spec: domain::StartSpec,
    ) -> Result<domain::TerminalRef, RuntimeError> {
        let terminal = Self::terminal(session);
        if !self.boundary(generation).await {
            return Err(RuntimeError::ControlLost(terminal));
        }
        Ok(terminal)
    }

    async fn send(
        &self,
        _terminal: &domain::TerminalRef,
        generation: domain::ControlGeneration,
        data: Vec<u8>,
    ) -> Result<domain::SendReceipt, SendError> {
        if !self.boundary(generation).await {
            return Err(SendError::Rejected(SendRejection::ControlLost));
        }
        Ok(domain::SendReceipt::new(data.len() as u64))
    }

    async fn stop_with_generation(
        &self,
        terminal: &domain::TerminalRef,
        generation: domain::ControlGeneration,
    ) -> Result<domain::TerminalSnapshot, RuntimeError> {
        if !self.boundary(generation).await {
            return Err(RuntimeError::ControlLost(terminal.clone()));
        }
        unreachable!("the race must invalidate control before the side effect")
    }

    async fn log_identity(
        &self,
        _terminal: &domain::TerminalRef,
    ) -> Result<domain::LogIdentity, RuntimeError> {
        unreachable!()
    }

    async fn read(
        &self,
        _request: &domain::ReadRequest,
    ) -> Result<domain::ReadResult, RuntimeError> {
        unreachable!()
    }

    async fn tail(
        &self,
        _terminal: &domain::TerminalRef,
        _limits: domain::ReadLimits,
    ) -> Result<domain::TailView, RuntimeError> {
        unreachable!()
    }
}

fn session() -> pb::SessionRef {
    pb::SessionRef {
        source: "test".into(),
        external_id: "lease-race".into(),
    }
}

fn control(token: String) -> pb::ControlContext {
    pb::ControlContext {
        session: Some(session()),
        control_token: token,
    }
}

async fn run_release_race(operation: Operation) {
    let backend = Arc::new(BoundaryBackend::new());
    let backend_trait: Arc<dyn TerminalBackend> = backend.clone();
    let leases = LeaseManager::new(backend_trait.clone(), Duration::from_secs(30)).unwrap();
    let service = TerminalGrpcService::new(backend_trait, leases, "test");
    let token = service
        .acquire_control(Request::new(pb::AcquireControlRequest {
            session: Some(session()),
        }))
        .await
        .unwrap()
        .into_inner()
        .control_token;

    let operation_service = service.clone();
    let operation_token = token.clone();
    let task: tokio::task::JoinHandle<Result<(), Status>> = match operation {
        Operation::Start => tokio::spawn(async move {
            operation_service
                .start(Request::new(pb::StartRequest {
                    control: Some(control(operation_token)),
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
                }))
                .await
                .map(|_| ())
        }),
        Operation::Send => tokio::spawn(async move {
            operation_service
                .send(Request::new(pb::SendRequest {
                    control: Some(control(operation_token)),
                    terminal_id: "race-terminal".into(),
                    data: b"x".to_vec(),
                }))
                .await
                .map(|_| ())
        }),
        Operation::Stop => tokio::spawn(async move {
            operation_service
                .stop(Request::new(pb::StopRequest {
                    control: Some(control(operation_token)),
                    terminal_id: "race-terminal".into(),
                }))
                .await
                .map(|_| ())
        }),
    };

    // The adapter has validated the lease and the operation is parked exactly
    // before its backend side effect. Release must advance the generation;
    // once resumed, the stale operation is refused rather than committed.
    backend.entered.wait().await;
    service
        .release_control(Request::new(pb::ReleaseControlRequest {
            control: Some(control(token)),
        }))
        .await
        .unwrap();
    backend.proceed.add_permits(1);

    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(backend.effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn release_linearizes_before_start_side_effect() {
    run_release_race(Operation::Start).await;
}

#[tokio::test]
async fn release_linearizes_before_send_side_effect() {
    run_release_race(Operation::Send).await;
}

#[tokio::test]
async fn release_linearizes_before_stop_side_effect() {
    run_release_race(Operation::Stop).await;
}
