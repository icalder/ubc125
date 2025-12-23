use clap::Args;

use crate::server;
use tower_http::cors::{Any, CorsLayer};

#[derive(Args)]
pub struct ServeArgs {
    #[arg(short, long, default_value_t = String::from("127.0.0.1:50051"))]
    pub server_addr: String,
}

// grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetModelInfo
// grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetFirmwareVersion

pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(ubc125_grpc::ubc125::v1::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let system_info_service = server::SystemInfoServiceImpl {};

    println!("Starting server at {}", args.server_addr);
    tonic::transport::Server::builder()
        .accept_http1(true)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(tonic_web::GrpcWebLayer::new())
        .add_service(reflection_service)
        .add_service(
            ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoServiceServer::new(
                system_info_service,
            ),
        )
        .serve(args.server_addr.parse()?)
        .await?;

    Ok(())
}
