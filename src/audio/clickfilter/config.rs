//! Filter configuration: every tunable, in physical units where that helps.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/config.py`. A policy is chosen per class:
//! [`Config::policy`] is the default for every class and an override replaces it
//! for one class (`long → descend`). The classes actually corrected are
//! `on_classes`, and everything the delay floor, the ring and the tail
//! validation depend on is derived from those, so switching a class off or
//! widening a class bound cannot leave a stale constant behind.

use std::collections::BTreeMap;

use crate::audio::clickfilter::constants::{
    CLASS_BOUNDS, ClickClass, Polarity, Policy, ms_to_samples, samples_to_ms,
};
use crate::audio::clickfilter::fill::fill_meta;
use crate::audio::clickfilter::format::{rounded, shortest};

/// The class vocabulary in the order `json.dumps(sort_keys=True)` puts it, which
/// is what the reference's config line shows for the per-class maps.
const CLICK_CLASSES_BY_NAME: [ClickClass; 4] = [
    ClickClass::Long,
    ClickClass::Other,
    ClickClass::Short,
    ClickClass::Xlong,
];

/// A configuration the filter cannot honour, with the reference's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    UnknownPolicy(String),
    UnknownPolarity(String),
    MaxPlateauTooSmall(i64),
    NegativeSamples {
        knob: &'static str,
        value: i64,
    },
    /// `descend` reaches zero at `window_end`, so it needs a recovery tail.
    DescendNeedsTail(Vec<ClickClass>),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::UnknownPolicy(name) => {
                write!(
                    f,
                    "policy must be one of interp, descend, mute, lf-null; got {name:?}"
                )
            }
            ConfigError::UnknownPolarity(name) => {
                write!(f, "polarity must be 'negative' or 'any'; got {name:?}")
            }
            ConfigError::MaxPlateauTooSmall(value) => write!(
                f,
                "max_plateau must be at least 1 sample, got {value}: a cap below one \
                 sample would make the cap split non-terminating"
            ),
            ConfigError::NegativeSamples { knob, value } => write!(
                f,
                "{knob} must not be negative, got {value} samples: the rig would \
                 build an empty ramp for it"
            ),
            ConfigError::DescendNeedsTail(classes) => write!(
                f,
                "the descend fill reaches zero at window_end, so it needs a recovery \
                    tail on the classes it corrects: [{}] Give those classes a tail \
                    (e.g. --tail-long-ms 150), or use another policy.",
                classes
                    .iter()
                    .map(|c| format!("{c:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Everything tunable in the filter, in physical units where that helps.
///
/// Build one with [`Config::builder`]; the defaults are the reference rig's, and
/// `Default` is the baseline artifact tag
/// (`interp_pre96_post480_xf96_tail0-0-0-0_on-short+long`).
#[derive(Debug, Clone)]
pub struct Config {
    policy: Policy,
    overrides: BTreeMap<ClickClass, Policy>,
    clip: f64,
    polarity: Polarity,
    min_run: i64,
    max_plateau: i64,
    frame: usize,
    pre: i64,
    post: i64,
    xfade: i64,
    lf_cut: f64,
    tails_ms: BTreeMap<ClickClass, f64>,
    on_classes: Vec<ClickClass>,
    min_delay: i64,
    delay: i64,
    delay_requested_ms: Option<f64>,
}

impl Default for Config {
    fn default() -> Self {
        // The reference's own defaults; `build` fills in the derived figures.
        Config::builder().build()
    }
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder {
            policy: Policy::Interp,
            overrides: BTreeMap::new(),
            clip: 0.98,
            polarity: Polarity::Negative,
            min_run: 4,
            max_plateau: 400,
            frame: crate::audio::clickfilter::constants::FRAME,
            pre_ms: 2.0,
            post_ms: 10.0,
            xfade_ms: 2.0,
            lf_cut: 180.0,
            tails_ms: BTreeMap::new(),
            on_classes: vec![ClickClass::Short, ClickClass::Long],
            delay_ms: None,
        }
    }

    /// The same configuration with a deliberately different output delay.
    ///
    /// Only the legality probes and the tests use this: the delay a build
    /// derives is the smallest legal one, and setting a smaller value makes the
    /// filter refuse writes instead of rewriting emitted samples.
    pub fn with_delay(&self, delay: i64) -> Config {
        let mut copy = self.clone();
        copy.delay = delay;
        copy
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    pub fn policy_overrides(&self) -> &BTreeMap<ClickClass, Policy> {
        &self.overrides
    }

    pub fn clip(&self) -> f64 {
        self.clip
    }

    pub fn polarity(&self) -> Polarity {
        self.polarity
    }

    pub fn min_run(&self) -> i64 {
        self.min_run
    }

    pub fn max_plateau(&self) -> i64 {
        self.max_plateau
    }

    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn pre(&self) -> i64 {
        self.pre
    }

    pub fn post(&self) -> i64 {
        self.post
    }

    pub fn xfade(&self) -> i64 {
        self.xfade
    }

    pub fn lf_cut(&self) -> f64 {
        self.lf_cut
    }

    pub fn on_classes(&self) -> &[ClickClass] {
        &self.on_classes
    }

    pub fn min_delay(&self) -> i64 {
        self.min_delay
    }

    pub fn delay(&self) -> i64 {
        self.delay
    }

    pub fn delay_ms(&self) -> f64 {
        samples_to_ms(self.delay)
    }

    pub fn delay_requested_ms(&self) -> Option<f64> {
        self.delay_requested_ms
    }

    /// The policy that corrects `class`: its override, else the default.
    pub fn policy_for(&self, class: ClickClass) -> Policy {
        self.overrides.get(&class).copied().unwrap_or(self.policy)
    }

    /// Is this class corrected, or classified and passed through?
    pub fn is_on(&self, class: ClickClass) -> bool {
        self.on_classes.contains(&class)
    }

    /// Distinct policies this config can actually run, default first.
    pub fn policies_used(&self) -> Vec<Policy> {
        let mut used: Vec<Policy> = Vec::new();
        for class in &self.on_classes {
            let policy = self.policy_for(*class);
            if !used.contains(&policy) {
                used.push(policy);
            }
        }
        used
    }

    /// Context samples the fill for `class` reads from before its window.
    pub fn pad_for(&self, class: ClickClass) -> i64 {
        fill_meta(self.policy_for(class)).pad
    }

    /// The pad the ring and delay floor must reserve: the widest pad over the
    /// classes this config corrects (0 when it corrects nothing).
    pub fn context_pad(&self) -> i64 {
        self.on_classes
            .iter()
            .map(|class| self.pad_for(*class))
            .max()
            .unwrap_or(0)
    }

    /// Samples the output must trail the input by, for a legal correction.
    ///
    /// The oldest sample a correction writes is `pre` before the onset, and the
    /// correction only runs once the sample just after the window exists, which
    /// is `post` after a plateau that may itself be `max_plateau` long.
    pub fn required_delay(&self) -> i64 {
        self.max_plateau + self.post + self.pre + self.context_pad()
    }

    /// The longest plateau this config can correct, from the class bounds it
    /// switched on — `max_plateau` is only a conservative stand-in for it,
    /// because a run that reaches `max_plateau` is capped and class `other`.
    pub fn max_correctable_run(&self) -> i64 {
        CLASS_BOUNDS
            .into_iter()
            .filter(|band| self.is_on(band.class))
            .map(|band| band.hi - 1)
            .max()
            .unwrap_or(0)
    }

    /// The floor the delay could legally drop to: the longest correctable run
    /// plus post plus pre (../ubc125-ml/docs/prototype.md, "Delay floor"). With no class
    /// switched on there is no correction to fit, so the floor is 0.
    pub fn tight_delay(&self) -> i64 {
        let longest = self.max_correctable_run();
        if longest > 0 {
            longest + self.post + self.pre
        } else {
            0
        }
    }

    /// Recovery-tail length for `class`, in samples.
    pub fn tail_samples(&self, class: ClickClass) -> i64 {
        ms_to_samples(self.tails_ms.get(&class).copied().unwrap_or(0.0))
    }

    /// Recovery-tail length for `class`, in milliseconds as configured.
    pub fn tail_ms(&self, class: ClickClass) -> f64 {
        self.tails_ms.get(&class).copied().unwrap_or(0.0)
    }

    /// Every tunable, in the key set and order the reference prints as JSON
    /// (`json.dumps(cfg.as_dict(), sort_keys=True)`), so a ported run records the
    /// same configuration text.
    pub fn as_json(&self) -> String {
        let mut overrides: Vec<(&ClickClass, &Policy)> = self.overrides.iter().collect();
        overrides.sort_by_key(|(class, _)| class.as_str());
        let tails: Vec<String> = CLICK_CLASSES_BY_NAME
            .iter()
            .map(|class| json_pair(class.as_str(), &shortest(self.tail_ms(*class))))
            .collect();
        let mut json = Json::default();
        json.number("clip", self.clip);
        json.integer("context_pad_samples", self.context_pad());
        json.raw("delay_ms", &rounded(self.delay_ms(), 3));
        json.integer("delay_samples", self.delay);
        json.integer("frame", self.frame as i64);
        json.number("lf_cut_hz", self.lf_cut);
        json.integer("max_correctable_run", self.max_correctable_run());
        json.integer("max_plateau", self.max_plateau);
        json.integer("min_delay_samples", self.min_delay);
        json.integer("min_run", self.min_run);
        json.list(
            "on_classes",
            &self
                .on_classes
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>(),
        );
        json.text("polarity", self.polarity.as_str());
        json.list(
            "policies_used",
            &self
                .policies_used()
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>(),
        );
        json.text("policy", self.policy.as_str());
        json.raw(
            "policy_overrides",
            &format!(
                "{{{}}}",
                overrides
                    .into_iter()
                    .map(|(class, policy)| json_pair(class.as_str(), &json_text(policy.as_str())))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        json.integer("post_samples", self.post);
        json.integer("pre_samples", self.pre);
        json.raw("tails_ms", &format!("{{{}}}", tails.join(", ")));
        json.integer("tight_delay_samples", self.tight_delay());
        json.integer("xfade_samples", self.xfade);
        json.finish()
    }
}

/// A tiny ordered JSON object writer: the report's configuration line is the
/// provenance record for a run, so it has to be emitted, not depend on a crate.
#[derive(Default)]
struct Json {
    parts: Vec<String>,
}

impl Json {
    fn raw(&mut self, key: &str, value: &str) {
        self.parts.push(json_pair(key, value));
    }

    fn number(&mut self, key: &str, value: f64) {
        self.raw(key, &shortest(value));
    }

    fn integer(&mut self, key: &str, value: i64) {
        self.raw(key, &value.to_string());
    }

    fn text(&mut self, key: &str, value: &str) {
        self.raw(key, &json_text(value));
    }

    fn list(&mut self, key: &str, items: &[&str]) {
        self.raw(
            key,
            &format!(
                "[{}]",
                items
                    .iter()
                    .map(|item| json_text(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    fn finish(self) -> String {
        format!("{{{}}}", self.parts.join(", "))
    }
}

fn json_pair(key: &str, value: &str) -> String {
    format!("{}: {value}", json_text(key))
}

fn json_text(value: &str) -> String {
    format!("\"{value}\"")
}

/// Builder for [`Config`]: one knob per method, validated on `build`.
#[derive(Debug, Clone)]
pub struct ConfigBuilder {
    policy: Policy,
    overrides: BTreeMap<ClickClass, Policy>,
    clip: f64,
    polarity: Polarity,
    min_run: i64,
    max_plateau: i64,
    frame: usize,
    pre_ms: f64,
    post_ms: f64,
    xfade_ms: f64,
    lf_cut: f64,
    tails_ms: BTreeMap<ClickClass, f64>,
    on_classes: Vec<ClickClass>,
    delay_ms: Option<f64>,
}

impl ConfigBuilder {
    pub fn policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy_override(mut self, class: ClickClass, policy: Policy) -> Self {
        self.overrides.insert(class, policy);
        self
    }

    pub fn clip(mut self, clip: f64) -> Self {
        self.clip = clip;
        self
    }

    pub fn polarity(mut self, polarity: Polarity) -> Self {
        self.polarity = polarity;
        self
    }

    pub fn min_run(mut self, min_run: i64) -> Self {
        self.min_run = min_run;
        self
    }

    pub fn max_plateau(mut self, max_plateau: i64) -> Self {
        self.max_plateau = max_plateau;
        self
    }

    pub fn frame(mut self, frame: usize) -> Self {
        self.frame = frame;
        self
    }

    pub fn pre_ms(mut self, ms: f64) -> Self {
        self.pre_ms = ms;
        self
    }

    pub fn post_ms(mut self, ms: f64) -> Self {
        self.post_ms = ms;
        self
    }

    pub fn xfade_ms(mut self, ms: f64) -> Self {
        self.xfade_ms = ms;
        self
    }

    pub fn lf_cut(mut self, hz: f64) -> Self {
        self.lf_cut = hz;
        self
    }

    pub fn tail_ms(mut self, class: ClickClass, ms: f64) -> Self {
        self.tails_ms.insert(class, ms);
        self
    }

    pub fn on_classes(mut self, classes: &[ClickClass]) -> Self {
        self.on_classes = classes.to_vec();
        self
    }

    pub fn delay_ms(mut self, ms: f64) -> Self {
        self.delay_ms = Some(ms);
        self
    }

    /// Finish the configuration, or refuse it. `unwrap`-friendly for tests: the
    /// `Default` path is infallible by construction.
    pub fn build(self) -> Config {
        self.try_build()
            .expect("the default knobs are valid; a refused configuration must be checked")
    }

    /// Finish the configuration, reporting every way it can be wrong.
    pub fn try_build(self) -> Result<Config, ConfigError> {
        if self.max_plateau < 1 {
            // A cap below one sample would make the cap split non-terminating.
            return Err(ConfigError::MaxPlateauTooSmall(self.max_plateau));
        }
        let pre = ms_to_samples(self.pre_ms);
        let post = ms_to_samples(self.post_ms);
        let xfade = ms_to_samples(self.xfade_ms);
        for (knob, value) in [("pre", pre), ("post", post), ("xfade", xfade)] {
            if value < 0 {
                return Err(ConfigError::NegativeSamples { knob, value });
            }
        }
        for class in ClickClass::ALL {
            let value = self.tail_samples_for(&class);
            if value < 0 {
                return Err(ConfigError::NegativeSamples {
                    knob: "tail",
                    value,
                });
            }
        }
        let mut cfg = Config {
            policy: self.policy,
            overrides: self.overrides,
            clip: self.clip,
            polarity: self.polarity,
            min_run: self.min_run,
            max_plateau: self.max_plateau,
            frame: self.frame,
            pre,
            post,
            xfade,
            lf_cut: self.lf_cut,
            tails_ms: self.tails_ms,
            on_classes: self.on_classes,
            min_delay: 0,
            delay: 0,
            delay_requested_ms: self.delay_ms,
        };
        cfg.require_tail_where_descend_reaches_zero()?;
        cfg.min_delay = cfg.required_delay();
        let requested = cfg.delay_requested_ms.map(ms_to_samples).unwrap_or(0);
        cfg.delay = requested.max(cfg.min_delay);
        Ok(cfg)
    }

    fn tail_samples_for(&self, class: &ClickClass) -> i64 {
        ms_to_samples(self.tails_ms.get(class).copied().unwrap_or(0.0))
    }
}

impl Config {
    /// `descend` reaches zero at `window_end` and stops there: without a
    /// recovery tail the audio after the window snaps back at full click level,
    /// which is the step the fill exists to avoid. Refuse the combination rather
    /// than listen to it.
    fn require_tail_where_descend_reaches_zero(&self) -> Result<(), ConfigError> {
        let bare: Vec<ClickClass> = self
            .on_classes
            .iter()
            .copied()
            .filter(|class| {
                self.policy_for(*class) == Policy::Descend && self.tail_samples(*class) <= 0
            })
            .collect();
        if bare.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::DescendNeedsTail(bare))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_text_matches_the_rigs_json_line() {
        // Verbatim from `json.dumps(Config().as_dict(), sort_keys=True)` in the
        // dev shell, so a run's provenance record reads the same in either
        // implementation — including the key order Python's sort produces
        // (`polarity` before `policies_used`, `long` before `short`).
        const BASELINE: &str = concat!(
            r#"{"clip": 0.98, "context_pad_samples": 8, "delay_ms": 20.5, "delay_samples": 984, "#,
            r#""frame": 960, "lf_cut_hz": 180.0, "max_correctable_run": 169, "max_plateau": 400, "#,
            r#""min_delay_samples": 984, "min_run": 4, "on_classes": ["short", "long"], "#,
            r#""polarity": "negative", "policies_used": ["interp"], "policy": "interp", "#,
            r#""policy_overrides": {}, "post_samples": 480, "pre_samples": 96, "tails_ms": "#,
            r#"{"long": 0.0, "other": 0.0, "short": 0.0, "xlong": 0.0}, "#,
            r#""tight_delay_samples": 745, "xfade_samples": 96}"#,
        );
        assert_eq!(Config::default().as_json(), BASELINE);
        // The config of record: a per-class override, one tail, and the derived
        // figures that go with them.
        let arm3 = Config::builder()
            .policy_override(ClickClass::Long, Policy::Descend)
            .tail_ms(ClickClass::Long, 150.0)
            .build();
        let text = arm3.as_json();
        for fragment in [
            r#""policies_used": ["interp", "descend"]"#,
            r#""policy_overrides": {"long": "descend"}"#,
            r#""tails_ms": {"long": 150.0, "other": 0.0, "short": 0.0, "xlong": 0.0}"#,
            r#""delay_samples": 984"#,
            r#""tight_delay_samples": 745"#,
        ] {
            assert!(text.contains(fragment), "missing {fragment} in {text}");
        }
    }

    #[test]
    fn geometry_and_derived_delays_follow_the_configuration() {
        let cfg = Config::default();
        assert_eq!((cfg.pre(), cfg.post(), cfg.xfade()), (96, 480, 96));
        assert_eq!(
            (cfg.context_pad(), cfg.min_delay(), cfg.delay()),
            (8, 984, 984)
        );
        assert!(
            (cfg.delay_ms() - 20.5).abs() < 1e-9,
            "delay_ms {}",
            cfg.delay_ms()
        );
        // A tail is measured in samples from milliseconds, half-to-even like the rig.
        assert_eq!(ms_to_samples(0.5), 24);
        assert_eq!(ms_to_samples(2.0), 96);
        assert_eq!(ms_to_samples(150.0), 7200);
        // lf-null's pad is inside the delay it reports; a class that is off asks
        // for nothing.
        let lf = Config::builder().policy(Policy::LowBandNull).build();
        assert_eq!((lf.context_pad(), lf.delay()), (288, 1264));
        let off = Config::builder()
            .policy_override(ClickClass::Xlong, Policy::LowBandNull)
            .build();
        assert_eq!(
            off.context_pad(),
            8,
            "xlong is off, so its pad is not needed"
        );
        assert_eq!(Config::builder().on_classes(&[]).build().tight_delay(), 0);
    }

    #[test]
    fn an_unknown_policy_or_polarity_text_is_rejected_by_the_parser() {
        // The CLI parses text into these enums; the rig raises ValueError on the
        // same inputs, so the port refuses them too.
        assert!(Policy::parse("denoise").is_none());
        assert!(Polarity::parse("positive").is_none());
        assert!(ClickClass::parse("huge").is_none());
        assert_eq!(Policy::parse("lf-null"), Some(Policy::LowBandNull));
        assert_eq!(ClickClass::parse("xlong"), Some(ClickClass::Xlong));
    }
}
