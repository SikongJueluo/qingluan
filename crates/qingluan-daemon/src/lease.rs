//! In-memory session control leases.
//!
//! Tokens are random, process-local and never persisted or logged. Every
//! grant, release and expiry advances the terminal runtime's internal control
//! generation while holding the same lease-table lock used for validation.
//! This makes a stale token unable to commit another write fragment.
//!
//! Those generation transitions are additionally serialized with
//! acknowledgements through a per-session async operation gate: an ack
//! validates under the gate and holds it across its backend commit, so a
//! release, expiry or new-controller takeover can never advance the
//! generation past a parked ack. The gate map is process-lifetime state
//! (one entry per session ever controlled) because removing entries could
//! let two gates coexist for one session; the short-lived lease-table
//! `Mutex` is never held across an await.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qingluan_core::terminal::{ControlGeneration, SessionEventState, SessionRef};
use qingluan_terminal::RuntimeError;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::Instant;
use uuid::Uuid;

use crate::backend::TerminalBackend;

pub const MAX_LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct LeaseManager {
    inner: Arc<LeaseInner>,
}

struct LeaseInner {
    backend: Arc<dyn TerminalBackend>,
    ttl: Duration,
    leases: Mutex<HashMap<SessionRef, Lease>>,
    /// Per-session lease-operation gates (see the module docs). Fetched
    /// under a brief std lock, then awaited; never held across an await.
    gates: Mutex<HashMap<SessionRef, Arc<AsyncMutex<()>>>>,
}

struct Lease {
    token: SecretToken,
    generation: ControlGeneration,
    expires_at: Instant,
}

impl LeaseInner {
    fn session_gate(self: &Arc<Self>, session: &SessionRef) -> Arc<AsyncMutex<()>> {
        let mut gates = self.gates.lock().expect("lease gates");
        gates.entry(session.clone()).or_default().clone()
    }
}

/// Holds a session's lease-operation gate across one acknowledgement's
/// backend commit. Dropping the guard (after the commit finished) re-opens
/// release, expiry and takeover for that session.
pub struct LeaseAckGuard {
    _gate: OwnedMutexGuard<()>,
}

/// A newly granted lease. Deliberately has no `Debug` implementation: the
/// token must never enter tracing, errors, or model-visible diagnostics.
pub struct LeaseGrant {
    pub control_token: String,
    pub expires_in: Duration,
    pub event_state: SessionEventState,
}

struct PendingGrant {
    manager: LeaseManager,
    session: SessionRef,
    token: SecretToken,
    armed: bool,
}

impl PendingGrant {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGrant {
    fn drop(&mut self) {
        if self.armed {
            self.manager.revoke_unpublished(&self.session, self.token);
        }
    }
}

#[derive(Debug)]
pub enum LeaseError {
    Busy { remaining: Duration },
    Expired,
    Runtime(RuntimeError),
}

impl From<RuntimeError> for LeaseError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

#[derive(Clone, Copy)]
struct SecretToken([u8; 16]);

impl SecretToken {
    fn mint() -> Self {
        Self(*Uuid::new_v4().as_bytes())
    }

    fn encode(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0u8; 32];
        for (index, byte) in self.0.iter().copied().enumerate() {
            out[index * 2] = HEX[usize::from(byte >> 4)];
            out[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        String::from_utf8(out.to_vec()).expect("hex is UTF-8")
    }

    fn parse(value: &str) -> Option<Self> {
        if value.len() != 32 || !value.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = (hex_nibble(value.as_bytes()[offset])? << 4)
                | hex_nibble(value.as_bytes()[offset + 1])?;
        }
        Some(Self(bytes))
    }

    /// Constant-work equality for fixed-size secret tokens.
    fn matches(self, other: Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl LeaseManager {
    pub fn new(backend: Arc<dyn TerminalBackend>, ttl: Duration) -> Result<Self, &'static str> {
        if ttl.is_zero() {
            return Err("control lease TTL must be non-zero");
        }
        if ttl > MAX_LEASE_TTL {
            return Err("control lease TTL must not exceed 24 hours");
        }
        Ok(Self {
            inner: Arc::new(LeaseInner {
                backend,
                ttl,
                leases: Mutex::new(HashMap::new()),
                gates: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Acquire the session's lease-operation gate: every path that grants,
    /// invalidates, removes or replaces a lease (and therefore advances the
    /// control generation) runs under this guard, and an acknowledgement
    /// holds it from validation until its backend commit completes.
    async fn lease_gate(&self, session: &SessionRef) -> OwnedMutexGuard<()> {
        self.inner.session_gate(session).lock_owned().await
    }

    /// Acquire a fresh token. A live holder is never preempted. An expired
    /// holder is invalidated by advancing the runtime generation before the
    /// replacement token becomes visible.
    pub async fn acquire(&self, session: &SessionRef) -> Result<LeaseGrant, LeaseError> {
        // Acquisition explicitly creates the durable session identity before
        // an in-memory token is granted.
        self.inner.backend.ensure_session(session).await?;

        let token = SecretToken::mint();
        // The grant (or expired-holder replacement) is a generation
        // transition: it queues behind any parked ack on the session's
        // lease-operation gate, so a takeover always commits after it.
        let gate = self.lease_gate(session).await;
        // Read the clock only after waiting for the gate. Otherwise a long
        // in-flight ack could make an already-expired holder look live, or
        // create a replacement whose deadline had already elapsed.
        let now = Instant::now();
        let generation = {
            let mut leases = self.inner.leases.lock().expect("lease table");
            if let Some(existing) = leases.get(session)
                && existing.expires_at > now
            {
                return Err(LeaseError::Busy {
                    remaining: existing.expires_at.saturating_duration_since(now),
                });
            }
            let generation = self.inner.backend.advance_control_generation(session)?;
            leases.insert(
                session.clone(),
                Lease {
                    token,
                    generation,
                    expires_at: now + self.inner.ttl,
                },
            );
            generation
        };
        drop(gate);

        // Arm cleanup before the next await: cancellation or persistence
        // failure cannot strand an unpublished token in the lease table.
        let mut pending = PendingGrant {
            manager: self.clone(),
            session: session.clone(),
            token,
            armed: true,
        };
        self.spawn_expiry(session.clone(), token);
        // Read after the grant so the response is as current as possible.
        let event_state = self.inner.backend.ensure_session(session).await?;
        pending.disarm();
        let _ = generation; // generation is intentionally server-internal.
        Ok(LeaseGrant {
            control_token: token.encode(),
            expires_in: self.inner.ttl,
            event_state,
        })
    }

    /// Validate one mutation request and return only the server-owned
    /// generation. The wire never carries a generation number. Validation
    /// runs under the session's lease-operation gate because the lazy
    /// expiry invalidation it may perform is itself a generation
    /// transition that must not pass a parked ack; the guard is dropped
    /// before the caller's backend work, where the runtime's generation
    /// barrier takes over (Start/Send/Stop may still be preempted by a
    /// release and must fail their side effect, not block it).
    pub async fn validate(
        &self,
        session: &SessionRef,
        control_token: &str,
    ) -> Result<ControlGeneration, LeaseError> {
        let _gate = self.lease_gate(session).await;
        self.validate_gated(session, control_token)
    }

    fn validate_gated(
        &self,
        session: &SessionRef,
        control_token: &str,
    ) -> Result<ControlGeneration, LeaseError> {
        let supplied = SecretToken::parse(control_token).ok_or(LeaseError::Expired)?;
        let now = Instant::now();
        let mut leases = self.inner.leases.lock().expect("lease table");
        let Some(existing) = leases.get(session) else {
            return Err(LeaseError::Expired);
        };
        if !existing.token.matches(supplied) {
            return Err(LeaseError::Expired);
        }
        if existing.expires_at <= now {
            self.inner.backend.advance_control_generation(session)?;
            leases.remove(session);
            return Err(LeaseError::Expired);
        }
        Ok(existing.generation)
    }

    /// Begin a lease-held acknowledgement: validate the token under the
    /// session's lease-operation gate and return a guard that the caller
    /// must hold until the backend ack commit completes. While the guard
    /// is held, no release, expiry or takeover can advance or replace the
    /// lease, so the ack's authorization cannot be invalidated between
    /// validation and commit.
    pub async fn begin_ack(
        &self,
        session: &SessionRef,
        control_token: &str,
    ) -> Result<LeaseAckGuard, LeaseError> {
        let gate = self.lease_gate(session).await;
        self.validate_gated(session, control_token)?;
        Ok(LeaseAckGuard { _gate: gate })
    }

    /// Renew a held lease. Gated because an expired token's renewal
    /// invalidates the generation.
    pub async fn renew(
        &self,
        session: &SessionRef,
        control_token: &str,
    ) -> Result<Duration, LeaseError> {
        let supplied = SecretToken::parse(control_token).ok_or(LeaseError::Expired)?;
        let _gate = self.lease_gate(session).await;
        let now = Instant::now();
        let mut leases = self.inner.leases.lock().expect("lease table");
        let Some(existing) = leases.get_mut(session) else {
            return Err(LeaseError::Expired);
        };
        if !existing.token.matches(supplied) {
            return Err(LeaseError::Expired);
        }
        if existing.expires_at <= now {
            self.inner.backend.advance_control_generation(session)?;
            leases.remove(session);
            return Err(LeaseError::Expired);
        }
        existing.expires_at = now + self.inner.ttl;
        Ok(self.inner.ttl)
    }

    /// Release a held lease. Gated: the release's generation advance is the
    /// linearization point clients observe, and it must queue behind any
    /// ack that already validated.
    pub async fn release(
        &self,
        session: &SessionRef,
        control_token: &str,
    ) -> Result<(), LeaseError> {
        let supplied = SecretToken::parse(control_token).ok_or(LeaseError::Expired)?;
        let _gate = self.lease_gate(session).await;
        let now = Instant::now();
        let mut leases = self.inner.leases.lock().expect("lease table");
        let Some(existing) = leases.get(session) else {
            return Err(LeaseError::Expired);
        };
        if !existing.token.matches(supplied) || existing.expires_at <= now {
            if existing.token.matches(supplied) && existing.expires_at <= now {
                self.inner.backend.advance_control_generation(session)?;
                leases.remove(session);
            }
            return Err(LeaseError::Expired);
        }
        self.inner.backend.advance_control_generation(session)?;
        leases.remove(session);
        Ok(())
    }

    fn revoke_unpublished(&self, session: &SessionRef, token: SecretToken) {
        // Ungated on purpose: this token was never handed to a client, so
        // no acknowledgement can hold the session's lease-operation gate
        // against it (a grant only becomes visible in the table after the
        // gate is released, and a live predecessor makes acquire Busy).
        let mut leases = self.inner.leases.lock().expect("lease table");
        if leases
            .get(session)
            .is_some_and(|lease| lease.token.matches(token))
        {
            // Failure to advance is intentionally loud at the original
            // persistence error; keeping no client-visible token is safer
            // than retaining a secret that was never returned.
            let _ = self.inner.backend.advance_control_generation(session);
            leases.remove(session);
        }
    }

    fn spawn_expiry(&self, session: SessionRef, token: SecretToken) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                let deadline = {
                    let leases = inner.leases.lock().expect("lease table");
                    let Some(lease) = leases.get(&session) else {
                        return;
                    };
                    if !lease.token.matches(token) {
                        return;
                    }
                    lease.expires_at
                };
                tokio::time::sleep_until(deadline).await;
                // Expiry invalidation is a generation transition: take the
                // session's lease-operation gate so it cannot pass a
                // parked ack either.
                let gate = inner.session_gate(&session).lock_owned().await;
                let error = {
                    let mut leases = inner.leases.lock().expect("lease table");
                    let Some(lease) = leases.get(&session) else {
                        return;
                    };
                    if !lease.token.matches(token) {
                        return;
                    }
                    let now = Instant::now();
                    if lease.expires_at > now {
                        continue;
                    }
                    match inner.backend.advance_control_generation(&session) {
                        Ok(_) => {
                            leases.remove(&session);
                            return;
                        }
                        Err(error) => error,
                    }
                };
                drop(gate);
                tracing::error!(
                    session_source = session.source.as_str(),
                    session_id = session.external_id.as_str(),
                    %error,
                    "failed to invalidate expired control generation"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use qingluan_core::terminal::{
        EventPage, ExternalSessionId, LogIdentity, ReadLimits, ReadRequest, ReadResult,
        SendReceipt, SessionSource, StartSpec, TailView, TerminalRef, TerminalSnapshot,
    };
    use qingluan_terminal::{RuntimeError, SendError};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::Barrier;

    struct GenerationBackend {
        generation: AtomicU64,
        ensure_calls: AtomicU64,
        park_second_ensure: bool,
        second_ensure_entered: Barrier,
    }

    impl GenerationBackend {
        fn new() -> Self {
            Self {
                generation: AtomicU64::new(1),
                ensure_calls: AtomicU64::new(0),
                park_second_ensure: false,
                second_ensure_entered: Barrier::new(2),
            }
        }

        fn parking_second_ensure() -> Self {
            Self {
                park_second_ensure: true,
                ..Self::new()
            }
        }
    }

    #[async_trait]
    impl TerminalBackend for GenerationBackend {
        fn advance_control_generation(
            &self,
            _session: &SessionRef,
        ) -> Result<ControlGeneration, RuntimeError> {
            let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            ControlGeneration::new(next).ok_or(RuntimeError::ControlGenerationExhausted)
        }

        async fn ensure_session(
            &self,
            _session: &SessionRef,
        ) -> Result<SessionEventState, RuntimeError> {
            let call = self.ensure_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.park_second_ensure && call == 2 {
                self.second_ensure_entered.wait().await;
                std::future::pending::<()>().await;
            }
            Ok(SessionEventState::new(0, 0, 0).unwrap())
        }

        async fn events_after(
            &self,
            _session: &SessionRef,
            _after_event_seq: u64,
        ) -> Result<EventPage, RuntimeError> {
            unreachable!()
        }

        async fn ack_events(
            &self,
            _session: &SessionRef,
            _up_to_seq: u64,
        ) -> Result<SessionEventState, RuntimeError> {
            unreachable!()
        }

        async fn start(
            &self,
            _session: &SessionRef,
            _generation: ControlGeneration,
            _spec: StartSpec,
        ) -> Result<TerminalRef, RuntimeError> {
            unreachable!()
        }

        async fn send(
            &self,
            _terminal: &TerminalRef,
            _generation: ControlGeneration,
            _data: Vec<u8>,
        ) -> Result<SendReceipt, SendError> {
            unreachable!()
        }

        async fn stop_with_generation(
            &self,
            _terminal: &TerminalRef,
            _generation: ControlGeneration,
        ) -> Result<TerminalSnapshot, RuntimeError> {
            unreachable!()
        }

        async fn log_identity(&self, _terminal: &TerminalRef) -> Result<LogIdentity, RuntimeError> {
            unreachable!()
        }

        async fn read(&self, _request: &ReadRequest) -> Result<ReadResult, RuntimeError> {
            unreachable!()
        }

        async fn tail(
            &self,
            _terminal: &TerminalRef,
            _limits: ReadLimits,
        ) -> Result<TailView, RuntimeError> {
            unreachable!()
        }
    }

    fn session() -> SessionRef {
        SessionRef {
            source: SessionSource::new("pi"),
            external_id: ExternalSessionId::new("s1"),
        }
    }

    #[test]
    fn lease_ttl_is_bounded_before_deadline_arithmetic() {
        let backend = Arc::new(GenerationBackend::new());
        assert!(LeaseManager::new(backend.clone(), Duration::ZERO).is_err());
        assert!(LeaseManager::new(backend.clone(), MAX_LEASE_TTL).is_ok());
        assert!(LeaseManager::new(backend, MAX_LEASE_TTL + Duration::from_secs(1)).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_renew_release_and_expiry_advance_generations() {
        let backend = Arc::new(GenerationBackend::new());
        let manager = LeaseManager::new(backend.clone(), Duration::from_secs(30)).unwrap();
        let session = session();

        let first = manager.acquire(&session).await.unwrap();
        assert_eq!(first.expires_in, Duration::from_secs(30));
        assert_eq!(backend.generation.load(Ordering::SeqCst), 2);
        assert!(matches!(
            manager.acquire(&session).await,
            Err(LeaseError::Busy { .. })
        ));
        assert_eq!(
            manager
                .validate(&session, &first.control_token)
                .await
                .unwrap()
                .get(),
            2
        );

        tokio::time::advance(Duration::from_secs(20)).await;
        manager.renew(&session, &first.control_token).await.unwrap();
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;
        assert!(
            manager
                .validate(&session, &first.control_token)
                .await
                .is_ok()
        );

        manager
            .release(&session, &first.control_token)
            .await
            .unwrap();
        assert_eq!(backend.generation.load(Ordering::SeqCst), 3);
        assert!(matches!(
            manager.validate(&session, &first.control_token).await,
            Err(LeaseError::Expired)
        ));

        let second = manager.acquire(&session).await.unwrap();
        assert_ne!(first.control_token, second.control_token);
        assert_eq!(backend.generation.load(Ordering::SeqCst), 4);
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            manager.validate(&session, &second.control_token).await,
            Err(LeaseError::Expired)
        ));
        assert_eq!(backend.generation.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn cancelling_acquire_cannot_strand_an_unpublished_lease() {
        let backend = Arc::new(GenerationBackend::parking_second_ensure());
        let manager = LeaseManager::new(backend.clone(), Duration::from_secs(30)).unwrap();
        let session = session();
        let task = {
            let manager = manager.clone();
            let session = session.clone();
            tokio::spawn(async move { manager.acquire(&session).await })
        };
        backend.second_ensure_entered.wait().await;
        task.abort();
        let cancelled = match task.await {
            Err(error) => error,
            Ok(_) => panic!("parked acquisition unexpectedly completed"),
        };
        assert!(cancelled.is_cancelled());
        assert_eq!(backend.generation.load(Ordering::SeqCst), 3);

        let replacement = tokio::time::timeout(Duration::from_secs(1), manager.acquire(&session))
            .await
            .expect("cancelled grant must not leave the session busy")
            .unwrap();
        assert_eq!(replacement.expires_in, Duration::from_secs(30));
        assert_eq!(backend.generation.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn malformed_and_foreign_tokens_are_expired_and_never_echoed() {
        let backend = Arc::new(GenerationBackend::new());
        let manager = LeaseManager::new(backend, Duration::from_secs(30)).unwrap();
        let session = session();
        let grant = manager.acquire(&session).await.unwrap();
        assert_eq!(grant.control_token.len(), 32);
        assert!(
            grant
                .control_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );

        for token in ["", "not-a-token", "00000000000000000000000000000000"] {
            let error = manager.validate(&session, token).await.unwrap_err();
            let rendered = format!("{error:?}");
            assert!(!rendered.contains(token) || token.is_empty());
            assert!(matches!(error, LeaseError::Expired));
        }
        assert!(format!("{:?}", LeaseError::Expired).contains("Expired"));
        assert!(!format!("{:?}", LeaseError::Expired).contains(&grant.control_token));
    }
}
