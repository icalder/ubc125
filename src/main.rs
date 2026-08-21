mod audio;
mod cmd;
mod constants;
mod modes;
mod scanner;
mod server;
mod status;
mod types;
mod web;

use clap::Parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cmd::cli::Cli::parse();

    // Initialize tracing based on the debug flag:
    //   -d  -> DEBUG, -dd -> TRACE, default -> WARN
    // `RUST_LOG` overrides the flag so a single module can be debugged
    // without the global spam (e.g. RUST_LOG=ubc125=debug keeps the h2
    // frame trace at warn).
    let default = match cli.debug {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| default.into());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&filter))
        .with_ansi(false)
        .init();

    match &cli.command {
        cmd::cli::Commands::Serve(args) => cmd::serve::run(args).await?,
        cmd::cli::Commands::Console(args) => cmd::console::run(args)?,
        cmd::cli::Commands::AudioTone(args) => cmd::audio_tone::run(args).await?,
    }
    Ok(())
}
