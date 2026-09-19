use std::{
    env,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use prost::Message;
use tokio::{net::UnixListener, sync::mpsc};
use tokio_stream::{Stream, wrappers::UnixListenerStream};
use tonic::{Code, Request, Response, Status, transport::Server};

mod probe {
    tonic::include_proto!("qingluan.terminal.probe.v1");
}

mod google_rpc {
    tonic::include_proto!("google.rpc");
}

use probe::{
    EchoRequest, EchoResponse, FailRequest, FailResponse, FailureScenario, GetServerInfoRequest,
    GetServerInfoResponse, PartialWriteDetails, ProbeErrorDetail, ProbeErrorReason, StreamItem,
    StreamRequest,
    probe_error_detail::Payload,
    probe_service_server::{ProbeService, ProbeServiceServer},
};

const DETAIL_TYPE_URL: &str = "type.googleapis.com/qingluan.terminal.probe.v1.ProbeErrorDetail";

#[derive(Clone)]
struct ProbeServer {
    cancellation_marker: PathBuf,
}

#[tonic::async_trait]
impl ProbeService for ProbeServer {
    async fn get_server_info(
        &self,
        _request: Request<GetServerInfoRequest>,
    ) -> Result<Response<GetServerInfoResponse>, Status> {
        Ok(Response::new(GetServerInfoResponse {
            protocol_major: 1,
            // The last entry is deliberately unknown to the current TS
            // client: same v1 major version, server one capability ahead.
            capabilities: vec![
                "echo".into(),
                "stream".into(),
                "rich-error".into(),
                "opaque-future-capability".into(),
            ],
        }))
    }

    async fn echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoResponse>, Status> {
        let request = request.into_inner();
        if request.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(request.delay_ms.into())).await;
        }
        Ok(Response::new(EchoResponse {
            payload: request.payload,
            sequence: request.sequence,
            optional_count: request.optional_count,
            mode: request.mode,
        }))
    }

    type StreamStream = Pin<Box<dyn Stream<Item = Result<StreamItem, Status>> + Send + 'static>>;

    async fn stream(
        &self,
        request: Request<StreamRequest>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let request = request.into_inner();
        let (sender, receiver) = mpsc::channel(1);
        let marker = self.cancellation_marker.clone();

        tokio::spawn(async move {
            for offset in 1..=u64::from(request.count) {
                if request.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(request.delay_ms.into())).await;
                }
                let item = StreamItem {
                    sequence: request.after_sequence + offset,
                };
                if sender.send(Ok(item)).await.is_err() {
                    let _ = tokio::fs::write(marker, b"receiver-dropped\n").await;
                    return;
                }
            }
        });

        Ok(Response::new(
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)) as Self::StreamStream,
        ))
    }

    async fn fail(&self, request: Request<FailRequest>) -> Result<Response<FailResponse>, Status> {
        let scenario = FailureScenario::try_from(request.into_inner().scenario)
            .unwrap_or(FailureScenario::Unspecified);

        let status = match scenario {
            FailureScenario::ValidZero => rich_partial_write(Some(0), Code::Cancelled, 1),
            FailureScenario::ValidLarge => {
                rich_partial_write(Some(9_007_199_254_740_993), Code::Cancelled, 1)
            }
            FailureScenario::MissingPayload => {
                let detail = ProbeErrorDetail {
                    reason: ProbeErrorReason::PartialWrite.into(),
                    payload: None,
                };
                rich_status(Code::Cancelled, 1, vec![pack_probe_detail(detail)])
            }
            FailureScenario::UnknownAny => rich_status(
                Code::Cancelled,
                1,
                vec![prost_types::Any {
                    type_url: "type.googleapis.com/example.UnknownDetail".into(),
                    value: vec![1, 2, 3],
                }],
            ),
            FailureScenario::MalformedStatus => Status::with_details(
                Code::Cancelled,
                "malformed rich status",
                Bytes::from_static(&[0xff]),
            ),
            FailureScenario::StatusMismatch => rich_status(
                Code::FailedPrecondition,
                3,
                vec![pack_probe_detail(partial_write_detail(Some(7)))],
            ),
            FailureScenario::UnknownReason => {
                let detail = ProbeErrorDetail {
                    reason: 99,
                    payload: Some(Payload::PartialWrite(PartialWriteDetails {
                        written_bytes: Some(7),
                    })),
                };
                rich_status(Code::Cancelled, 1, vec![pack_probe_detail(detail)])
            }
            FailureScenario::DuplicateDetail => rich_status(
                Code::Cancelled,
                1,
                vec![
                    pack_probe_detail(partial_write_detail(Some(7))),
                    pack_probe_detail(partial_write_detail(Some(8))),
                ],
            ),
            FailureScenario::MissingWrittenBytes => rich_status(
                Code::Cancelled,
                1,
                vec![pack_probe_detail(partial_write_detail(None))],
            ),
            FailureScenario::MalformedDetail => {
                let any = prost_types::Any {
                    type_url: DETAIL_TYPE_URL.to_owned(),
                    // Not a valid ProbeErrorDetail encoding: varint never
                    // terminates, so decoding must fail.
                    value: vec![0xff; 16],
                };
                rich_status(Code::Cancelled, 1, vec![any])
            }
            FailureScenario::ValidPlusUnknownAny => rich_status(
                Code::Cancelled,
                1,
                vec![
                    pack_probe_detail(partial_write_detail(Some(42))),
                    prost_types::Any {
                        type_url: "type.googleapis.com/example.UnknownDetail".into(),
                        value: vec![1, 2, 3],
                    },
                ],
            ),
            FailureScenario::OversizedStatus => {
                // Encoded google.rpc.Status deliberately larger than the
                // client's probe-local MAX_STATUS_DETAILS_BYTES guard, but
                // small enough that the gRPC transport still delivers it.
                let status = google_rpc::Status {
                    code: 1,
                    message: "x".repeat(6 * 1024),
                    details: vec![pack_probe_detail(partial_write_detail(Some(5)))],
                };
                Status::with_details(
                    Code::Cancelled,
                    "oversized probe failure",
                    status.encode_to_vec().into(),
                )
            }
            FailureScenario::Plain | FailureScenario::Unspecified => {
                Status::failed_precondition("plain probe failure")
            }
        };

        Err(status)
    }
}

fn partial_write_detail(written_bytes: Option<u64>) -> ProbeErrorDetail {
    ProbeErrorDetail {
        reason: ProbeErrorReason::PartialWrite.into(),
        payload: Some(Payload::PartialWrite(PartialWriteDetails { written_bytes })),
    }
}

fn rich_partial_write(written_bytes: Option<u64>, outer: Code, inner_code: i32) -> Status {
    rich_status(
        outer,
        inner_code,
        vec![pack_probe_detail(partial_write_detail(written_bytes))],
    )
}

fn pack_probe_detail(detail: ProbeErrorDetail) -> prost_types::Any {
    prost_types::Any {
        type_url: DETAIL_TYPE_URL.to_owned(),
        value: detail.encode_to_vec(),
    }
}

fn rich_status(outer: Code, inner_code: i32, details: Vec<prost_types::Any>) -> Status {
    let status = google_rpc::Status {
        code: inner_code,
        message: "probe failure".into(),
        details,
    };
    Status::with_details(outer, "probe failure", status.encode_to_vec().into())
}

async fn shutdown_signal() {
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        signal.recv().await;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate => {},
    }
}

/// Make the socket path safe to bind on:
/// - absent: fine, nothing to do;
/// - regular file (or any non-socket entry): reject and preserve it;
/// - socket that still accepts connections: reject, another server owns it;
/// - socket that refuses connections: stale leftover of a dead server,
///   remove it and let the caller bind.
fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::other(format!(
            "refusing to replace non-socket path: {}",
            path.display()
        )));
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::other(format!(
            "refusing to unlink active socket: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
        }
        // Removed by someone else between the lstat and the connect attempt.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = PathBuf::from(env::args().nth(1).ok_or("missing socket path")?);
    let cancellation_marker = PathBuf::from(
        env::var_os("PROBE_CANCELLATION_MARKER").ok_or("missing cancellation marker")?,
    );

    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let mode = std::fs::metadata(&socket)?.permissions().mode() & 0o777;

    println!("READY socket_mode={mode:o}");

    Server::builder()
        .add_service(ProbeServiceServer::new(ProbeServer {
            cancellation_marker,
        }))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await?;

    remove_stale_socket(&socket)?;
    Ok(())
}
