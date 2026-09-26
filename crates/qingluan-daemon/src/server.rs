//! Daemon assembly: existing HTTP plus terminal gRPC over a private UDS.

use std::error::Error;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use qingluan_config::Config;
use qingluan_protocol::terminal::v1::terminal_service_server::TerminalServiceServer;
use qingluan_terminal::{RuntimeConfig, RuntimeError, TerminalRuntime};
use tokio::sync::watch;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::backend::TerminalBackend;
use crate::http;
use crate::lease::{LeaseManager, MAX_LEASE_TTL};
use crate::service::TerminalGrpcService;
use crate::socket::BoundUnixSocket;

pub type DaemonResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
const SERVER_DRAIN_WAIT: Duration = Duration::from_secs(5);

pub async fn run(config: Config, shutdown: impl Future<Output = ()> + Send) -> DaemonResult<()> {
    validate_config(&config)?;

    // Own the process/socket identity before touching the shared cgroup root:
    // a second daemon can never reconcile the first daemon's cgroups.
    let bound = BoundUnixSocket::bind(&config.terminal.socket_path)?;
    let (grpc_listener, _socket_guard) = bound.into_parts();
    let http_addr = format!("{}:{}", config.daemon.host, config.daemon.port);
    let http_listener = tokio::net::TcpListener::bind(&http_addr).await?;

    let runtime_config = RuntimeConfig::new(&config.terminal.cgroup_tag)
        .with_limits(config.terminal.session_limit, config.terminal.global_limit);
    let runtime = Arc::new(
        TerminalRuntime::open(&config.terminal.storage_root, runtime_config)
            .await
            .map_err(|error| match error {
                RuntimeError::Cgroup { .. } => format!(
                    "terminal runtime startup failed ({error}); the systemd user unit must provide Delegate=yes and cgroup.kill"
                ),
                other => format!("terminal runtime startup failed: {other}"),
            })?,
    );
    let backend: Arc<dyn TerminalBackend> = runtime.clone();
    let leases = LeaseManager::new(
        Arc::clone(&backend),
        Duration::from_secs(config.terminal.lease_ttl_seconds),
    )
    .map_err(|error| error.to_owned())?;
    let grpc = TerminalGrpcService::new(backend, leases, qingluan_core::version());
    let grpc = TerminalServiceServer::new(grpc)
        .max_decoding_message_size(config.terminal.max_message_bytes)
        .max_encoding_message_size(config.terminal.max_message_bytes);

    tracing::info!(address = %http_addr, "HTTP daemon listening");
    if matches!(config.daemon.host.as_str(), "0.0.0.0" | "::" | "") {
        for (name, ip) in lan_addresses() {
            tracing::info!(
                interface = %name,
                address = %format!("http://{ip}:{}", config.daemon.port),
                "HTTP daemon reachable on the local network"
            );
        }
    }
    tracing::info!(
        socket = %config.terminal.socket_path.display(),
        mode = "0600",
        protocol_major = crate::wire::PROTOCOL_MAJOR,
        "terminal gRPC listening"
    );

    let (shutdown_tx, _) = watch::channel(false);
    let http_shutdown = wait_for_shutdown(shutdown_tx.subscribe());
    let grpc_shutdown = wait_for_shutdown(shutdown_tx.subscribe());

    let http_app = http::router(qingluan_core::version());
    let mut http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_app)
            .with_graceful_shutdown(http_shutdown)
            .await
            .map_err(|error| error.to_string())
    });
    let mut grpc_task = tokio::spawn(async move {
        Server::builder()
            .add_service(grpc)
            .serve_with_incoming_shutdown(UnixListenerStream::new(grpc_listener), grpc_shutdown)
            .await
            .map_err(|error| error.to_string())
    });

    enum Trigger {
        Signal,
        Http(Result<Result<(), String>, tokio::task::JoinError>),
        Grpc(Result<Result<(), String>, tokio::task::JoinError>),
    }
    tokio::pin!(shutdown);
    let trigger = tokio::select! {
        _ = &mut shutdown => Trigger::Signal,
        result = &mut http_task => Trigger::Http(result),
        result = &mut grpc_task => Trigger::Grpc(result),
    };
    shutdown_tx.send_replace(true);

    let (http_result, grpc_result) = match trigger {
        Trigger::Signal => (
            join_server(&mut http_task).await,
            join_server(&mut grpc_task).await,
        ),
        Trigger::Http(result) => (
            unexpected_server_exit("HTTP", result),
            join_server(&mut grpc_task).await,
        ),
        Trigger::Grpc(result) => (
            join_server(&mut http_task).await,
            unexpected_server_exit("gRPC", result),
        ),
    };

    let runtime_result = runtime.shutdown().await;
    if let Err(error) = runtime_result {
        return Err(format!("terminal runtime shutdown incomplete: {error}").into());
    }
    http_result.map_err(|error| format!("HTTP server failed: {error}"))?;
    grpc_result.map_err(|error| format!("gRPC server failed: {error}"))?;
    Ok(())
}

async fn wait_for_shutdown(mut receiver: watch::Receiver<bool>) {
    let _ = receiver.wait_for(|shutdown| *shutdown).await;
}

async fn join_server(task: &mut tokio::task::JoinHandle<Result<(), String>>) -> Result<(), String> {
    match tokio::time::timeout(SERVER_DRAIN_WAIT, &mut *task).await {
        Ok(result) => flatten_join(result),
        Err(_) => {
            task.abort();
            Err("graceful drain exceeded 5 seconds".into())
        }
    }
}

fn unexpected_server_exit(
    name: &str,
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), String> {
    flatten_join(result).and_then(|()| Err(format!("{name} server stopped unexpectedly")))
}

fn flatten_join(result: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    match result {
        Ok(result) => result,
        Err(error) => Err(format!("server task failed: {error}")),
    }
}

/// Global IPv4 addresses of every non-loopback interface, so the startup
/// log covers every network the host is reachable on (physical LAN,
/// Tailscale, VPNs, bridges) instead of only the default-route one.
fn lan_addresses() -> Vec<(String, std::net::Ipv4Addr)> {
    let mut addrs: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| {
            let if_addrs::IfAddr::V4(v4) = iface.addr else {
                return None;
            };
            // Skip 169.254.* autoconfiguration addresses; they are not
            // usable as client targets.
            if v4.ip.is_link_local() {
                return None;
            }
            Some((iface.name, v4.ip))
        })
        .collect();
    addrs.sort();
    addrs.dedup();
    addrs
}

fn validate_config(config: &Config) -> DaemonResult<()> {
    if config.terminal.session_limit == 0 || config.terminal.global_limit == 0 {
        return Err("terminal quota limits must be non-zero".into());
    }
    if config.terminal.session_limit > config.terminal.global_limit {
        return Err("terminal session_limit must not exceed global_limit".into());
    }
    if config.terminal.lease_ttl_seconds == 0 {
        return Err("terminal lease_ttl_seconds must be non-zero".into());
    }
    if Duration::from_secs(config.terminal.lease_ttl_seconds) > MAX_LEASE_TTL {
        return Err("terminal lease_ttl_seconds must not exceed 86400".into());
    }
    if !config.terminal.storage_root.is_absolute() {
        return Err("terminal storage_root must be absolute".into());
    }
    if config.terminal.cgroup_tag.is_empty()
        || config.terminal.cgroup_tag.len() > 64
        || !config
            .terminal
            .cgroup_tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "terminal cgroup_tag must be 1..=64 ASCII letters, digits, '.', '_' or '-'".into(),
        );
    }
    if !(512 * 1024..=16 * 1024 * 1024).contains(&config.terminal.max_message_bytes) {
        return Err("terminal max_message_bytes must be between 512 KiB and 16 MiB".into());
    }
    Ok(())
}
