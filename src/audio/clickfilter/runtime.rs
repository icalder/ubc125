//! The production seam: the contract `../ubc125-ml/docs/deployment.md` hands to
//! the parent, expressed against the parent's own
//! [`crate::audio::filter::PcmFrameFilter`] trait (the duplicate trait the
//! standalone rig defined here is dropped in the port).
//!
//! The reference rig releases samples at the position the fixed output delay
//! frees, which is a variable-length reply per call. ALSA wants a frame back for
//! every frame in, so [`InPlaceDeClick`] wraps the released-position logic in a
//! fixed 960-sample writer: the stream trails the input by `delay`, so its first
//! chunk is silence and its second is `delay - frame` silence plus the samples
//! the first release freed. Nothing else changes: the audio, the delay and the
//! refusals are the ones the offline rig measures.

use std::collections::VecDeque;

use crate::audio::clickfilter::config::Config;
use crate::audio::clickfilter::filter::{ClickFilter, Metrics};
use crate::audio::filter::PcmFrameFilter;

/// [`ClickFilter`] behind the production frame contract.
pub struct InPlaceDeClick {
    inner: ClickFilter,
    /// Released samples waiting for the next frame's write slot.
    pending: VecDeque<i16>,
    /// Silence the fixed output delay still owes at the head of the stream.
    leading_silence: i64,
    /// Calls that wanted more samples than the delay line could give: must stay 0.
    underruns: i64,
}

impl InPlaceDeClick {
    pub fn new(cfg: &Config) -> Self {
        InPlaceDeClick {
            inner: ClickFilter::new(cfg),
            pending: VecDeque::new(),
            leading_silence: cfg.delay(),
            underruns: 0,
        }
    }

    pub fn config(&self) -> &Config {
        self.inner.config()
    }

    pub fn metrics(&self) -> &Metrics {
        self.inner.metrics()
    }

    pub fn events(&self) -> &[crate::audio::clickfilter::filter::EventRecord] {
        self.inner.events()
    }

    pub fn underruns(&self) -> i64 {
        self.underruns
    }

    /// Write `want` samples: leading silence first, then the delay line.
    fn drain_into(&mut self, want: usize, out: &mut [i16]) {
        let mut written = 0;
        let silence = (self.leading_silence as usize).min(want);
        if silence > 0 {
            out[..silence].fill(0);
            self.leading_silence -= silence as i64;
            written = silence;
        }
        while written < want {
            match self.pending.pop_front() {
                Some(sample) => {
                    out[written] = sample;
                    written += 1;
                }
                None => {
                    // The delay line cannot be short: the reference releases one
                    // frame per frame ingested once the head start is paid, and
                    // `test_frame_sized_output_matches_the_offline_stream` pins
                    // it. Fill with silence and count it so a future change that
                    // breaks the schedule shows up as a number, not a gap.
                    out[written] = 0;
                    written += 1;
                    self.underruns += 1;
                }
            }
        }
    }
}

impl PcmFrameFilter for InPlaceDeClick {
    fn process_frame(&mut self, frame: &mut [i16]) {
        assert_eq!(
            frame.len(),
            self.inner.config().frame(),
            "the production seam takes one full frame"
        );
        let input = frame.to_vec();
        let released = self.inner.process_frame(&input);
        self.pending.extend(released);
        self.drain_into(frame.len(), frame);
    }

    fn flush(&mut self) -> Vec<Vec<i16>> {
        let released = self.inner.flush();
        self.pending.extend(released);
        let frame = self.inner.config().frame();
        let mut out = Vec::new();
        while !self.pending.is_empty() || self.leading_silence > 0 {
            let want = frame.min(self.pending.len() + self.leading_silence as usize);
            let mut chunk = vec![0i16; want];
            self.drain_into(want, &mut chunk);
            out.push(chunk);
        }
        out
    }

    fn for_capture(&self) -> Box<dyn PcmFrameFilter> {
        Box::new(InPlaceDeClick::new(self.inner.config()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::clickfilter::constants::{ClickClass, FRAME, Policy};
    use crate::audio::clickfilter::filter::run_filter;

    fn dirty(onset: usize, run_len: usize, total: usize) -> Vec<i16> {
        let mut x: Vec<i16> = (0..total)
            .map(|i| {
                (0.4 * 32767.0 * (2.0 * std::f64::consts::PI * 50.0 * i as f64 / 48000.0).sin())
                    as i16
            })
            .collect();
        x[onset..onset + run_len].fill(-32768);
        x
    }

    fn selected_config() -> Config {
        Config::builder()
            .policy(Policy::Interp)
            .policy_override(ClickClass::Long, Policy::Descend)
            .tail_ms(ClickClass::Long, 150.0)
            .build()
    }

    fn run_in_place(cfg: &Config, samples: &[i16]) -> Vec<i16> {
        let mut filter = InPlaceDeClick::new(cfg);
        let mut out = Vec::new();
        for frame in samples.as_chunks::<FRAME>().0 {
            let mut buffer = frame.to_vec();
            filter.process_frame(&mut buffer);
            out.extend(buffer);
        }
        for chunk in filter.flush() {
            out.extend(chunk);
        }
        out
    }

    #[test]
    fn frame_sized_output_matches_the_offline_stream() {
        // The in-place stream is the offline stream plus the delay's head of
        // silence: same samples, same positions, no underrun.
        let cfg = selected_config();
        let x = dirty(2000, 150, 20 * FRAME);
        let (offline, _) = run_filter(&cfg, &x);
        let live = run_in_place(&cfg, &x);
        assert_eq!(live.len(), offline.len() + cfg.delay() as usize);
        assert_eq!(
            &live[..cfg.delay() as usize],
            vec![0i16; cfg.delay() as usize].as_slice()
        );
        assert_eq!(&live[cfg.delay() as usize..], offline.as_slice());
    }

    #[test]
    fn first_chunks_are_silence_then_936_real_samples() {
        // The schedule ../ubc125-ml/docs/deployment.md describes: 960 silence, then 24
        // silence plus the 936 samples the second frame releases.
        let cfg = selected_config();
        assert_eq!(cfg.delay(), 984);
        let mut filter = InPlaceDeClick::new(&cfg);
        let mut first = vec![7i16; FRAME];
        filter.process_frame(&mut first);
        assert_eq!(first, vec![0i16; FRAME], "the first chunk is pure silence");
        let mut second = vec![7i16; FRAME];
        filter.process_frame(&mut second);
        assert_eq!(&second[..24], vec![0i16; 24].as_slice());
        // The remaining 936 slots hold the audio the second frame released.
        assert_eq!(&second[24..], vec![7i16; FRAME - 24].as_slice());
        assert_eq!(filter.underruns(), 0);
    }

    #[test]
    fn for_capture_starts_clean() {
        let cfg = selected_config();
        let mut used = InPlaceDeClick::new(&cfg);
        let mut buffer = vec![0i16; FRAME];
        used.process_frame(&mut buffer);
        let mut fresh = used.for_capture();
        let mut probe = vec![-32768i16; FRAME];
        fresh.process_frame(&mut probe);
        // A fresh filter has seen one frame and releases nothing yet.
        assert_eq!(probe, vec![0i16; FRAME]);
    }

    #[test]
    fn the_seam_is_send_and_sync_for_the_parent() {
        // `PcmFrameFilter: Send + Sync` is part of the contract in
        // ../ubc125-ml/docs/deployment.md: the parent moves the box between audio threads, and
        // builds one per capture with for_capture.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InPlaceDeClick>();
        let cfg = selected_config();
        let filter: Box<dyn PcmFrameFilter> = Box::new(InPlaceDeClick::new(&cfg));
        let handle = std::thread::spawn(move || {
            let mut fresh = filter.for_capture();
            let mut frame = vec![3i16; FRAME];
            fresh.process_frame(&mut frame);
            // The frame is filled in place and still holds the delay's silence,
            // so the box moved here behaves like the one built on this thread.
            frame.into_iter().sum::<i16>() as i64
        });
        assert_eq!(handle.join().expect("thread panicked"), 0);
    }

    #[test]
    fn flush_emits_every_held_sample_once() {
        let cfg = Config::default();
        let x = dirty(3000, 67, 5 * FRAME);
        let live = run_in_place(&cfg, &x);
        let mut filter = ClickFilter::new(&cfg);
        let mut seen = 0i64;
        for frame in x.as_chunks::<FRAME>().0 {
            seen += filter.process_frame(frame).len() as i64;
        }
        seen += filter.flush().len() as i64;
        assert_eq!(seen, x.len() as i64);
        assert_eq!(live.len() as i64, x.len() as i64 + cfg.delay());
        assert_eq!(filter.metrics().late_writes, 0);
    }
}
