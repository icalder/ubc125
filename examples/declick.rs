//! Offline de-clicker harness: WAV in, corrected WAV + event files out.
//!
//! Thin entry point over the ported rig's CLI (`crate::audio::clickfilter::cli`):
//! the same filter production runs behind `serve --declick`, drivable on a
//! recording for offline verification and for the Pi CPU measurement
//! `../ubc125-ml/docs/deployment.md` requires (`--benchmark`, against the
//! 20 ms-of-CPU-per-frame budget).
//!
//! ```sh
//! cargo run --release --example declick -- --file test-audio/raw60.wav --benchmark
//! ```

use std::io::Write;
use std::process::ExitCode;

use ubc125::audio::clickfilter::cli::{Options, Usage, run};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let options = match Options::parse(&argv) {
        Ok(options) => options,
        Err(Usage::Help) => {
            print!("{}", Options::usage_text());
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("declick: {err}");
            eprintln!("run with --help for the flag list");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = std::io::stdout();
    match run(&options, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = stdout.flush();
            eprintln!("declick: {err}");
            ExitCode::FAILURE
        }
    }
}
