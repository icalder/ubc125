pub mod ubc125 {
    pub mod v1 {
        pub const FILE_DESCRIPTOR_SET: &[u8] =
            tonic::include_file_descriptor_set!("ubc125_descriptor");

        // Generated code is committed (regenerate with UBC125_REGEN=1,
        // see build.rs).
        include!("proto/ubc125.v1.rs");
    }
}
