use std::sync::Arc;
use std::sync::Mutex;
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

    async fn events_after(
        &self,
        _session: &domain::SessionRef,
        _after_event_seq: u64,
    ) -> Result<domain::EventPage, RuntimeError> {
        unreachable!()
    }

    async fn ack_events(
        &self,
        _session: &domain::SessionRef,
        _up_to_seq: u64,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        unreachable!()
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

/// Backend that parks `ack_events` exactly before its commit and records
/// the relative order of ack commits and control-generation advances, so
/// ack/lease linearization is provable deterministically (no real sleeps).
struct AckRaceBackend {
    generation: AtomicU64,
    acks_committed: AtomicU64,
    order: Mutex<Vec<&'static str>>,
    ack_entered: Barrier,
    ack_proceed: Semaphore,
}

impl AckRaceBackend {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(1),
            acks_committed: AtomicU64::new(0),
            order: Mutex::new(Vec::new()),
            ack_entered: Barrier::new(2),
            ack_proceed: Semaphore::new(0),
        }
    }

    fn record(&self, event: &'static str) {
        self.order.lock().unwrap().push(event);
    }

    fn recorded(&self) -> Vec<&'static str> {
        self.order.lock().unwrap().clone()
    }
}

#[async_trait]
impl TerminalBackend for AckRaceBackend {
    fn advance_control_generation(
        &self,
        _session: &domain::SessionRef,
    ) -> Result<domain::ControlGeneration, RuntimeError> {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.record("advance");
        domain::ControlGeneration::new(next).ok_or(RuntimeError::ControlGenerationExhausted)
    }

    async fn ensure_session(
        &self,
        _session: &domain::SessionRef,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        Ok(domain::SessionEventState::new(0, 0, 0).unwrap())
    }

    async fn events_after(
        &self,
        _session: &domain::SessionRef,
        _after_event_seq: u64,
    ) -> Result<domain::EventPage, RuntimeError> {
        unreachable!()
    }

    async fn ack_events(
        &self,
        _session: &domain::SessionRef,
        up_to_seq: u64,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        // The caller has already validated its lease; park exactly before
        // the durable commit so a racing release/expiry/takeover gets its
        // chance to (wrongly) pass the ack.
        self.ack_entered.wait().await;
        self.ack_proceed.acquire().await.unwrap().forget();
        self.acks_committed.fetch_add(1, Ordering::SeqCst);
        self.record("ack");
        Ok(domain::SessionEventState::new(0, up_to_seq, up_to_seq).unwrap())
    }

    async fn start(
        &self,
        _session: &domain::SessionRef,
        _generation: domain::ControlGeneration,
        _spec: domain::StartSpec,
    ) -> Result<domain::TerminalRef, RuntimeError> {
        unreachable!()
    }

    async fn send(
        &self,
        _terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
        _data: Vec<u8>,
    ) -> Result<domain::SendReceipt, SendError> {
        unreachable!()
    }

    async fn stop_with_generation(
        &self,
        _terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
    ) -> Result<domain::TerminalSnapshot, RuntimeError> {
        unreachable!()
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

/// S8 linearization proof: an ack that validated its lease and is parked
/// exactly before its backend commit cannot be passed by a release — the
/// release's generation advance (the linearization point clients observe)
/// happens strictly after the ack commit.
#[tokio::test(start_paused = true)]
async fn release_cannot_pass_a_parked_ack_commit() {
    let backend = Arc::new(AckRaceBackend::new());
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
    assert_eq!(backend.recorded(), ["advance"]);

    let ack_service = service.clone();
    let ack_token = token.clone();
    let ack = tokio::spawn(async move {
        ack_service
            .ack_session_events(Request::new(pb::AckSessionEventsRequest {
                control: Some(control(ack_token)),
                up_to_seq: 5,
            }))
            .await
    });
    // The ack holds the session's lease-operation gate and is parked at its
    // commit.
    backend.ack_entered.wait().await;

    let mut release = std::pin::pin!(service.release_control(Request::new(
        pb::ReleaseControlRequest {
            control: Some(control(token)),
        },
    )));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), release.as_mut())
            .await
            .is_err(),
        "release must queue behind the parked ack's lease gate"
    );

    backend.ack_proceed.add_permits(1);
    release.await.unwrap();
    ack.await.unwrap().unwrap();
    assert_eq!(backend.acks_committed.load(Ordering::SeqCst), 1);
    // The ack commit is recorded strictly before the release's generation
    // advance: the release hands the session on only after the ack landed.
    assert_eq!(backend.recorded(), ["advance", "ack", "advance"]);
}

/// S8 linearization proof: after the lease expires underneath a parked
/// ack, neither the expiry invalidation nor a new controller's takeover
/// may advance the generation before the ack commits; the previous token
/// stays unable to acknowledge afterwards.
#[tokio::test(start_paused = true)]
async fn takeover_after_expiry_cannot_pass_a_parked_ack_commit() {
    let backend = Arc::new(AckRaceBackend::new());
    let backend_trait: Arc<dyn TerminalBackend> = backend.clone();
    let leases = LeaseManager::new(backend_trait.clone(), Duration::from_secs(1)).unwrap();
    let service = TerminalGrpcService::new(backend_trait, leases, "test");
    let token = service
        .acquire_control(Request::new(pb::AcquireControlRequest {
            session: Some(session()),
        }))
        .await
        .unwrap()
        .into_inner()
        .control_token;

    let ack_service = service.clone();
    let ack_token = token.clone();
    let ack = tokio::spawn(async move {
        ack_service
            .ack_session_events(Request::new(pb::AckSessionEventsRequest {
                control: Some(control(ack_token)),
                up_to_seq: 3,
            }))
            .await
    });
    backend.ack_entered.wait().await;

    // The lease expires while the ack is parked at its commit.
    tokio::time::advance(Duration::from_secs(2)).await;

    let mut takeover = std::pin::pin!(service.acquire_control(Request::new(
        pb::AcquireControlRequest {
            session: Some(session()),
        },
    )));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), takeover.as_mut())
            .await
            .is_err(),
        "takeover must queue behind the parked ack's lease gate"
    );

    backend.ack_proceed.add_permits(1);
    ack.await.unwrap().unwrap();
    let replacement = takeover.await.unwrap().into_inner();
    assert_ne!(replacement.control_token, token);

    // The previous controller cannot acknowledge anything anymore.
    let stale = service
        .ack_session_events(Request::new(pb::AckSessionEventsRequest {
            control: Some(control(token)),
            up_to_seq: 3,
        }))
        .await
        .unwrap_err();
    assert_eq!(stale.code(), Code::FailedPrecondition);

    // The very first recorded event after the initial grant is the ack
    // commit: no generation transition (expiry invalidation or replacement
    // grant) passed the parked ack, and at least the takeover's grant
    // followed it.
    let recorded = backend.recorded();
    assert_eq!(recorded.first(), Some(&"advance"));
    assert_eq!(recorded.get(1), Some(&"ack"));
    assert!(recorded.iter().filter(|event| **event == "advance").count() >= 2);
}
