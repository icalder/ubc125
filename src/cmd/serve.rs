use std::sync::Arc;

use crate::audio::{
    AlsaOpusSource, AudioBroadcaster, CaptureSource, CommandSource, SquelchGate,
    SquelchGateConfig, DEFAULT_AUDIO_DEVICE,
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
    /// Scanner serial device (default: auto-detect the UBC125 by its USB
    /// id, 1965:0018).
    #[arg(long, env = "UBC125_DEVICE")]
    pub device: Option<String>,
    /// ALSA capture device for the audio pipeline (default is the Pi's USB
    /// mic, card 2).
    #[arg(long, env = "UBC125_AUDIO_DEVICE", default_value = DEFAULT_AUDIO_DEVICE)]
    pub audio_device: String,
    /// Test hook (not user config): run this shell command line instead of
    /// the native capture; its stdout is the WebM byte stream.
    #[arg(long, env = "UBC125_AUDIO_CMD", hide = true)]
    pub audio_cmd: Option<String>,
    /// Enable the experimental squelch de-clicker. This uses the current
    /// interim 1000 ms floor-anchored close fade and a 20 ms onset fade.
    /// The test audio-command hook is already WebM and is not filtered.
    #[arg(long, env = "UBC125_DECLICK", default_value_t = false)]
    pub declick: bool,
}

/// Pick the capture source: the test hook command if given, else the
/// production ALSA → Opus → WebM native pipeline.
fn audio_source(args: &ServeArgs) -> Arc<dyn CaptureSource> {
    match &args.audio_cmd {
        Some(cmd) => Arc::new(CommandSource::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            cmd.clone(),
        ])),
        None => {
            let filter = if args.declick {
                let config = SquelchGateConfig {
                    fade_out_ms: 1_000,
                    ..SquelchGateConfig::default()
                };
                Some(Arc::new(SquelchGate::new(config)))
            } else {
                None
            };
            let source = AlsaOpusSource::new(&args.audio_device);
            match filter {
                Some(filter) => Arc::new(source.with_filter(filter)),
                None => Arc::new(source),
            }
        }
    }
}

pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(ubc125_grpc::ubc125::v1::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let device = crate::detect::resolve_device(args.device.as_deref())?;
    let client = ScannerClient::new(&device)?;
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
    eprintln!("Scanner device:  {device}");
    eprintln!("Listening on:    {}", args.server_addr);
    eprintln!("Web UI:          http://{}/", args.server_addr);
    eprintln!("gRPC (grpcurl):  grpc://{}", args.server_addr);
    eprintln!("Use -d for debug logging.");

    // ctrl-c: stop accepting, drain in-flight streams, then stop the capture
    // so it cannot keep the ALSA device across a restart.
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
