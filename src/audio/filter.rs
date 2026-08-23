//! The real-time PCM filter seam in the capture pipeline.
//!
//! A [`PcmFrameFilter`] is applied to every 960-sample (20 ms) i16 frame
//! after the capture read and **before** Opus encoding (see
//! [`crate::audio::native`]). Filters may be stateful and may add
//! latency: the output frame is not required to be a pure function of the
//! input frame (a filter may buffer and emit delayed output).
//!
//! One filter instance per capture generation: the source calls
//! [`PcmFrameFilter::for_capture`] when a capture starts and the returned
//! instance is owned by that capture's reader thread, so all filter state
//! is fresh per generation and zero-padded at startup.

/// A real-time filter for 960-sample (20 ms, 48 kHz) i16 PCM frames.
pub trait PcmFrameFilter: Send + Sync {
    /// Process one frame in place. The frame is replaced by the filter's
    /// output for it (which may be delayed or attenuated); the frame
    /// length is unchanged.
    fn process_frame(&mut self, frame: &mut [i16]);

    /// Return frames held by a stateful filter at the end of a capture.
    ///
    /// The default is correct for filters without latency. A filter that
    /// delays input must override this method, otherwise its final buffered
    /// samples would be lost when a finite capture ends.
    fn flush(&mut self) -> Vec<Vec<i16>> {
        Vec::new()
    }

    /// A fresh-state instance for a new capture generation.
    fn for_capture(&self) -> Box<dyn PcmFrameFilter>;
}

/// Null filter: frames pass through untouched. Serves as the explicit
/// "filter in the chain but doing nothing" case (byte-identical to no
/// filter at all — see the seam regression test in
/// [`crate::audio::native`]).
#[derive(Debug, Default, Clone)]
pub struct PassThrough;

impl PcmFrameFilter for PassThrough {
    fn process_frame(&mut self, _frame: &mut [i16]) {}

    fn for_capture(&self) -> Box<dyn PcmFrameFilter> {
        Box::new(PassThrough)
    }
}
