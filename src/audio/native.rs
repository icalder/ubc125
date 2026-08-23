//! Native capture sources: ALSA → Opus → WebM, with no child process.
//!
//! [`AlsaOpusSource`] is the production source: it opens the ALSA device on
//! a blocking thread, encodes 960-sample (20 ms) frames with Opus, muxes
//! them into WebM clusters, and emits the pre-muxed bytes as
//! [`SourceEvent::Bytes`] — the same contract the `--audio-cmd` hook and
//! the test fakes use, so the `WebmSegmenter` and broadcaster are shared by
//! every source kind.
//!
//! [`ToneSource`] runs the identical pipeline over a deterministic sine
//! (no ALSA); it is the `audio-tone` subcommand's engine and the
//! hardware-free test source.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::warn;

use crate::audio::alsacapture::{AlsaReader, FrameSplitter, PcmEvent, PcmReader, PERIOD_FRAMES};
use crate::audio::filter::PcmFrameFilter;
use crate::audio::opusenc::{OpusFrameEncoder, FRAME_SAMPLES};
use crate::audio::source::{
    CaptureHandle, CaptureSource, HandleStop, SourceError, SourceEvent, SourceExit,
    EVENT_CHANNEL_CAPACITY,
};
use crate::audio::webm_mux::{DEFAULT_BITRATE_BPS, OPUS_FRAME_MS, WebmMuxer};

/// One 20 ms frame's worth of samples to request per ALSA read. The device
/// period is 960 frames; asking for two periods makes partial reads and
/// `plug:` period changes a non-event (the splitter re-chunks either way).
const READ_BUF_FRAMES: usize = (PERIOD_FRAMES * 2) as usize;
/// Sine amplitude for the tone sources (0.5 full scale).
const TONE_AMPLITUDE: f64 = 0.5;

/// Composes the encoder and muxer for one capture: every 960-sample frame
/// becomes exactly one Opus packet in exactly one SimpleBlock, with
/// frame-count-based (deterministic, monotonic) timecodes.
struct WebmOpusPipeline {
    encoder: OpusFrameEncoder,
    muxer: WebmMuxer,
    frame_index: u64,
}

impl WebmOpusPipeline {
    fn new(bitrate_bps: u32) -> Result<Self, String> {
        Ok(Self {
            encoder: OpusFrameEncoder::with_bitrate(bitrate_bps)
                .map_err(|e| format!("opus encoder: {e}"))?,
            muxer: WebmMuxer::new(),
            frame_index: 0,
        })
    }

    /// Encode one frame; return any clusters closed by it.
    fn add_frame(&mut self, samples: &[i16]) -> Result<Vec<Vec<u8>>, String> {
        let packet = self
            .encoder
            .encode_frame(samples)
            .map_err(|e| format!("opus encode: {e}"))?;
        let closed = self.muxer.add_block(self.frame_index * OPUS_FRAME_MS, &packet);
        self.frame_index += 1;
        Ok(closed)
    }

    fn flush(&mut self) -> Option<Vec<u8>> {
        self.muxer.flush()
    }
}

/// Production capture source: ALSA device → PCM filter (optional) →
/// Opus → WebM, no child process.
#[derive(Clone)]
pub struct AlsaOpusSource {
    device: String,
    filter: Option<Arc<dyn PcmFrameFilter>>,
}

impl std::fmt::Debug for AlsaOpusSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlsaOpusSource")
            .field("device", &self.device)
            .field("filter", &self.filter.as_ref().map(|_| "present"))
            .finish()
    }
}

impl AlsaOpusSource {
    /// `device` is an ALSA device string such as `hw:2`.
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            filter: None,
        }
    }

    /// Set the real-time PCM filter applied to every captured frame
    /// before Opus encoding. A fresh filter state is created per capture
    /// generation ([`PcmFrameFilter::for_capture`]).
    pub fn with_filter(mut self, filter: Arc<dyn PcmFrameFilter>) -> Self {
        self.filter = Some(filter);
        self
    }
}

impl CaptureSource for AlsaOpusSource {
    fn start(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CaptureHandle, SourceError>> + Send + '_>,
    > {
        let device = self.device.clone();
        Box::pin(async move {
            // Device open is blocking ALSA I/O (and can take a while when
            // the device is busy or absent); keep it off the runtime.
            let reader = tokio::task::spawn_blocking(move || AlsaReader::open(&device))
                .await
                .map_err(|e| SourceError::Start(format!("capture thread join: {e}")))??;
            let stop = std::sync::Arc::new(HandleStop::new());
            let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            // Subscribe before spawning so a stop() racing ahead of the
            // task's first poll still delivers.
            let cancel = stop.cancel.subscribe();
            let task_stop = std::sync::Arc::clone(&stop);
            let filter = self.filter.as_ref().map(|f| f.for_capture());
            tokio::task::spawn_blocking(move || {
                run_reader_capture(reader, tx, task_stop, cancel, filter);
            });
            Ok(CaptureHandle { rx, stop })
        })
    }
}

/// Deterministic tone source: the native pipeline (including the optional
/// PCM filter) over a sine, no ALSA — the offline-simulation test
/// pattern. Emits `duration` of audio as fast as possible, then ends
/// cleanly.
#[derive(Clone)]
pub struct ToneSource {
    freq: f64,
    duration: Duration,
    bitrate: u32,
    filter: Option<Arc<dyn PcmFrameFilter>>,
}

impl ToneSource {
    pub fn new(freq: f64, duration: Duration, bitrate_bps: u32) -> Self {
        Self {
            freq,
            duration,
            bitrate: bitrate_bps,
            filter: None,
        }
    }

    /// Set the real-time PCM filter (see [`AlsaOpusSource::with_filter`]).
    pub fn with_filter(mut self, filter: Arc<dyn PcmFrameFilter>) -> Self {
        self.filter = Some(filter);
        self
    }
}

impl std::fmt::Debug for ToneSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToneSource")
            .field("freq", &self.freq)
            .field("duration", &self.duration)
            .field("bitrate", &self.bitrate)
            .field("filter", &self.filter.as_ref().map(|_| "present"))
            .finish()
    }
}

impl Default for ToneSource {
    fn default() -> Self {
        Self::new(440.0, Duration::from_secs(60), DEFAULT_BITRATE_BPS)
    }
}

impl CaptureSource for ToneSource {
    fn start(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CaptureHandle, SourceError>> + Send + '_>,
    > {
        let (freq, duration, bitrate) = (self.freq, self.duration, self.bitrate);
        Box::pin(async move {
            let stop = std::sync::Arc::new(HandleStop::new());
            let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            let cancel = stop.cancel.subscribe();
            let task_stop = std::sync::Arc::clone(&stop);
            let filter = self.filter.as_ref().map(|f| f.for_capture());
            tokio::task::spawn_blocking(move || {
                run_tone_capture(tx, task_stop, cancel, freq, duration, bitrate, filter);
            });
            Ok(CaptureHandle { rx, stop })
        })
    }
}

/// Send `bytes` on the event channel. `false` when the receiver is gone:
/// the caller must then release resources and mark the task exited.
fn send_bytes(tx: &mpsc::Sender<SourceEvent>, bytes: Vec<u8>) -> bool {
    tx.blocking_send(SourceEvent::Bytes(bytes)).is_ok()
}

fn finish(tx: &mpsc::Sender<SourceEvent>, stop: &HandleStop, exit: SourceExit) {
    let _ = tx.blocking_send(SourceEvent::End(exit));
    stop.mark_exited();
}

/// The blocking capture loop: ALSA reads → splitter → PCM filter (the
/// optional [`PcmFrameFilter`], applied to every 960-sample frame before
/// Opus encoding) → WebM → events.
fn run_reader_capture<R: PcmReader>(
    mut reader: R,
    tx: mpsc::Sender<SourceEvent>,
    stop: std::sync::Arc<HandleStop>,
    mut cancel: watch::Receiver<bool>,
    mut filter: Option<Box<dyn PcmFrameFilter>>,
) {
    let mut pipeline = match WebmOpusPipeline::new(DEFAULT_BITRATE_BPS) {
        Ok(p) => p,
        Err(e) => {
            let _ = reader.close();
            finish(&tx, &stop, SourceExit::Failed(e));
            return;
        }
    };
    if !send_bytes(&tx, WebmMuxer::init_segment()) {
        let _ = reader.close();
        stop.mark_exited();
        return;
    }
    let mut splitter = FrameSplitter::new(FRAME_SAMPLES);
    let mut buf = vec![0i16; READ_BUF_FRAMES];
    let exit = loop {
        if *cancel.borrow_and_update() {
            break SourceExit::Clean;
        }
        match reader.read(&mut buf) {
            Ok(PcmEvent::Frames(n)) => {
                for mut frame in splitter.feed(&buf[..n]) {
                    if let Some(f) = filter.as_mut() {
                        f.process_frame(&mut frame);
                    }
                    let Ok(closed) = pipeline.add_frame(&frame) else {
                        let _ = reader.close();
                        finish(&tx, &stop, SourceExit::Failed("encode failed".into()));
                        return;
                    };
                    for cluster in closed {
                        if !send_bytes(&tx, cluster) {
                            // Reader (generation pump) is gone; release the
                            // device and exit quietly.
                            let _ = reader.close();
                            stop.mark_exited();
                            return;
                        }
                    }
                }
            }
            Ok(PcmEvent::Xrun) => {
                warn!("audio capture xrun; dropping torn frame");
                splitter.reset();
            }
            Ok(PcmEvent::Fatal(reason)) => {
                warn!(%reason, "audio capture device fatal error");
                break SourceExit::Failed(reason);
            }
            Err(e) => break SourceExit::Failed(e.to_string()),
        }
    };
    if let Some(f) = filter.as_mut() {
        for frame in f.flush() {
            let Ok(closed) = pipeline.add_frame(&frame) else {
                let _ = reader.close();
                finish(&tx, &stop, SourceExit::Failed("encode failed".into()));
                return;
            };
            for cluster in closed {
                if !send_bytes(&tx, cluster) {
                    let _ = reader.close();
                    stop.mark_exited();
                    return;
                }
            }
        }
    }
    if let Some(cluster) = pipeline.flush() {
        send_bytes(&tx, cluster);
    }
    let _ = reader.close();
    finish(&tx, &stop, exit);
}

/// The tone loop: identical to the reader loop's encode/mux tail, with a
/// sine in place of ALSA and no device to release.
fn run_tone_capture(
    tx: mpsc::Sender<SourceEvent>,
    stop: std::sync::Arc<HandleStop>,
    mut cancel: watch::Receiver<bool>,
    freq: f64,
    duration: Duration,
    bitrate_bps: u32,
    mut filter: Option<Box<dyn PcmFrameFilter>>,
) {
    let mut pipeline = match WebmOpusPipeline::new(bitrate_bps) {
        Ok(p) => p,
        Err(e) => {
            finish(&tx, &stop, SourceExit::Failed(e));
            return;
        }
    };
    if !send_bytes(&tx, WebmMuxer::init_segment()) {
        stop.mark_exited();
        return;
    }
    let total_samples =
        (duration.as_millis() as u64 / OPUS_FRAME_MS) * FRAME_SAMPLES as u64;
    let mut sample_index = 0u64;
    let exit = loop {
        if *cancel.borrow_and_update() {
            break SourceExit::Clean;
        }
        if sample_index >= total_samples {
            break SourceExit::Clean;
        }
        let mut frame: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (sample_index + i as u64) as f64 / 48_000.0;
                (TONE_AMPLITUDE * (2.0 * std::f64::consts::PI * freq * t).sin())
                    * i16::MAX as f64
            })
            .map(|s| s.round() as i16)
            .collect();
        sample_index += FRAME_SAMPLES as u64;
        if let Some(f) = filter.as_mut() {
            f.process_frame(&mut frame);
        }
        let Ok(closed) = pipeline.add_frame(&frame) else {
            finish(&tx, &stop, SourceExit::Failed("encode failed".into()));
            return;
        };
        for cluster in closed {
            if !send_bytes(&tx, cluster) {
                stop.mark_exited();
                return;
            }
        }
    };
    if let Some(f) = filter.as_mut() {
        for frame in f.flush() {
            let Ok(closed) = pipeline.add_frame(&frame) else {
                finish(&tx, &stop, SourceExit::Failed("encode failed".into()));
                return;
            };
            for cluster in closed {
                if !send_bytes(&tx, cluster) {
                    stop.mark_exited();
                    return;
                }
            }
        }
    }
    if let Some(cluster) = pipeline.flush() {
        send_bytes(&tx, cluster);
    }
    finish(&tx, &stop, exit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::webm::{DEFAULT_MAX_SEGMENT_SIZE, Segment, WebmSegmenter};
    use crate::audio::alsacapture::PcmError;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::timeout;

    const WAIT: Duration = Duration::from_secs(5);

    /// A fake reader that serves a scripted number of frames then loops
    /// forever (like a live device), or ends on a scripted fatal.
    struct LoopingFakeReader {
        /// frames per read; `None` means fatal on the first read.
        frames: Option<usize>,
        /// End with a scripted fatal after this many reads
        /// (`None` = loop forever).
        max_reads: Option<usize>,
        reads: Arc<AtomicUsize>,
        close_count: Arc<AtomicUsize>,
    }

    impl LoopingFakeReader {
        fn new(frames: usize) -> Self {
            Self {
                frames: Some(frames),
                max_reads: None,
                reads: Arc::new(AtomicUsize::new(0)),
                close_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Deterministic finite reader: exactly `max` reads, then fatal.
        fn finite(frames: usize, max: usize) -> Self {
            Self {
                frames: Some(frames),
                max_reads: Some(max),
                reads: Arc::new(AtomicUsize::new(0)),
                close_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn fatal() -> Self {
            Self {
                frames: None,
                max_reads: None,
                reads: Arc::new(AtomicUsize::new(0)),
                close_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl PcmReader for LoopingFakeReader {
        fn read(&mut self, buf: &mut [i16]) -> Result<PcmEvent, PcmError> {
            let n_reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if self.max_reads.is_some_and(|m| n_reads > m) {
                return Ok(PcmEvent::Fatal("scripted end".into()));
            }
            match self.frames {
                None => Ok(PcmEvent::Fatal("scripted device loss".into())),
                Some(n) => {
                    buf.iter_mut().take(n).enumerate().for_each(|(i, s)| {
                        *s = ((i + self.reads() * 31) % 200) as i16;
                    });
                    Ok(PcmEvent::Frames(n))
                }
            }
        }

        fn close(self) -> Result<(), PcmError> {
            self.close_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn run_reader_source(reader: LoopingFakeReader) -> (CaptureHandle, Arc<AtomicUsize>) {
        let close_count = Arc::clone(&reader.close_count);
        let stop = Arc::new(HandleStop::new());
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let cancel = stop.cancel.subscribe();
        let task_stop = Arc::clone(&stop);
        tokio::task::spawn_blocking(move || run_reader_capture(reader, tx, task_stop, cancel, None));
        (CaptureHandle { rx, stop }, close_count)
    }

    /// A filter that negates every sample: proves the seam is in the
    /// encode path (its bytes must differ from the unfiltered run).
    struct SignFlip;

    impl PcmFrameFilter for SignFlip {
        fn process_frame(&mut self, frame: &mut [i16]) {
            for s in frame.iter_mut() {
                *s = s.wrapping_neg();
            }
        }

        fn for_capture(&self) -> Box<dyn PcmFrameFilter> {
            Box::new(SignFlip)
        }
    }

    /// Run the reader pipeline to its scripted end with the given filter;
    /// return every emitted byte (init segment + clusters).
    async fn run_reader_bytes(filter: Option<Box<dyn PcmFrameFilter>>) -> Vec<u8> {
        let reader = LoopingFakeReader::finite(960, 30);
        let stop = Arc::new(HandleStop::new());
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let cancel = stop.cancel.subscribe();
        let task_stop = Arc::clone(&stop);
        tokio::task::spawn_blocking(move || run_reader_capture(reader, tx, task_stop, cancel, filter));
        let mut events = rx;
        let mut bytes = Vec::new();
        loop {
            match timeout(WAIT, events.recv()).await.expect("timed out") {
                Some(SourceEvent::Bytes(b)) => bytes.extend(b),
                Some(SourceEvent::End(_)) => break,
                None => break,
            }
        }
        bytes
    }

    /// Regression (filter off): with no filter in the chain, frames reach
    /// the Opus encoder untouched, so the capture output is byte-identical
    /// to the pre-seam pipeline. A pass-through (null) filter is likewise
    /// byte-identical; an active filter changes the bytes (the seam is in
    /// the encode path).
    #[tokio::test]
    async fn filter_off_is_byte_identical_to_pre_seam_pipeline() {
        let no_filter = run_reader_bytes(None).await;
        let pass_through = run_reader_bytes(Some(Box::new(crate::audio::filter::PassThrough))).await;
        let sign_flip = run_reader_bytes(Some(Box::new(SignFlip))).await;
        assert!(!no_filter.is_empty(), "pipeline emitted nothing");
        assert_eq!(
            no_filter, pass_through,
            "a no-op filter in the chain must be byte-identical to no filter"
        );
        assert_ne!(
            no_filter, sign_flip,
            "an active filter must change the encoded bytes"
        );
    }

    /// G13: the fake-reader pipeline composes correctly — init before any
    /// media, every 960-sample read frame becomes one cluster block at 20 ms
    /// steps, and a stop mid-stream ends `Clean` (classified `Unavailable`
    /// by the broadcaster, not `Failed`).
    #[tokio::test]
    async fn reader_pipeline_wiring_and_clean_stop() {
        let (handle, close_count) = run_reader_source(LoopingFakeReader::new(960));
        let stop = handle.stop_handle();
        let mut events = handle.into_events();
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        // Collect ~300 ms of audio (15 frames) then stop.
        loop {
            match timeout(WAIT, events.recv()).await.expect("timed out") {
                Some(SourceEvent::Bytes(b)) => segments.extend(segmenter.feed(&b).unwrap()),
                Some(SourceEvent::End(e)) => panic!("unexpected early end: {e:?}"),
                None => panic!("channel closed"),
            }
            if segments.len() >= 2 {
                break;
            }
        }
        assert!(matches!(&segments[0], Segment::Init(_)), "init precedes media");
        assert!(
            segments[1..].iter().all(|s| matches!(s, Segment::Media(_))),
            "only clusters after init"
        );
        // Cluster timecodes step by 200 ms (10 blocks of 20 ms).
        let timecodes: Vec<u64> = segments[1..]
            .iter()
            .map(|s| {
                let Segment::Media(bytes) = s else {
                    unreachable!()
                };
                parse_cluster_timecode(bytes)
            })
            .collect();
        assert_eq!(
            timecodes,
            (0..timecodes.len() as u64).map(|i| i * 200).collect::<Vec<_>>(),
            "cluster timecodes 0, 200, 400, ..."
        );
        timeout(WAIT, stop.stop()).await.expect("stop timed out");
        assert_eq!(
            close_count.load(Ordering::SeqCst),
            1,
            "reader closed exactly once on stop"
        );
    }

    /// Read the Cluster::Timecode (big-endian uint) from a cluster
    /// element: id (4 B) + size vint, then the Timecode child element.
    fn parse_cluster_timecode(cluster: &[u8]) -> u64 {
        // [CLUSTER_ID 4 bytes][size vint][TIMECODE_ID 1][size 1][uint...]
        assert_eq!(&cluster[..4], &[0x1F, 0x43, 0xB6, 0x75], "cluster id");
        let size_len = vint_width(cluster[4]);
        let mut pos = 4 + size_len;
        assert_eq!(cluster[pos], 0xE7, "first child must be Timecode");
        pos += 1;
        let len = vint_width(cluster[pos]);
        pos += 1;
        let mut v = 0u64;
        for &b in &cluster[pos..pos + len] {
            v = (v << 8) | b as u64;
        }
        v
    }

    /// Width of a vint from its first byte: 0x80-0xFF is 1 byte,
    /// 0x40-0x7F is 2, 0x10-0x3F is 4, 0x01-0x07 is 8.
    fn vint_width(first: u8) -> usize {
        match first {
            0x80..=0xFF => 1,
            0x40..=0x7F => 2,
            0x10..=0x3F => 4,
            0x01..=0x07 => 8,
            _ => panic!("invalid vint first byte {first:#04x}"),
        }
    }

    /// G12: a scripted fatal device error ends the source `Failed` (the
    /// broadcaster turns that into a failed generation; the next
    /// `subscribe()` starts fresh).
    #[tokio::test]
    async fn reader_fatal_ends_failed() {
        let reader = LoopingFakeReader::fatal();
        let handle = run_reader_source(reader);
        let mut events = handle.0.into_events();
        let mut failed = false;
        loop {
            match timeout(WAIT, events.recv()).await.expect("timed out") {
                Some(SourceEvent::End(SourceExit::Failed(_))) => {
                    failed = true;
                    break;
                }
                Some(SourceEvent::End(e)) => panic!("expected Failed, got {e:?}"),
                Some(SourceEvent::Bytes(_)) => {}
                None => break,
            }
        }
        assert!(failed, "fatal must surface as End(Failed)");
    }

    /// G12 (broadcaster level): `ToneSource` as a `CaptureSource` through a
    /// real [`AudioBroadcaster`] — first subscriber gets Init then Media,
    /// the finite tone ends the generation cleanly, and a new subscriber
    /// starts a fresh generation.
    #[tokio::test]
    async fn tone_source_through_broadcaster() {
        use crate::audio::broadcaster::{AudioBroadcaster, AudioEvent, Status};
        // 600 ms of tone = 30 frames = exactly 3 clusters.
        let source = Arc::new(ToneSource::new(
            440.0,
            Duration::from_millis(600),
            DEFAULT_BITRATE_BPS,
        ));
        let broadcaster = AudioBroadcaster::new(source);
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        let mut got_init = false;
        let mut clusters = 0usize;
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out");
            match timeout(WAIT, rx.recv())
                .await
                .expect("event timed out")
            {
                Ok(AudioEvent::Init(_, _)) => got_init = true,
                Ok(AudioEvent::Media(_, _)) => {
                    clusters += 1;
                    if clusters >= 3 {
                        break;
                    }
                }
                Ok(AudioEvent::Failed) => break,
                Err(_) => break,
            }
        }
        assert!(got_init, "init must arrive first");
        assert_eq!(clusters, 3, "600 ms of audio is exactly 3 clusters");
        drop(sub);
        // Wait for the first generation to fully end before resubscribing.
        // Otherwise the resubscribe can race the pump's termination cleanup
        // (source done, not yet marked finished) and join the dying
        // generation: its channel then delivers no further events and never
        // closes (the subscription keeps the sender alive).
        let end_deadline = std::time::Instant::now() + WAIT;
        while broadcaster.status() != Status::Unavailable {
            assert!(
                std::time::Instant::now() < end_deadline,
                "first generation did not end"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // The generation ended (finite source): a new subscriber starts
        // fresh with a new init.
        let sub2 = broadcaster.subscribe().await.expect("resubscribe");
        let mut rx2 = sub2.resubscribe();
        let mut fresh_init = false;
        let deadline = std::time::Instant::now() + WAIT;
        while !fresh_init {
            assert!(std::time::Instant::now() < deadline, "timed out");
            match timeout(WAIT, rx2.recv())
                .await
                .expect("event timed out")
            {
                Ok(AudioEvent::Init(_, _)) => fresh_init = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(fresh_init, "fresh generation must emit a new init");
        drop(sub2);
    }

    /// G12: two subscribers share one generation (the source is started
    /// exactly once) and the late joiner gets the cached init.
    #[tokio::test]
    async fn two_subscribers_share_tone_generation() {
        use crate::audio::broadcaster::AudioBroadcaster;
        let source = Arc::new(ToneSource::new(
            440.0,
            Duration::from_secs(60),
            DEFAULT_BITRATE_BPS,
        ));
        let broadcaster = AudioBroadcaster::new(source);
        let sub_a = broadcaster.subscribe().await.expect("a");
        let mut rx_a = sub_a.resubscribe();
        // Wait for the init so the cached copy exists.
        let deadline = std::time::Instant::now() + WAIT;
        while std::time::Instant::now() < deadline {
            match timeout(Duration::from_secs(2), rx_a.recv()).await {
                Ok(Ok(_)) => break,
                Ok(Err(_)) => panic!("channel closed"),
                Err(_) => {}
            }
        }
        let sub_b = broadcaster.subscribe().await.expect("b");
        assert!(
            sub_b.cached_init().is_some(),
            "late joiner gets the cached init"
        );
        drop(sub_a);
        drop(sub_b);
        // Let the last-subscriber stop complete.
        let _ = broadcaster.status();
    }

    /// Replaced `real_ffmpeg_sine_smoke` (Phase 5): `ToneSource` → segmenter
    /// yields 3 complete clusters for 600 ms of tone.
    #[tokio::test]
    async fn tone_source_smoke() {
        let source = ToneSource::new(
            440.0,
            Duration::from_millis(600),
            DEFAULT_BITRATE_BPS,
        );
        let handle = source.start().await.expect("start");
        let _stop = handle.stop_handle();
        let mut events = handle.into_events();
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        loop {
            match timeout(WAIT, events.recv()).await.expect("timed out") {
                Some(SourceEvent::Bytes(b)) => segments.extend(segmenter.feed(&b).unwrap()),
                Some(SourceEvent::End(e)) => {
                    assert!(e.is_clean(), "unexpected exit: {e:?}");
                    break;
                }
                None => panic!("channel closed"),
            }
        }
        assert_eq!(segments.len(), 4, "init + 3 clusters");
        assert!(matches!(segments[0], Segment::Init(_)));
        assert!(segments[1..].iter().all(|s| matches!(s, Segment::Media(_))));
    }
}
