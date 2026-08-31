use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::constants::{ClickClass, Policy};
use crate::audio::{
    AlsaOpusSource, AudioBroadcaster, CaptureSource, CommandSource, InPlaceDeClick,
    DEFAULT_AUDIO_DEVICE,
};
use crate::audio::stats::{AudioStats, SharedAudioStats};
use crate::audio::webm_mux::DEFAULT_CLUSTER_TIME_MS;
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
/// B5: default per-subscriber broadcast capacity in chunks — the broadcaster's
/// own default (the old 64 held up to ~13 s of stale audio), named here for
/// the flag's default value.
const DEFAULT_AUDIO_SUBSCRIBER_QUEUE: usize = crate::audio::DEFAULT_SUBSCRIBER_QUEUE;
/// B10: the stats reporter's period.
const STATS_REPORT_INTERVAL: Duration = Duration::from_secs(5);

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
    /// Enable the de-click filter: the plateau-trigger classifier ported from
    /// the ML prototype (T3 config of record — `interp` base, `long` clicks
    /// get a `descend` fill with a 150 ms recovery tail). Adds a fixed
    /// 20.5 ms (984-sample) output delay; the first chunk of each capture
    /// generation is silence. The test audio-command hook is already WebM
    /// and is not filtered.
    #[arg(long, env = "UBC125_DECLICK", default_value_t = false)]
    pub declick: bool,
    /// B1: WebM cluster duration in ms (a multiple of the 20 ms Opus frame;
    /// smaller clusters reach the browser sooner — the default 60 ms is
    /// three frames, ~17 chunks/s vs ~5/s at the old 200 ms).
    #[arg(long, env = "UBC125_AUDIO_CLUSTER_MS", default_value_t = DEFAULT_CLUSTER_TIME_MS, value_parser = clap::builder::RangedU64ValueParser::<u64>::new().range(20..=1000))]
    pub audio_cluster_ms: u64,
    /// B5: max chunks one subscriber may buffer ahead of the pump before the
    /// oldest are dropped (drop-oldest, never stall). 8 × 60 ms ≈ 480 ms. The
    /// B10 `audio stats` line (every 5 s) counts what was dropped; it needs -d
    /// or RUST_LOG=info to be visible.
    #[arg(long, env = "UBC125_AUDIO_SUBSCRIBER_QUEUE", default_value_t = DEFAULT_AUDIO_SUBSCRIBER_QUEUE as u64, value_parser = clap::builder::RangedU64ValueParser::<u64>::new().range(1..=256))]
    pub audio_subscriber_queue: u64,
}

/// Pick the capture source: the test hook command if given, else the
/// production ALSA → Opus → WebM native pipeline. The ALSA source records
/// its xrun / channel-stall counters into the shared `stats` (B10); the
/// command hook has no device of its own and does not record.
fn audio_source(args: &ServeArgs, stats: SharedAudioStats) -> Arc<dyn CaptureSource> {
    match &args.audio_cmd {
        Some(cmd) => Arc::new(CommandSource::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            cmd.clone(),
        ])),
        None => {
            let filter = if args.declick {
                // T3 config of record (../ubc125-ml/docs/prototype.md, arm 3):
                // interp base, long -> descend with a 150 ms recovery tail.
                let config = Config::builder()
                    .policy(Policy::Interp)
                    .policy_override(ClickClass::Long, Policy::Descend)
                    .tail_ms(ClickClass::Long, 150.0)
                    .build();
                Some(Arc::new(InPlaceDeClick::new(&config)))
            } else {
                None
            };
            let source = AlsaOpusSource::new(&args.audio_device)
                .with_cluster_time(args.audio_cluster_ms)
                .with_stats(stats);
            match filter {
                Some(filter) => Arc::new(source.with_filter(filter)),
                None => Arc::new(source),
            }
        }
    }
}

/// B10: log a window of pipeline counters every 5 s while anything moves
/// (chunk rate + cluster duration from the pump; xruns and channel stalls
/// from capture; per-subscriber `Lagged` drops from the fan-out). This is
/// the "measure before/after" harness for the BUFFERING-FIXES work: a
/// healthy steady state is a constant chunk rate with no xruns, no stalls,
/// and no lagging subscribers; a throttled client shows up in the lag list
/// without any xruns (drop-oldest at the fan-out, not the device).
fn spawn_stats_reporter(stats: SharedAudioStats) {
    tokio::spawn(async move {
        let mut last = stats.snapshot();
        let mut interval = tokio::time::interval(STATS_REPORT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let now = stats.snapshot();
            if !now.moved(&last) {
                continue;
            }
            let chunks = now.chunks_produced.saturating_sub(last.chunks_produced);
            let cluster_ms = now.cluster_ms_sum.saturating_sub(last.cluster_ms_sum);
            // Mean cluster duration over the window: the chunk rate times it
            // is the audio rate (17 × 60 ms ≈ 1.02 s of audio per second).
            let mean_ms = cluster_ms.checked_div(chunks).unwrap_or(0);
            let xruns = now.xruns.saturating_sub(last.xruns);
            let stalls = now.channel_stalls.saturating_sub(last.channel_stalls);
            let lagging = if now.subscribers.is_empty() {
                String::new()
            } else {
                now.subscribers
                    .iter()
                    .map(|(id, s)| format!("#{id}: {}/{}", s.lag_events, s.lag_drops))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            info!(
                chunks_in_window = chunks,
                cluster_ms_in_window = cluster_ms,
                cluster_ms_mean = mean_ms,
                cluster_ms_min = ?now.cluster_ms_min,
                xruns_in_window = xruns,
                xruns_total = now.xruns,
                channel_stalls_in_window = stalls,
                channel_stalls_total = now.channel_stalls,
                lagging_subscribers = %lagging,
                "audio stats"
            );
            last = now;
        }
    });
}

pub async fn run(args: &ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(ubc125_grpc::ubc125::v1::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let device = crate::detect::resolve_device(args.device.as_deref())?;
    let client = ScannerClient::new(&device)?;
    let scanner_server = server::ScannerServer::new(client);
    // One stats instance shared by the capture source (xruns, channel
    // stalls), the pump (chunk rate) and the listeners (per-subscriber
    // drops); the reporter reads it every 5 s (B10).
    let audio_stats = Arc::new(AudioStats::new());
    let audio_broadcaster = Arc::new(
        AudioBroadcaster::with_stats(audio_source(args, audio_stats.clone()), audio_stats)
            .with_subscriber_queue(args.audio_subscriber_queue as usize),
    );
    spawn_stats_reporter(audio_broadcaster.stats());
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
    eprintln!(
        "Audio pipeline:  cluster {} ms, subscriber queue {} chunks",
        args.audio_cluster_ms, args.audio_subscriber_queue
    );
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
