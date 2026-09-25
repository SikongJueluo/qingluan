use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../..");
    let proto_dir = root.join("proto");
    let third_party = root.join("third_party");
    let terminal = proto_dir.join("qingluan/terminal/v1/terminal.proto");
    let error = proto_dir.join("qingluan/terminal/v1/error.proto");
    let status = third_party.join("google/rpc/status.proto");
    let any = third_party.join("google/protobuf/any.proto");

    for path in [&terminal, &error, &status, &any] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&[terminal, error, status], &[proto_dir, third_party])?;

    Ok(())
}
