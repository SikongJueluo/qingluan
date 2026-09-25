//! Tonic `TerminalService` adapter.

use std::sync::Arc;

use qingluan_core::terminal as domain;
use qingluan_protocol::terminal::v1 as pb;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalService;
use qingluan_terminal::RuntimeError;
use tonic::{Request, Response, Status};

use crate::backend::TerminalBackend;
use crate::lease::LeaseManager;
use crate::wire;

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
}
