//! The streaming filter: frames in, frames out, fixed output delay.
//!
//! Port of `../ubc125-ml/scripts/clickfilter/filter.py`, and the piece the runtime contract
//! in `../ubc125-ml/docs/deployment.md` is about:
//!
//!   * `process_frame` takes one `frame`-sample frame and returns the samples
//!     the fixed output delay releases with it — variable length (0 for the
//!     first frame at the default delay, then frame-sized). `flush`/`finish`
//!     drain the rest, so output length equals input length and output positions
//!     equal input positions.
//!   * Correction is local and bounded: one blend window over the plateau plus
//!     one forward recovery ramp on the ring-down. Elsewhere the output is
//!     bit-exact.
//!   * The filter refuses to rewrite a sample it has already emitted and counts
//!     such refusals ([`Metrics::late_writes`], which must stay 0).

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::constants::{ClickClass, Policy};
use crate::audio::clickfilter::detect::{Candidate, PlateauTrigger, classify};
use crate::audio::clickfilter::fill::{Fill, RightEdge, cosine_blend, make_fill, ramp_to_unity};
use crate::audio::clickfilter::ring::PcmRing;

/// What the filter decided about one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Correct,
    PassThrough,
    /// The window's oldest sample had already been emitted: refused, counted.
    TooLate,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Correct => "correct",
            Decision::PassThrough => "pass-through",
            Decision::TooLate => "too-late",
        }
    }
}

/// One event's bookkeeping. The `Option` fields stand in for the reference's
/// "key present only when a correction was applied": [`EventRecord::applied`]
/// is the check the rig writes `if "window_end" in e`.
#[derive(Debug, Clone)]
pub struct EventRecord {
    pub onset: i64,
    pub end: i64,
    pub run_len: i64,
    pub class: ClickClass,
    pub capped: bool,
    pub decision: Decision,
    /// `max |x|` over the plateau, full scale.
    pub peak: f64,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
    pub tail_samples: Option<i64>,
    pub gain_end: Option<i64>,
    pub policy: Option<Policy>,
    pub right_edge_ramp: Option<bool>,
    pub pre_dbfs: Option<f64>,
    pub post_dbfs: Option<f64>,
}

impl EventRecord {
    fn new(candidate: &Candidate, class: ClickClass, decision: Decision, peak: f64) -> Self {
        EventRecord {
            onset: candidate.onset,
            end: candidate.end,
            run_len: candidate.run_len,
            class,
            capped: candidate.capped,
            decision,
            peak,
            window_start: None,
            window_end: None,
            tail_samples: None,
            gain_end: None,
            policy: None,
            right_edge_ramp: None,
            pre_dbfs: None,
            post_dbfs: None,
        }
    }

    /// A correction window was recorded for this event.
    pub fn applied(&self) -> bool {
        self.window_end.is_some()
    }
}

/// Counts the filter reports for inspection. `late_writes` and
/// `changed_outside_windows` (see [`super::checks`]) must both stay 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metrics {
    pub frames_in: i64,
    pub samples_in: i64,
    pub samples_out: i64,
    pub candidates: i64,
    pub corrected: i64,
    pub skipped: i64,
    pub capped: i64,
    pub late_writes: i64,
    pub overlaps: i64,
}

/// Forward gain curve applied to samples as they arrive.
///
/// A recovery tail is one ramp over the whole tail length, split at the boundary
/// of what has been ingested: the in-ring slice and this continuation are
/// consecutive slices of the same ramp, so the join has no step.
#[derive(Debug, Clone)]
pub struct GainPlan {
    pub start: i64,
    pub curve: Vec<f64>,
}

impl GainPlan {
    fn end(&self) -> i64 {
        self.start + self.curve.len() as i64
    }
}

/// Streaming click filter: frames in, frames out, fixed output delay.
pub struct ClickFilter {
    cfg: Config,
    pad: i64,
    ring: PcmRing,
    trigger: PlateauTrigger,
    fills: Vec<(Policy, Box<dyn Fill>)>,
    gain_plans: Vec<GainPlan>,
    queued: Vec<Candidate>,
    head: i64,
    out: i64,
    events: Vec<EventRecord>,
    metrics: Metrics,
}

impl ClickFilter {
    pub fn new(cfg: &Config) -> Self {
        let pad = cfg.context_pad();
        let ring_capacity = (cfg.delay() + 2 * cfg.frame() as i64 + 2 * pad + 512) as usize;
        let fills = cfg
            .policies_used()
            .into_iter()
            .map(|policy| (policy, make_fill(policy, cfg)))
            .collect();
        ClickFilter {
            cfg: cfg.clone(),
            pad,
            ring: PcmRing::new(ring_capacity),
            trigger: PlateauTrigger::new(cfg),
            fills,
            gain_plans: Vec::new(),
            queued: Vec::new(),
            head: 0,
            out: 0,
            events: Vec::new(),
            metrics: Metrics::default(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn events(&self) -> &[EventRecord] {
        &self.events
    }

    pub fn events_mut(&mut self) -> &mut Vec<EventRecord> {
        &mut self.events
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn config_pad(&self) -> i64 {
        self.pad
    }

    /// Ingest one production frame and return the samples the delay releases.
    pub fn process_frame(&mut self, frame: &[i16]) -> Vec<i16> {
        assert_eq!(
            frame.len(),
            self.cfg.frame(),
            "expected {} samples, got {}",
            self.cfg.frame(),
            frame.len()
        );
        let delay = self.cfg.delay();
        self.ingest(frame);
        // Fixed output delay: position p is final once input p + delay has been
        // ingested, so emit only what the delay releases. Variable length per
        // call (0, then frame-sized once the delay is covered).
        let available = self.head - delay - self.out;
        self.emit(available)
    }

    /// Drain the delay line at end of stream: the live path, full frames only.
    pub fn flush(&mut self) -> Vec<i16> {
        self.finish(&[])
    }

    /// Drain the delay line, after ingesting the file's ragged final partial
    /// frame.
    ///
    /// `production` `flush()` takes no argument and input frames are always
    /// full; the offline rig ends on a short frame, so the two paths differ only
    /// by this argument, which must be shorter than a frame. That is the
    /// deviation `../ubc125-ml/docs/deployment.md` asks the port to resolve: it is resolved by
    /// making the extra input explicit here rather than by two code paths.
    pub fn finish(&mut self, partial: &[i16]) -> Vec<i16> {
        assert!(
            partial.len() < self.cfg.frame(),
            "a final partial frame must be shorter than a frame: {}",
            partial.len()
        );
        if !partial.is_empty() {
            self.ingest(partial);
        }
        for candidate in self.trigger.close_open(self.head) {
            self.plan(&candidate);
        }
        let waiting = std::mem::take(&mut self.queued);
        for candidate in waiting {
            self.apply(&candidate, true);
        }
        let remaining = self.head - self.out;
        self.emit(remaining)
    }

    // -- internals ---------------------------------------------------------

    fn ingest(&mut self, frame: &[i16]) {
        let base = self.head;
        self.ring.push(base, frame);
        self.apply_gain_plans(base, frame.len());
        for candidate in self.trigger.feed(frame, base) {
            self.plan(&candidate);
        }
        self.head += frame.len() as i64;
        self.apply_queued();
        self.metrics.frames_in += 1;
        self.metrics.samples_in += frame.len() as i64;
    }

    fn apply_gain_plans(&mut self, base: i64, n: usize) {
        let end = base + n as i64;
        let mut live: Vec<GainPlan> = Vec::new();
        for plan in std::mem::take(&mut self.gain_plans) {
            let from = plan.start.max(base);
            let to = plan.end().min(end);
            if from < to {
                let offset = (from - plan.start) as usize;
                let count = (to - from) as usize;
                self.ring
                    .scale_gains(from, &plan.curve[offset..offset + count]);
            }
            if plan.end() > end {
                live.push(plan);
            }
        }
        self.gain_plans = live;
    }

    fn plan(&mut self, candidate: &Candidate) {
        self.metrics.candidates += 1;
        if candidate.capped {
            self.metrics.capped += 1;
        }
        let (class, correct) = classify(candidate, &self.cfg);
        let samples = self.ring.read(candidate.onset, candidate.run_len as usize);
        let peak = samples.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
        let decision = if correct {
            self.queued.push(*candidate);
            Decision::Correct
        } else {
            self.metrics.skipped += 1;
            Decision::PassThrough
        };
        self.events
            .push(EventRecord::new(candidate, class, decision, peak));
    }

    /// Apply queued corrections whose window's far edge has arrived.
    fn apply_queued(&mut self) {
        let post = self.cfg.post();
        let mut ready: Vec<Candidate> = Vec::new();
        let mut waiting: Vec<Candidate> = Vec::new();
        for candidate in self.queued.iter() {
            if candidate.end + post < self.head {
                ready.push(*candidate);
            } else {
                waiting.push(*candidate);
            }
        }
        self.queued = waiting;
        for candidate in ready {
            self.apply(&candidate, false);
        }
    }

    fn apply(&mut self, candidate: &Candidate, at_end: bool) {
        let pre = self.cfg.pre();
        let post = self.cfg.post();
        let start = candidate.onset - pre;
        let stop = if at_end {
            (candidate.end + post).min(self.head)
        } else {
            candidate.end + post
        };
        let record = self.record_for(candidate);
        if start < self.out || stop <= start {
            self.metrics.late_writes += 1;
            if let Some(index) = record {
                self.events[index].decision = Decision::TooLate;
            }
            return;
        }
        let class = classify(candidate, &self.cfg).0;
        let policy = self.cfg.policy_for(class);
        let pad = self.fill_pad(policy);
        let span = (stop - start) as usize;
        let ctx = self.ring.read(start - pad, span + 2 * pad as usize);
        let w1 = pad as usize + span;
        let target = self.build_fill(policy, &ctx, pad as usize, w1);
        // Recovery tail: one raised-cosine ramp over the whole tail, split at the
        // samples already ingested, so the ring slice and the gain plan join
        // without a step.
        let tail = if at_end {
            0
        } else {
            self.cfg.tail_samples(class)
        };
        // Seam policy: a fill that reaches zero at window_end joins the ramp, so
        // its right-edge crossfade would fade the correction back into the click
        // over the last xfade samples and then step from ~full scale to the
        // ramp's ~zero gain. A fill that ends at an arbitrary value keeps the
        // crossfade, because it has nothing else to hand over to.
        let right = if tail > 0 && self.fill_ends_at_zero(policy) {
            RightEdge::Hold
        } else {
            RightEdge::Fade
        };
        let weights = cosine_blend(span, self.cfg.xfade(), right);
        // Overlap policy: the earlier correction's replacement wins the slots it
        // already wrote; the count is reported for inspection.
        self.metrics.overlaps += self.ring.blend_replacement(start, &target, &weights) as i64;
        if tail > 0 {
            let ramp = ramp_to_unity(tail as usize);
            let written = (stop + tail).min(self.head);
            if written > stop {
                let count = (written - stop) as usize;
                self.ring.scale_gains(stop, &ramp[..count]);
            }
            if stop + tail > self.head {
                let from = (written - stop) as usize;
                self.gain_plans.push(GainPlan {
                    start: self.head,
                    curve: ramp[from..].to_vec(),
                });
            }
        }
        self.metrics.corrected += 1;
        if let Some(index) = record {
            let record = &mut self.events[index];
            record.window_start = Some(start);
            record.window_end = Some(stop);
            record.tail_samples = Some(tail);
            record.gain_end = Some(stop + tail);
            record.policy = Some(policy);
            record.right_edge_ramp = Some(right == RightEdge::Fade);
        }
    }

    /// The event that records this candidate: the reference scans backwards for
    /// a matching onset, and there is at most one queued candidate per onset.
    fn record_for(&self, candidate: &Candidate) -> Option<usize> {
        self.events
            .iter()
            .rposition(|record| record.onset == candidate.onset)
    }

    fn fill(&self, policy: Policy) -> &dyn Fill {
        self.fills
            .iter()
            .find(|(known, _)| *known == policy)
            .map(|(_, fill)| fill.as_ref())
            .expect("every policy in use has a fill")
    }

    fn fill_pad(&self, policy: Policy) -> i64 {
        self.fill(policy).pad()
    }

    fn fill_ends_at_zero(&self, policy: Policy) -> bool {
        self.fill(policy).ends_at_zero()
    }

    fn build_fill(&self, policy: Policy, ctx: &[f64], pad: usize, w1: usize) -> Vec<f64> {
        self.fill(policy).build(ctx, pad, w1)
    }

    fn emit(&mut self, n: i64) -> Vec<i16> {
        if n <= 0 {
            return Vec::new();
        }
        let out = self.ring.take(self.out, n as usize);
        self.out += n;
        self.metrics.samples_out += n;
        out
    }
}

/// Feed one recording through the filter, frame by frame, and drain it.
///
/// The offline shape of the runtime contract: output length equals input length
/// and output positions equal input positions.
pub fn run_filter(cfg: &Config, samples: &[i16]) -> (Vec<i16>, ClickFilter) {
    let mut filter = ClickFilter::new(cfg);
    let frame = cfg.frame();
    let whole = samples.len() - samples.len() % frame;
    let mut out: Vec<i16> = Vec::with_capacity(samples.len());
    for chunk in samples[..whole].chunks(frame) {
        out.extend(filter.process_frame(chunk));
    }
    out.extend(filter.finish(&samples[whole..]));
    (out, filter)
}
