use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../..");
    let proto = root.join("proto/probe.proto");
    let status = root.join("third_party/google/rpc/status.proto");
    let any = root.join("third_party/google/protobuf/any.proto");
    let proto_dir = root.join("proto");
    let third_party = root.join("third_party");

    println!("cargo:rerun-if-changed={}", proto.display());
    println!("cargo:rerun-if-changed={}", status.display());
    println!("cargo:rerun-if-changed={}", any.display());

    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .compile_protos(&[proto, status], &[proto_dir, third_party])?;

    Ok(())
}
