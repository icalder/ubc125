use std::sync::Arc;

use crate::audio::{
    AudioBroadcaster, CaptureSource, CommandSource, DEFAULT_AUDIO_DEVICE, FfmpegSource,
};
use crate::scanner::ScannerClient;
use crate::server;
use crate::web;
use clap::Args;
use tonic_web::GrpcWebLayer;
use tower::Layer;
use tower_http::cors::{Any, CorsLayer};
use ubc125_grpc::ubc125::v1::audio_service_server::AudioServiceServer;
use ubc125_grpc::ubc125::v1::scanner_control_service_server::ScannerControlServiceServer;
use ubc125_grpc::ubc125::v1::system_info_service_server::SystemInfoServiceServer;

/// Max gRPC message size for the audio service (init + 64 KiB clusters).
const AUDIO_MAX_MESSAGE_SIZE: usize = 64 * 1024;

#[derive(Args)]
pub struct ServeArgs {
    #[arg(short, long, default_value_t = String::from("127.0.0.1:50051"))]
    pub server_addr: String,
    // No short flag: `-d` is reserved for the global debug flag (an
    // automatic `-d` here would shadow it and break "Use -d for debug
    // logging").
    #[arg(long, default_value_t = String::from("/dev/ttyACM0"))]
    pub device: String,
    /// ALSA capture device for the audio pipeline (default is the Pi's USB
    /// mic, card 2).
    #[arg(long, env = "UBC125_AUDIO_DEVICE", default_value = DEFAULT_AUDIO_DEVICE)]
    pub audio_device: String,
    /// Test hook (not user config): run this shell command line instead of
    /// the default ffmpeg capture; its stdout is the WebM byte stream.
    #[arg(long, env = "UBC125_AUDIO_CMD", hide = true)]
    pub audio_cmd: Option<String>,
}

/// Pick the capture source: the D1 test hook command if given, else the
/// production ALSA → Opus → WebM ffmpeg pipeline.
fn audio_source(args: &ServeArgs) -> Arc<dyn CaptureSource> {
    match &args.audio_cmd {
        Some(cmd) => Arc::new(CommandSource::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            cmd.clone(),
        ])),
        None => Arc::new(FfmpegSource::new(&args.audio_device)),
    }
}

pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(ubc125_grpc::ubc125::v1::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let client = ScannerClient::new(&args.device)?;
    let scanner_server = server::ScannerServer::new(client);
    let audio_broadcaster = Arc::new(AudioBroadcaster::new(audio_source(args)));
    let audio_server = server::AudioServer::new(audio_broadcaster.clone());

    // Each gRPC service is wrapped in the grpc-web codec individually
    // (GrpcWebLayer as a blanket server layer would 400 every
    // non-grpc-web HTTP/1.1 request, including the browser's static-file
    // GETs). Native gRPC (h2, application/grpc) passes through untouched.
    let routes = axum::Router::new()
        .route_service(
            "/ubc125.v1.ScannerControlService/{*rest}",
            GrpcWebLayer::new().layer(ScannerControlServiceServer::new(scanner_server.clone())),
        )
        .route_service(
            "/ubc125.v1.SystemInfoService/{*rest}",
            GrpcWebLayer::new().layer(SystemInfoServiceServer::new(scanner_server)),
        )
        .route_service(
            "/ubc125.v1.AudioService/{*rest}",
            GrpcWebLayer::new().layer(
                AudioServiceServer::new(audio_server)
                    .max_encoding_message_size(AUDIO_MAX_MESSAGE_SIZE)
                    .max_decoding_message_size(AUDIO_MAX_MESSAGE_SIZE),
            ),
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

    // ctrl-c: stop accepting, drain in-flight streams, then kill the capture
    // so ffmpeg cannot keep the ALSA device across a restart.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    server
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            shutdown,
        )
        .await?;
    audio_broadcaster.shutdown().await;

    Ok(())
}
