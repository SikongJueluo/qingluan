//! Test-only endpoint for `just terminal-grpc-interop`.
//!
//! It serves the production `TerminalGrpcService` and socket lifecycle over a
//! small fake runtime. The fake supplies deterministic bigint/read/delay
//! cases without requiring cgroup delegation; it is not installed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use qingluan_core::terminal as domain;
use qingluan_daemon::backend::TerminalBackend;
use qingluan_daemon::lease::LeaseManager;
use qingluan_daemon::service::TerminalGrpcService;
use qingluan_daemon::socket::BoundUnixSocket;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalServiceServer;
use qingluan_terminal::{RuntimeError, SendError};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

struct FixtureBackend {
    generation: AtomicU64,
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
        _session: &domain::SessionRef,
    ) -> Result<domain::SessionEventState, RuntimeError> {
        Ok(domain::SessionEventState::new(0, 0, u64::MAX).unwrap())
    }

    async fn start(
        &self,
        _session: &domain::SessionRef,
        _generation: domain::ControlGeneration,
        _spec: domain::StartSpec,
    ) -> Result<domain::TerminalRef, RuntimeError> {
        unreachable!("not used by the interop fixture")
    }

    async fn send(
        &self,
        _terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
        data: Vec<u8>,
    ) -> Result<domain::SendReceipt, SendError> {
        if data == b"delay" {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(domain::SendReceipt::new(data.len() as u64))
    }

    async fn stop_with_generation(
        &self,
        _terminal: &domain::TerminalRef,
        _generation: domain::ControlGeneration,
    ) -> Result<domain::TerminalSnapshot, RuntimeError> {
        unreachable!("not used by the interop fixture")
    }

    async fn log_identity(
        &self,
        terminal: &domain::TerminalRef,
    ) -> Result<domain::LogIdentity, RuntimeError> {
        Ok(domain::LogIdentity {
            terminal: terminal.clone(),
            log_epoch: domain::LogEpoch::new("fixture"),
        })
    }

    async fn read(
        &self,
        request: &domain::ReadRequest,
    ) -> Result<domain::ReadResult, RuntimeError> {
        let position = domain::HistoryPosition::new(u64::MAX, 0).unwrap();
        let cursor = domain::ReadCursor::new(request.log().clone(), position, u64::MAX).unwrap();
        let page = domain::ReadPage::new(Vec::new(), None, None, None, false).unwrap();
        Ok(domain::ReadResult::new(cursor, page))
    }

    async fn tail(
        &self,
        _terminal: &domain::TerminalRef,
        _limits: domain::ReadLimits,
    ) -> Result<domain::TailView, RuntimeError> {
        unreachable!("not used by the interop fixture")
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os()
        .nth(1)
        .ok_or("usage: grpc_interop_fixture SOCKET")?;
    let bound = BoundUnixSocket::bind(std::path::Path::new(&socket))?;
    let (listener, _guard) = bound.into_parts();

    let backend: Arc<dyn TerminalBackend> = Arc::new(FixtureBackend {
        generation: AtomicU64::new(1),
    });
    let leases = LeaseManager::new(backend.clone(), Duration::from_secs(30))?;
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
