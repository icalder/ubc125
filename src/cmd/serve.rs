use crate::scanner::ScannerClient;
use crate::server;
use crate::web;
use clap::Args;
use tonic_web::GrpcWebLayer;
use tower::Layer;
use tower_http::cors::{Any, CorsLayer};
use ubc125_grpc::ubc125::v1::scanner_control_service_server::ScannerControlServiceServer;
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoServiceServer;

#[derive(Args)]
pub struct ServeArgs {
    #[arg(short, long, default_value_t = String::from("127.0.0.1:50051"))]
    pub server_addr: String,
    #[arg(short, long, default_value_t = String::from("/dev/ttyACM0"))]
    pub device: String,
}

pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(ubc125_grpc::ubc125::v1::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let client = ScannerClient::new(&args.device)?;
    let scanner_server = server::ScannerServer::new(client);

    // Each gRPC service is wrapped in the grpc-web codec individually
    // (GrpcWebLayer as a blanket server layer would 400 every
    // non-grpc-web HTTP/1.1 request, including the browser's static-file
    // GETs). Native gRPC (h2, application/grpc) passes through untouched.
    let routes = axum::Router::new()
        .route_service(
            "/ubc125.v1.ScannerControlService/{*rest}",
            GrpcWebLayer::new().layer(ScannerControlServiceServer::new(
                scanner_server.clone(),
            )),
        )
        .route_service(
            "/ubc125.v1.SystemInfoService/{*rest}",
            GrpcWebLayer::new().layer(SystemInfoServiceServer::new(scanner_server)),
        )
        .route_service(
            "/grpc.reflection.v1.ServerReflection/{*rest}",
            GrpcWebLayer::new().layer(reflection_service),
        )
        // Everything that is not a gRPC service path is the web UI.
        .fallback_service(web::router());
    let server = tonic::transport::Server::builder()
        .accept_http1(true)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .add_routes(routes.into());

    // Bind first so the banner only prints once the port is actually ours.
    let listener = tokio::net::TcpListener::bind(&args.server_addr).await?;
    eprintln!("Scanner device:  {}", args.device);
    eprintln!("Listening on:    {}", args.server_addr);
    eprintln!("Web UI:          http://{}/", args.server_addr);
    eprintln!("gRPC (grpcurl):  grpc://{}", args.server_addr);
    eprintln!("Use -d for debug logging.");

    server
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;

    Ok(())
}
