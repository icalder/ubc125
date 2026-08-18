mod audio;
mod cmd;
mod constants;
mod modes;
mod scanner;
mod server;
mod types;
mod web;

use clap::Parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cmd::cli::Cli::parse();

    // Initialize tracing based on the debug flag:
    //   -d  -> DEBUG, -dd -> TRACE, default -> WARN
    let level = match cli.debug {
        0 => tracing::Level::WARN,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .init();

    match &cli.command {
        cmd::cli::Commands::Serve(args) => cmd::serve::run(args).await?,
        cmd::cli::Commands::Console(args) => cmd::console::run(args)?,
    }
    Ok(())
}
