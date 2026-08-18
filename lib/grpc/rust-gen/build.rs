use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Re-run the build script when the regen flag changes or the protos
    // change (the descriptor set must track the .proto files, otherwise
    // reflection serves a stale service list).
    println!("cargo:rerun-if-env-changed=UBC125_REGEN");
    println!("cargo:rerun-if-changed=../proto/ubc125/v1/services.proto");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // The generated prost/tonic code is committed under src/proto so the
    // package builds without a protobuf toolchain. build.rs normally only
    // produces the file descriptor set (used for gRPC reflection).
    //
    // After changing the .proto files, regenerate the committed code with:
    //   UBC125_REGEN=1 cargo build -p ubc125-grpc
    // and commit the updated src/proto files.
    let mut builder = tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("ubc125_descriptor.bin"))
        .compile_well_known_types(true);
    if env::var("UBC125_REGEN").is_ok() {
        builder = builder.out_dir("src/proto");
    }

    builder.compile_protos(&["../proto/ubc125/v1/services.proto"], &["../proto/"])?;
    Ok(())
}
