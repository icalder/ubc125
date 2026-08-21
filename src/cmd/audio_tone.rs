//! Hidden subcommand: deterministic WebM/Opus tone generator.
//!
//! Runs [`ToneSource`] — the same encoder/muxer the native capture uses —
//! to completion (or until the process is killed with `--loop`), writing
//! the emitted bytes in order: init segment, then clusters. This is the
//! ffmpeg-free fixture for the audio E2E tests: a finite file for the
//! file-source phase, an unbounded faster-than-real-time stream
//! (`--loop --out -`) for the continuous-source phase.

use std::io::{self, Write};
use std::time::Duration;

use clap::Args;

use crate::audio::source::{CaptureSource, SourceEvent};
use crate::audio::ToneSource;
use crate::audio::webm_mux::DEFAULT_BITRATE_BPS;

/// `--loop` means "until killed"; cap the tone at ~10 years so the process
/// lifetime, not the duration, decides when it ends.
const LOOP_DURATION: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

#[derive(Args)]
#[command(hide = true)]
pub struct AudioToneArgs {
    /// Output file, or `-` for stdout.
    #[arg(long, default_value = "-")]
    pub out: String,
    /// Duration in seconds (ignored with `--loop`).
    #[arg(long, default_value_t = 60.0)]
    pub duration: f64,
    /// Emit clusters forever until the process is killed.
    #[arg(long)]
    pub loop_: bool,
    /// Tone frequency in Hz.
    #[arg(long, default_value_t = 440.0)]
    pub freq: f64,
    /// Opus bitrate in bits/s.
    #[arg(long, default_value_t = DEFAULT_BITRATE_BPS)]
    pub bitrate: u32,
}

pub async fn run(args: &AudioToneArgs) -> Result<(), Box<dyn std::error::Error>> {
    let duration = if args.loop_ {
        LOOP_DURATION
    } else {
        Duration::from_secs_f64(args.duration)
    };
    let source = ToneSource::new(args.freq, duration, args.bitrate);
    let handle = source.start().await?;
    let _stop = handle.stop_handle(); // released with the process
    let events = handle.into_events();
    let out: Box<dyn Write + Send> = if args.out == "-" {
        Box::new(io::stdout())
    } else {
        Box::new(std::fs::File::create(&args.out)?)
    };
    // The tone thread emits faster than real time; pump its channel on a
    // blocking thread so `blocking_recv` stays off the runtime.
    tokio::task::spawn_blocking(move || pump_to_out(events, out))
        .await??;
    Ok(())
}

fn pump_to_out(
    mut events: tokio::sync::mpsc::Receiver<SourceEvent>,
    mut out: Box<dyn Write + Send>,
) -> io::Result<()> {
    loop {
        match events.blocking_recv() {
            Some(SourceEvent::Bytes(bytes)) => {
                out.write_all(&bytes)?;
                // Flush per event so pipe consumers (and the broadcaster's
                // channel) are fed without waiting for the process to end.
                out.flush()?;
            }
            Some(SourceEvent::End(exit)) => {
                if exit.is_failed() {
                    return Err(io::Error::other(format!("tone source failed: {exit:?}")));
                }
                break;
            }
            None => break,
        }
    }
    Ok(())
}
