// https://docs.rs/tonic-build/latest/tonic_build/

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    //let folder_path = Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../proto");

    tonic_prost_build::configure()
        .out_dir("src/proto")
        .file_descriptor_set_path(out_dir.join("ubc125_descriptor.bin"))
        .compile_well_known_types(true)
        .compile_protos(&["../proto/ubc125/v1/services.proto"], &["../proto/"])?;
    // .compile_protos(
    //     &["../proto/ubc125/v1/services.proto"],
    //     &[folder_path.into_os_string().into_string().unwrap().as_str()],
    // )?;
    //tonic_prost_build::compile_protos("../proto/ubc125/v1/services.proto")?;
    Ok(())
}
