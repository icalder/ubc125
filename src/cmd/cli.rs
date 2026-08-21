use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "UBC 125 Scanner Control", long_about = None)]
pub struct Cli {
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
    /// Deterministic WebM/Opus tone generator (test fixture, hidden).
    #[command(hide = true)]
    AudioTone(super::audio_tone::AudioToneArgs),
}
