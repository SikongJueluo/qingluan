//! Tonic `TerminalService` adapter.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use qingluan_core::terminal as domain;
use qingluan_protocol::terminal::v1 as pb;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalService;
use qingluan_terminal::RuntimeError;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::backend::TerminalBackend;
use crate::lease::LeaseManager;
use crate::wire;

/// How long the event follow loop idles between polls of the durable store
/// when no new events are committed. Polling the durable store while idle is
/// the accepted follow mechanism (there is no second event state machine and
/// nothing is published before commit), so the interval only bounds latency
/// and idle work; it is deliberately small, fixed, and independent of the
/// client so a slow consumer cannot increase server-side polling.
const EVENT_FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Bounded buffering between the follow loop and the gRPC stream: the loop
/// blocks on a full buffer (backpressure) instead of growing without bound,
/// and a consumer that stops draining still leaves the loop cancellable
/// through the channel's send failure.
const EVENT_STREAM_BUFFER: usize = 8;

type EventStream =
    Pin<Box<dyn Stream<Item = Result<pb::WatchSessionEventsResponse, Status>> + Send>>;

#[derive(Clone)]
pub struct TerminalGrpcService {
    backend: Arc<dyn TerminalBackend>,
    leases: LeaseManager,
    daemon_version: String,
}
impl TerminalGrpcService {
    pub fn new(
        backend: Arc<dyn TerminalBackend>,
        leases: LeaseManager,
        daemon_version: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            leases,
            daemon_version: daemon_version.into(),
        }
    }
}

#[tonic::async_trait]
impl TerminalService for TerminalGrpcService {
    type WatchSessionEventsStream = EventStream;

    async fn get_server_info(
        &self,
        _request: Request<pb::GetServerInfoRequest>,
    ) -> Result<Response<pb::GetServerInfoResponse>, Status> {
        Ok(Response::new(pb::GetServerInfoResponse {
            daemon_version: self.daemon_version.clone(),
            protocol_major: wire::PROTOCOL_MAJOR,
            protocol_minor: wire::PROTOCOL_MINOR,
            capabilities: vec![
                "terminal.control.v1".into(),
                "terminal.start.v1".into(),
                "terminal.send.v1".into(),
                "terminal.stop.v1".into(),
                "terminal.read.v1".into(),
                "terminal.tail.v1".into(),
                "terminal.events.v1".into(),
                "rich-error.google.rpc.status.v1".into(),
            ],
        }))
    }

    async fn acquire_control(
        &self,
        request: Request<pb::AcquireControlRequest>,
    ) -> Result<Response<pb::AcquireControlResponse>, Status> {
        let session = wire::session(request.into_inner().session)?;
        let grant = self
            .leases
            .acquire(&session)
            .await
            .map_err(wire::lease_status)?;
        Ok(Response::new(pb::AcquireControlResponse {
            control_token: grant.control_token,
            expires_in_ms: wire::duration_ms(grant.expires_in),
            event_state: Some(wire::event_state(grant.event_state)),
        }))
    }

    async fn renew_control(
        &self,
        request: Request<pb::RenewControlRequest>,
    ) -> Result<Response<pb::RenewControlResponse>, Status> {
        let (session, token) = wire::control(request.into_inner().control)?;
        let expires_in = self
            .leases
            .renew(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        Ok(Response::new(pb::RenewControlResponse {
            expires_in_ms: wire::duration_ms(expires_in),
        }))
    }

    async fn release_control(
        &self,
        request: Request<pb::ReleaseControlRequest>,
    ) -> Result<Response<pb::ReleaseControlResponse>, Status> {
        let (session, token) = wire::control(request.into_inner().control)?;
        self.leases
            .release(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        Ok(Response::new(pb::ReleaseControlResponse {}))
    }

    async fn start(
        &self,
        request: Request<pb::StartRequest>,
    ) -> Result<Response<pb::StartResponse>, Status> {
        let mut request = request.into_inner();
        let (session, token) = wire::control(request.control.take())?;
        let spec = wire::start_spec(&mut request)?;
        // Keep validation adjacent to the runtime's generation barrier: a
        // large but valid request cannot consume part of the lease first.
        let generation = self
            .leases
            .validate(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        let terminal = self
            .backend
            .start(&session, generation, spec)
            .await
            .map_err(wire::runtime_status)?;
        Ok(Response::new(pb::StartResponse {
            terminal_id: terminal.terminal_id.as_str().to_owned(),
        }))
    }

    async fn send(
        &self,
        request: Request<pb::SendRequest>,
    ) -> Result<Response<pb::SendResponse>, Status> {
        let request = request.into_inner();
        let (session, token) = wire::control(request.control)?;
        let generation = self
            .leases
            .validate(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        let terminal = wire::terminal_for_session(session, request.terminal_id)?;
        if request.data.len() > wire::MAX_SEND_BYTES {
            return Err(Status::invalid_argument("send payload exceeds 256 KiB"));
        }
        let receipt = self
            .backend
            .send(&terminal, generation, request.data)
            .await
            .map_err(|error| wire::send_status(error, &terminal))?;
        Ok(Response::new(pb::SendResponse {
            written_bytes: receipt.written_bytes,
        }))
    }

    async fn stop(
        &self,
        request: Request<pb::StopRequest>,
    ) -> Result<Response<pb::StopResponse>, Status> {
        let request = request.into_inner();
        let (session, token) = wire::control(request.control)?;
        let generation = self
            .leases
            .validate(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        let terminal = wire::terminal_for_session(session, request.terminal_id)?;
        let snapshot = self
            .backend
            .stop_with_generation(&terminal, generation)
            .await
            .map_err(wire::runtime_status)?;
        Ok(Response::new(pb::StopResponse {
            snapshot: Some(wire::snapshot(&snapshot)?),
        }))
    }

    async fn read(
        &self,
        request: Request<pb::ReadRequest>,
    ) -> Result<Response<pb::ReadResponse>, Status> {
        let request = request.into_inner();
        let terminal = wire::terminal(request.terminal)?;
        let limits = wire::limits(request.limits)?;
        let domain_request = match request.position {
            Some(pb::read_request::Position::Cursor(cursor)) => {
                let cursor = wire::read_cursor(cursor)?;
                if cursor.log().terminal != terminal {
                    return Err(wire::runtime_status(RuntimeError::CursorExpired {
                        earliest: None,
                        missing: None,
                    }));
                }
                domain::ReadRequest::resume(cursor, limits)
            }
            position => {
                let position = position
                    .ok_or_else(|| Status::invalid_argument("read position is required"))?;
                let log = self
                    .backend
                    .log_identity(&terminal)
                    .await
                    .map_err(wire::runtime_status)?;
                let start = match position {
                    pb::read_request::Position::Earliest(_) => domain::ReadStart::Earliest,
                    pb::read_request::Position::Newest(_) => domain::ReadStart::Newest,
                    pb::read_request::Position::At(position) => {
                        domain::ReadStart::At(wire::history_position(Some(position))?)
                    }
                    pb::read_request::Position::Cursor(_) => unreachable!(),
                };
                domain::ReadRequest::first(log, Some(start), limits)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?
            }
        };
        let result = self
            .backend
            .read(&domain_request)
            .await
            .map_err(wire::runtime_status)?;
        Ok(Response::new(wire::read_response(&result)?))
    }

    async fn tail(
        &self,
        request: Request<pb::TailRequest>,
    ) -> Result<Response<pb::TailResponse>, Status> {
        let request = request.into_inner();
        let terminal = wire::terminal(request.terminal)?;
        let limits = wire::limits(request.limits)?;
        let result = self
            .backend
            .tail(&terminal, limits)
            .await
            .map_err(wire::tail_status)?;
        Ok(Response::new(wire::tail_response(&result)?))
    }

    /// Server-streaming follow of a session's committed lifecycle events.
    ///
    /// Lease-free by design: watching is observation and never confirms
    /// consumption. The stream is a bounded, cancellation-safe follow loop
    /// over ordered durable pages — each batch continues explicitly from the
    /// last emitted `event_seq`, an empty batch only establishes the current
    /// watermark (initially, or when the watermarks moved while idle), and
    /// any backend failure (including a cleared replay range) terminates the
    /// stream explicitly with that status instead of being retried or
    /// skipped. Cancelling the RPC drops the receiver; the loop observes the
    /// closed channel on every send and, while caught up, races the idle
    /// poll sleep against channel closure, so a dropped stream stops
    /// following promptly instead of polling the store forever.
    async fn watch_session_events(
        &self,
        request: Request<pb::WatchSessionEventsRequest>,
    ) -> Result<Response<Self::WatchSessionEventsStream>, Status> {
        let request = request.into_inner();
        let session = wire::session(request.session)?;
        let mut after_event_seq = request.after_event_seq;
        let backend = Arc::clone(&self.backend);

        let (tx, rx) =
            mpsc::channel::<Result<pb::WatchSessionEventsResponse, Status>>(EVENT_STREAM_BUFFER);
        tokio::spawn(async move {
            // Watermark of the last emitted batch: an idle poll re-emits an
            // empty batch only when the watermarks actually moved (the
            // initial emission always establishes the watermark).
            let mut sent_state: Option<domain::SessionEventState> = None;
            loop {
                if tx.is_closed() {
                    // The receiver is gone: stop before touching the store
                    // again, so cancellation cannot leak polling work.
                    return;
                }
                let page = match backend.events_after(&session, after_event_seq).await {
                    Ok(page) => page,
                    Err(error) => {
                        // Storage degradation and a cleared range both end
                        // the stream with the typed status; the client decides
                        // whether and where to resubscribe.
                        let _ = tx.send(Err(wire::runtime_status(error))).await;
                        return;
                    }
                };
                let more_pages = page.next_after_seq.is_some();
                if let Some(event) = page.events.last() {
                    after_event_seq = event.event_seq.get();
                }
                let watermark_moved = sent_state != Some(page.state);
                if !page.events.is_empty() || watermark_moved {
                    let batch = match wire::watch_response(&page) {
                        Ok(batch) => batch,
                        Err(status) => {
                            // An unrepresentable payload is never published:
                            // fail closed instead of emitting an event a
                            // client could acknowledge blindly.
                            let _ = tx.send(Err(status)).await;
                            return;
                        }
                    };
                    sent_state = Some(page.state);
                    if tx.send(Ok(batch)).await.is_err() {
                        // Receiver dropped (client cancelled or disconnected):
                        // stop following; nothing is left to clean up.
                        return;
                    }
                }
                if !more_pages {
                    // Caught up: idle-poll the durable store on a bounded
                    // interval, but never sleep past a cancelled stream —
                    // the wait resolves as soon as the receiver drops
                    // instead of polling the store forever. A full page
                    // continues immediately.
                    tokio::select! {
                        _ = tx.closed() => return,
                        _ = tokio::time::sleep(EVENT_FOLLOW_POLL_INTERVAL) => {}
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    /// Cumulative acknowledgement of a session's events. Requires a valid
    /// control lease (only the controlling client may acknowledge); a bound
    /// beyond the committed bound is refused with nothing written. The ack
    /// is linearized against lease generation transitions: the lease gate is
    /// held from validation through the backend commit, so a concurrent
    /// release, expiry or takeover cannot invalidate the ack in between.
    async fn ack_session_events(
        &self,
        request: Request<pb::AckSessionEventsRequest>,
    ) -> Result<Response<pb::AckSessionEventsResponse>, Status> {
        let request = request.into_inner();
        let (session, token) = wire::control(request.control)?;
        let lease = self
            .leases
            .begin_ack(&session, &token)
            .await
            .map_err(wire::lease_status)?;
        let state = self
            .backend
            .ack_events(&session, request.up_to_seq)
            .await
            .map_err(wire::runtime_status)?;
        drop(lease);
        Ok(Response::new(pb::AckSessionEventsResponse {
            state: Some(wire::event_state(state)),
        }))
    }
}
