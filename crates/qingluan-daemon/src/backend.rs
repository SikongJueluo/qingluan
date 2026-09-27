//! Wire-independent backend seam for the terminal gRPC adapter.
//!
//! Production uses [`TerminalRuntime`]. Tests can provide an in-memory backend
//! so lease, protobuf and UDS behavior remains executable on hosts without
//! cgroup delegation; this never creates a production fallback.

use async_trait::async_trait;
use qingluan_core::terminal::{
    ControlGeneration, EventPage, LogIdentity, ReadRequest, ReadResult, SendReceipt,
    SessionEventState, SessionRef, StartSpec, TailView, TerminalRef, TerminalSnapshot,
};
use qingluan_terminal::{RuntimeError, SendError, TerminalRuntime};

#[async_trait]
pub trait TerminalBackend: Send + Sync + 'static {
    fn advance_control_generation(
        &self,
        session: &SessionRef,
    ) -> Result<ControlGeneration, RuntimeError>;

    async fn ensure_session(&self, session: &SessionRef)
    -> Result<SessionEventState, RuntimeError>;

    /// One bounded, ordered page of a session's committed lifecycle events
    /// strictly after `after_event_seq`, with consistent watermarks. A
    /// request before the pruned bound fails with
    /// [`RuntimeError::EventRangeCleared`].
    async fn events_after(
        &self,
        session: &SessionRef,
        after_event_seq: u64,
    ) -> Result<EventPage, RuntimeError>;

    /// Cumulative event acknowledgement; a bound beyond the committed
    /// bound fails with [`RuntimeError::EventAckOutOfBounds`] and writes
    /// nothing.
    async fn ack_events(
        &self,
        session: &SessionRef,
        up_to_seq: u64,
    ) -> Result<SessionEventState, RuntimeError>;

    async fn start(
        &self,
        session: &SessionRef,
        generation: ControlGeneration,
        spec: StartSpec,
    ) -> Result<TerminalRef, RuntimeError>;

    async fn send(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
        data: Vec<u8>,
    ) -> Result<SendReceipt, SendError>;

    async fn stop_with_generation(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
    ) -> Result<TerminalSnapshot, RuntimeError>;

    async fn log_identity(&self, terminal: &TerminalRef) -> Result<LogIdentity, RuntimeError>;

    async fn read(&self, request: &ReadRequest) -> Result<ReadResult, RuntimeError>;

    async fn tail(
        &self,
        terminal: &TerminalRef,
        limits: qingluan_core::terminal::ReadLimits,
    ) -> Result<TailView, RuntimeError>;
}

#[async_trait]
impl TerminalBackend for TerminalRuntime {
    fn advance_control_generation(
        &self,
        session: &SessionRef,
    ) -> Result<ControlGeneration, RuntimeError> {
        TerminalRuntime::advance_control_generation(self, session)
    }

    async fn ensure_session(
        &self,
        session: &SessionRef,
    ) -> Result<SessionEventState, RuntimeError> {
        TerminalRuntime::ensure_session(self, session).await
    }

    async fn events_after(
        &self,
        session: &SessionRef,
        after_event_seq: u64,
    ) -> Result<EventPage, RuntimeError> {
        TerminalRuntime::events_after(self, session, after_event_seq).await
    }

    async fn ack_events(
        &self,
        session: &SessionRef,
        up_to_seq: u64,
    ) -> Result<SessionEventState, RuntimeError> {
        TerminalRuntime::ack_events(self, session, up_to_seq).await
    }

    async fn start(
        &self,
        session: &SessionRef,
        generation: ControlGeneration,
        spec: StartSpec,
    ) -> Result<TerminalRef, RuntimeError> {
        TerminalRuntime::start(self, session, generation, spec).await
    }

    async fn send(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
        data: Vec<u8>,
    ) -> Result<SendReceipt, SendError> {
        TerminalRuntime::send(self, terminal, generation, data).await
    }

    async fn stop_with_generation(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
    ) -> Result<TerminalSnapshot, RuntimeError> {
        TerminalRuntime::stop_with_generation(self, terminal, generation).await
    }

    async fn log_identity(&self, terminal: &TerminalRef) -> Result<LogIdentity, RuntimeError> {
        TerminalRuntime::log_identity(self, terminal).await
    }

    async fn read(&self, request: &ReadRequest) -> Result<ReadResult, RuntimeError> {
        TerminalRuntime::read(self, request).await
    }

    async fn tail(
        &self,
        terminal: &TerminalRef,
        limits: qingluan_core::terminal::ReadLimits,
    ) -> Result<TailView, RuntimeError> {
        TerminalRuntime::tail(self, terminal, limits).await
    }
}
