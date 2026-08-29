//! Command line: WAV in, corrected WAV + event files out.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/cli.py` minus the corpus sweep: the rig reads
//! the capture registry, this port takes explicit WAV paths, because the byte
//! comparison has to name its inputs anyway. Flags and their defaults match the
//! rig's, and `config_tag` reproduces its artifact names so a ported run and a
//! reference run land on the same tag.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::audio::clickfilter::EventRecord;
use crate::audio::clickfilter::checks::{
    add_context_stats, class_lines, class_profile, pass_through_check, summary_line,
};
use crate::audio::clickfilter::config::{Config, ConfigBuilder, ConfigError};
use crate::audio::clickfilter::constants::{ClickClass, Polarity, Policy, RATE};
use crate::audio::clickfilter::filter::{ClickFilter, run_filter};
use crate::audio::clickfilter::format::rounded;
use crate::audio::clickfilter::wav::{read_wav, write_reference_wav};

/// The fallback data root; `UBC125_ML_DATA` overrides it (never hard-code it in
/// a manifest or an annotation — `../ubc125-ml/AGENTS.md`, "Data and paths").
const DATA_FALLBACK: &str = "/home/itcalde/rust/ubc125/test-audio";

#[derive(Debug)]
pub enum Usage {
    /// `--help` was asked for: print the usage text and stop.
    Help,
    MissingValue(String),
    UnknownFlag(String),
    BadValue {
        flag: String,
        value: String,
    },
    NoInput,
    Config(ConfigError),
    DataRoot(String),
    Io(std::io::Error),
    Wav(crate::audio::clickfilter::wav::WavError),
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Usage::Help => f.write_str(USAGE),
            Usage::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Usage::UnknownFlag(flag) => write!(f, "unknown flag {flag}"),
            Usage::BadValue { flag, value } => write!(f, "{flag} cannot use {value:?}"),
            Usage::NoInput => write!(f, "pass --file <wav> (the rig's --all stays in Python)"),
            Usage::Config(err) => write!(f, "configuration refused: {err}"),
            Usage::DataRoot(reason) => write!(f, "{reason}"),
            Usage::Io(err) => write!(f, "{err}"),
            Usage::Wav(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Usage {}

impl From<ConfigError> for Usage {
    fn from(err: ConfigError) -> Self {
        Usage::Config(err)
    }
}

impl From<std::io::Error> for Usage {
    fn from(err: std::io::Error) -> Self {
        Usage::Io(err)
    }
}

impl From<crate::audio::clickfilter::wav::WavError> for Usage {
    fn from(err: crate::audio::clickfilter::wav::WavError) -> Self {
        Usage::Wav(err)
    }
}

/// Every parsed flag.
#[derive(Debug, Clone)]
pub struct Options {
    pub files: Vec<String>,
    pub out: PathBuf,
    pub name: Option<String>,
    pub tag_suffix: String,
    pub no_write: bool,
    pub print_config: bool,
    /// Time each `process_frame` call and report the distribution (T8).
    pub benchmark: bool,
    builder: ConfigBuilder,
}

impl Options {
    pub fn config(&self) -> Result<Config, ConfigError> {
        self.builder.clone().try_build()
    }

    pub fn usage_text() -> &'static str {
        USAGE
    }
}

const USAGE: &str = "\
ubc125-ml — the Rust port of the de-click prototype rig (../ubc125-ml/docs/prototype.md)

USAGE:
    ubc125-ml [FLAGS] --file <wav> [--file <wav> ...]

FLAGS (same names and defaults as ../ubc125-ml/scripts/declick.py):
    --file <wav>          WAV to process (registry-relative unless absolute)
    --out <dir>           output directory            [artifacts/rust-declick]
    --name <stem>         artifact name stem          [derived from --file]
    --tag <suffix>        extra suffix for output names
    --no-write            measure only, write nothing
    --config-json         print the configuration line and exit
    --benchmark           time every process_frame call and report the distribution
    --policy <name>       interp | descend | mute | lf-null
    --policy-short|long|xlong|other <name>   per-class override
    --polarity <name>     negative | any
    --clip <float>        full-scale trigger level    [0.98]
    --min-run <int>       shortest run to report      [4]
    --max-plateau <int>   cap for one run             [400]
    --delay-ms <float>    output delay, raised to what the window needs
    --pre-ms <float>      guard before the onset      [2]
    --post-ms <float>     window after the run ends   [10]
    --xfade-ms <float>    raised-cosine blend edges   [2]
    --lf-cut <float>      lf-null cutoff, Hz          [180]
    --tail-short|long|xlong|other-ms <float>  recovery tail per class
    --on-classes <list>   comma-separated classes to correct  [short,long]
    --help                this text
";

impl Options {
    /// Parse argv (without the program name).
    pub fn parse(argv: &[String]) -> Result<Options, Usage> {
        let mut files: Vec<String> = Vec::new();
        let mut out = PathBuf::from("artifacts/rust-declick");
        let mut name: Option<String> = None;
        let mut tag_suffix = String::new();
        let mut no_write = false;
        let mut print_config = false;
        let mut benchmark = false;
        let mut builder = Config::builder();
        let mut tail_ms: Vec<(ClickClass, f64)> = Vec::new();
        let mut overrides: Vec<(ClickClass, Policy)> = Vec::new();
        let mut on_classes: Option<Vec<ClickClass>> = None;

        let mut index = 0;
        while index < argv.len() {
            let (flag, inline) = split_flag(&argv[index])?;
            let mut value = || -> Result<String, Usage> {
                if let Some(text) = &inline {
                    return Ok(text.clone());
                }
                index += 1;
                argv.get(index)
                    .cloned()
                    .ok_or_else(|| Usage::MissingValue(flag.to_string()))
            };
            match flag {
                "--help" | "-h" => return Err(Usage::Help),
                "--file" => files.push(value()?),
                "--out" => out = PathBuf::from(value()?),
                "--name" => name = Some(value()?),
                "--tag" => tag_suffix = value()?,
                "--no-write" => no_write = true,
                "--config-json" => print_config = true,
                "--benchmark" => benchmark = true,
                "--policy" => builder = builder.policy(policy(&value()?, flag)?),
                "--polarity" => builder = builder.polarity(polarity(&value()?, flag)?),
                "--clip" => builder = builder.clip(number(&value()?, flag)?),
                "--min-run" => builder = builder.min_run(number(&value()?, flag)?),
                "--max-plateau" => builder = builder.max_plateau(number(&value()?, flag)?),
                "--delay-ms" => builder = builder.delay_ms(number(&value()?, flag)?),
                "--pre-ms" => builder = builder.pre_ms(number(&value()?, flag)?),
                "--post-ms" => builder = builder.post_ms(number(&value()?, flag)?),
                "--xfade-ms" => builder = builder.xfade_ms(number(&value()?, flag)?),
                "--lf-cut" => builder = builder.lf_cut(number(&value()?, flag)?),
                "--policy-short" => overrides.push((ClickClass::Short, policy(&value()?, flag)?)),
                "--policy-long" => overrides.push((ClickClass::Long, policy(&value()?, flag)?)),
                "--policy-xlong" => overrides.push((ClickClass::Xlong, policy(&value()?, flag)?)),
                "--policy-other" => overrides.push((ClickClass::Other, policy(&value()?, flag)?)),
                "--tail-short-ms" => tail_ms.push((ClickClass::Short, number(&value()?, flag)?)),
                "--tail-long-ms" => tail_ms.push((ClickClass::Long, number(&value()?, flag)?)),
                "--tail-xlong-ms" => tail_ms.push((ClickClass::Xlong, number(&value()?, flag)?)),
                "--tail-other-ms" => tail_ms.push((ClickClass::Other, number(&value()?, flag)?)),
                "--on-classes" => on_classes = Some(class_list(&value()?, flag)?),
                other => return Err(Usage::UnknownFlag(other.to_string())),
            }
            index += 1;
        }
        for (class, policy) in overrides {
            builder = builder.policy_override(class, policy);
        }
        for (class, ms) in tail_ms {
            builder = builder.tail_ms(class, ms);
        }
        if let Some(classes) = on_classes {
            builder = builder.on_classes(&classes);
        }
        if files.is_empty() && !print_config {
            return Err(Usage::NoInput);
        }
        Ok(Options {
            files,
            out,
            name,
            tag_suffix,
            no_write,
            print_config,
            benchmark,
            builder,
        })
    }
}

/// Run the whole command: every input file, through one configuration.
pub fn run(options: &Options, stdout: &mut dyn Write) -> Result<(), Usage> {
    let cfg = options.config()?;
    if options.print_config {
        writeln!(stdout, "config `{}`", cfg.as_json())?;
        return Ok(());
    }
    let tag = config_tag(&cfg, &options.tag_suffix);
    let mut failures = 0;
    for file in &options.files {
        let path = resolve_input(file)?;
        let stem = options.name.clone().unwrap_or_else(|| derived_stem(file));
        let original = read_wav(&path)?;
        let started = std::time::Instant::now();
        let (corrected, mut filter) = run_filter(&cfg, &original);
        let wall = started.elapsed().as_secs_f64();
        if options.benchmark {
            let frames = frame_time_distribution(&cfg, &original);
            writeln!(stdout, "  per-frame: {frames}")?;
        }
        add_context_stats(&cfg, &original, filter.events_mut());
        let events = filter.events();
        let checks = pass_through_check(&original, &corrected, events);
        let profile = class_profile(&original, &corrected, events);
        let seconds = original.len() as f64 / RATE;
        writeln!(
            stdout,
            "{}",
            summary_line(&stem, seconds, wall, &cfg, filter.metrics(), &checks)
        )?;
        for line in class_lines(&profile) {
            writeln!(stdout, "{line}")?;
        }
        if checks.changed_outside_windows > 0 || filter.metrics().late_writes > 0 {
            failures += 1;
        }
        if !options.no_write {
            write_artifacts(&options.out, &tag, &stem, &corrected, events)?;
        }
    }
    if failures > 0 {
        return Err(Usage::DataRoot(format!(
            "{failures} input(s) broke the runtime contract (late writes or \
             changes outside a window)"
        )));
    }
    Ok(())
}

/// Time every `process_frame` call: what the deployment path costs per 20 ms of
/// audio. `../ubc125-ml/docs/deployment.md` asks for warm and cold numbers and the
/// percentiles, so the first frame is reported apart from the rest.
fn frame_time_distribution(cfg: &Config, samples: &[i16]) -> String {
    let frame = cfg.frame();
    let whole = samples.len() - samples.len() % frame;
    let mut filter = ClickFilter::new(cfg);
    let mut timings: Vec<f64> = Vec::with_capacity(whole / frame);
    for chunk in samples[..whole].chunks(frame) {
        let started = std::time::Instant::now();
        filter.process_frame(chunk);
        timings.push(started.elapsed().as_secs_f64() * 1e6);
    }
    let cold = timings.first().copied().unwrap_or(0.0);
    let warm: Vec<f64> = timings.iter().skip(1).copied().collect();
    let mean = warm.iter().sum::<f64>() / warm.len().max(1) as i64 as f64;
    let mut indexed: Vec<(f64, usize)> = warm.iter().copied().zip(1..).collect();
    indexed.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("durations are ordered"));
    let percentile = |q: f64| -> f64 {
        if indexed.is_empty() {
            0.0
        } else {
            indexed[((indexed.len() as f64 - 1.0) * q).round() as usize].0
        }
    };
    let slowest = indexed.last().copied().unwrap_or((0.0, 0));
    format!(
        "{:.1} us/frame  (cold {:.1}, mean {:.1}, p50 {:.1}, p95 {:.1}, p99 {:.1}, \
         max {:.1} at frame {}) over {} frames",
        (mean * warm.len() as f64 + cold) / timings.len().max(1) as f64,
        cold,
        mean,
        percentile(0.5),
        percentile(0.95),
        percentile(0.99),
        slowest.0,
        slowest.1 + 1, // the dropped first frame keeps the numbering honest
        timings.len(),
    )
}

/// Artifact name: every knob that changes the audio, so two configurations
/// cannot write over each other's artifacts.
pub fn config_tag(cfg: &Config, extra: &str) -> String {
    let mut policy = cfg.policy().as_str().to_string();
    let overrides: Vec<String> = ClickClass::ALL
        .iter()
        .filter_map(|class| {
            cfg.policy_overrides()
                .get(class)
                .map(|value| format!("{class}={value}"))
        })
        .collect();
    if !overrides.is_empty() {
        policy = format!("{}+{}", policy, overrides.join(","));
    }
    let tails: Vec<String> = ClickClass::ALL
        .iter()
        .map(|class| (cfg.tail_ms(*class) as i64).to_string())
        .collect();
    let mut parts = vec![
        policy,
        format!("pre{}", cfg.pre()),
        format!("post{}", cfg.post()),
        format!("xf{}", cfg.xfade()),
        format!("tail{}", tails.join("-")),
        format!("on-{}", join_classes(cfg.on_classes())),
    ];
    if !extra.is_empty() {
        parts.push(extra.to_string());
    }
    parts.join("_")
}

fn join_classes(classes: &[ClickClass]) -> String {
    classes
        .iter()
        .map(|class| class.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

/// Registry-relative paths resolve under `$UBC125_ML_DATA`; an explicitly set
/// root that cannot be read is an error, not a silent fallback.
fn resolve_input(file: &str) -> Result<PathBuf, Usage> {
    let path = Path::new(file);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let root = match std::env::var("UBC125_ML_DATA") {
        Ok(root) => {
            if !Path::new(&root).is_dir() {
                return Err(Usage::DataRoot(format!(
                    "UBC125_ML_DATA={root} is set but is not a readable directory"
                )));
            }
            root
        }
        Err(_) => DATA_FALLBACK.to_string(),
    };
    Ok(Path::new(&root).join(file))
}

/// The registry's shape for an artifact name: `<session_id>.<recording_id>`,
/// which for a registry-relative path is its directory then its file name.
fn derived_stem(file: &str) -> String {
    let trimmed = file.trim_end_matches(".wav");
    match trimmed.split_once('/') {
        Some((dir, name)) if !name.is_empty() => format!("{dir}.{name}"),
        _ => trimmed.to_string(),
    }
}

fn write_artifacts(
    out_dir: &Path,
    tag: &str,
    stem: &str,
    corrected: &[i16],
    events: &[EventRecord],
) -> Result<(), Usage> {
    let base = out_dir.join(tag).join(format!("{stem}.{tag}"));
    if let Some(parent) = base.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_reference_wav(&suffixed(&base, ".wav"), corrected)?;
    let file = std::fs::File::create(suffixed(&base, ".events.csv"))?;
    write_events_csv(file, events)?;
    let file = std::fs::File::create(suffixed(&base, ".labels.tsv"))?;
    write_labels_tsv(file, events)
}

/// Append a suffix to a path: the artifact names already contain dots, so
/// `with_extension` would replace part of the name instead of adding a suffix.
fn suffixed(base: &Path, suffix: &str) -> PathBuf {
    let mut name = base.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The event CSV, column-for-column the rig's `EVENT_FIELDS`, with CRLF line
/// endings and Python's `True`/`False`/empty cells so the files compare equal.
fn write_events_csv(mut file: impl Write, events: &[EventRecord]) -> Result<(), Usage> {
    let header = "onset,end,run_len,class,capped,decision,peak,window_start,window_end,\
                  tail_samples,policy,right_edge_ramp,pre_dbfs,post_dbfs";
    writeln_crlf(&mut file, header)?;
    for event in events {
        let row = [
            event.onset.to_string(),
            event.end.to_string(),
            event.run_len.to_string(),
            event.class.as_str().to_string(),
            py_bool(event.capped),
            event.decision.as_str().to_string(),
            rounded(event.peak, 4),
            option_number(event.window_start),
            option_number(event.window_end),
            option_number(event.tail_samples),
            event
                .policy
                .map_or(String::new(), |p| p.as_str().to_string()),
            event.right_edge_ramp.map_or(String::new(), py_bool),
            event.pre_dbfs.map_or(String::new(), |v| rounded(v, 1)),
            event.post_dbfs.map_or(String::new(), |v| rounded(v, 1)),
        ];
        writeln_crlf(&mut file, &row.join(","))?;
    }
    Ok(())
}

/// Audacity label track, to jump between events while listening.
fn write_labels_tsv(mut file: impl Write, events: &[EventRecord]) -> Result<(), Usage> {
    let quarter = (RATE as i64) / 4;
    for (index, event) in events.iter().enumerate() {
        let start = (event.onset - quarter).max(0) as f64 / RATE;
        let stop = event.window_end.unwrap_or(event.end) + quarter;
        writeln_plain(
            &mut file,
            &format!(
                "{:.6}\t{:.6}\t{} {} {}\n",
                start,
                stop as f64 / RATE,
                index + 1,
                event.class.as_str(),
                event.decision.as_str()
            ),
        )?;
    }
    Ok(())
}

fn writeln_crlf(file: &mut impl Write, text: &str) -> Result<(), Usage> {
    writeln_plain(file, &format!("{text}\r\n"))
}

fn writeln_plain(file: &mut impl Write, text: &str) -> Result<(), Usage> {
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn py_bool(value: bool) -> String {
    if value { "True" } else { "False" }.to_string()
}

fn option_number(value: Option<i64>) -> String {
    value.map_or(String::new(), |v| v.to_string())
}

fn split_flag(text: &str) -> Result<(&str, Option<String>), Usage> {
    if !text.starts_with('-') {
        return Err(Usage::UnknownFlag(text.to_string()));
    }
    match text.split_once('=') {
        Some((flag, value)) => Ok((flag, Some(value.to_string()))),
        None => Ok((text, None)),
    }
}

fn policy(text: &str, flag: &str) -> Result<Policy, Usage> {
    Policy::parse(text).ok_or_else(|| bad_value(flag, text))
}

fn polarity(text: &str, flag: &str) -> Result<Polarity, Usage> {
    Polarity::parse(text).ok_or_else(|| bad_value(flag, text))
}

fn class_list(text: &str, flag: &str) -> Result<Vec<ClickClass>, Usage> {
    let mut out = Vec::new();
    for name in text
        .split(',')
        .map(|part| part.trim())
        .filter(|p| !p.is_empty())
    {
        out.push(ClickClass::parse(name).ok_or_else(|| bad_value(flag, name))?);
    }
    Ok(out)
}

fn number<T>(text: &str, flag: &str) -> Result<T, Usage>
where
    T: std::str::FromStr,
{
    text.parse::<T>().map_err(|_| bad_value(flag, text))
}

fn bad_value(flag: &str, value: &str) -> Usage {
    Usage::BadValue {
        flag: flag.to_string(),
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn tag_names_the_baseline_and_the_selected_arm() {
        let base = config_tag(&Config::default(), "");
        assert_eq!(base, "interp_pre96_post480_xf96_tail0-0-0-0_on-short+long");
        let arm3 = config_tag(
            &Config::builder()
                .policy_override(ClickClass::Long, Policy::Descend)
                .tail_ms(ClickClass::Long, 150.0)
                .build(),
            "",
        );
        assert_eq!(
            arm3,
            "interp+long=descend_pre96_post480_xf96_tail0-150-0-0_on-short+long"
        );
        // Two configurations that would sound different must not share a name.
        let variants = [
            Config::builder().xfade_ms(0.5).build(),
            Config::builder().pre_ms(4.0).build(),
            Config::builder().post_ms(20.0).build(),
            Config::builder().policy(Policy::Mute).build(),
            Config::builder()
                .policy_override(ClickClass::Long, Policy::Descend)
                .tail_ms(ClickClass::Long, 150.0)
                .build(),
            Config::builder().tail_ms(ClickClass::Long, 150.0).build(),
            Config::builder()
                .on_classes(&[ClickClass::Short, ClickClass::Long, ClickClass::Xlong])
                .build(),
        ];
        for other in variants {
            assert_ne!(config_tag(&other, ""), base, "{:?}", other.policy());
        }
        assert!(config_tag(&Config::builder().xfade_ms(0.5).build(), "").contains("_xf24"));
        assert!(
            config_tag(
                &Config::builder().tail_ms(ClickClass::Long, 150.0).build(),
                ""
            )
            .contains("tail0-150-0-0")
        );
        assert!(
            config_tag(
                &Config::builder()
                    .on_classes(&[ClickClass::Short, ClickClass::Long, ClickClass::Xlong])
                    .build(),
                ""
            )
            .contains("on-short+long+xlong")
        );
        assert!(config_tag(&Config::default(), "extra").ends_with("_extra"));
    }

    #[test]
    fn flags_build_the_configuration_of_record() {
        let options = Options::parse(&argv(
            "--file raw60.wav --policy interp --policy-long descend --tail-long-ms 150",
        ))
        .expect("parse");
        let cfg = options.config().expect("config");
        assert_eq!(cfg.delay(), 984);
        assert_eq!(cfg.tail_samples(ClickClass::Long), 7200);
        assert_eq!(cfg.policy_for(ClickClass::Long), Policy::Descend);
        assert_eq!(cfg.policy_for(ClickClass::Short), Policy::Interp);
    }

    #[test]
    fn inline_values_and_equals_forms_agree() {
        let spaced = Options::parse(&argv("--file a.wav --pre-ms 4")).unwrap();
        let joined = Options::parse(&argv("--file=a.wav --pre-ms=4")).unwrap();
        assert_eq!(
            spaced.config().unwrap().pre(),
            joined.config().unwrap().pre()
        );
    }

    #[test]
    fn a_refused_configuration_is_reported() {
        // descend with no tail on the class it corrects.
        let options = Options::parse(&argv("--file a.wav --policy-long descend")).unwrap();
        let err = options.config().expect_err("must refuse");
        assert!(err.to_string().contains("needs a recovery tail"));
    }

    #[test]
    fn unknown_flags_and_bad_numbers_are_reported() {
        assert!(matches!(
            Options::parse(&argv("--file a.wav --denoise")),
            Err(Usage::UnknownFlag(_))
        ));
        assert!(matches!(
            Options::parse(&argv("--file a.wav --clip loud")),
            Err(Usage::BadValue { .. })
        ));
        assert!(matches!(
            Options::parse(&argv("--file a.wav --policy")),
            Err(Usage::MissingValue(_))
        ));
        assert!(matches!(Options::parse(&argv("")), Err(Usage::NoInput)));
    }

    #[test]
    fn stems_drop_the_directory_and_the_extension() {
        assert_eq!(
            derived_stem("scan-2026-08-28-a/scan60.wav"),
            "scan-2026-08-28-a.scan60"
        );
        assert_eq!(derived_stem("raw60.wav"), "raw60");
    }
}
