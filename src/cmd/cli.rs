use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about="UBC 125 gRPC Gateway", long_about = None)]
pub struct Cli {
    /// Sets a custom config file
    // #[arg(short, long, value_name = "FILE")]
    // pub config: Option<PathBuf>,

    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub debug: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Serve(super::serve::ServeArgs),
    Console(super::console::ConsoleArgs),
}
