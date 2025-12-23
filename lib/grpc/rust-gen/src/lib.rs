pub mod ubc125 {
    pub mod v1 {
        pub const FILE_DESCRIPTOR_SET: &[u8] =
            tonic::include_file_descriptor_set!("ubc125_descriptor");

        include!("proto/ubc125.v1.rs");
        //include!(concat!(env!("OUT_DIR"), "/ubc125.v1.rs"));
    }
}
