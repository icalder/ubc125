mod cmd;
mod server;

use cmd::cli::Commands;
use cmd::prelude::*;
// use std::time::Duration;

// https://docs.rs/serialport/latest/serialport/
// https://github.com/serialport/serialport-rs

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cmd::cli::Cli::parse();
    println!("debug level = {}", cli.debug);

    match &cli.command {
        Commands::Serve(args) => cmd::serve::run(args).await?,
        Commands::Console(args) => cmd::console::run(args)?,
    }
    Ok(())
}
