//! Live audio broadcast: one capture generation at a time, fanned out to any
//! number of gRPC subscribers.
//!
//! Lifecycle:
//! - the first `subscribe()` starts the capture source and the pump task;
//! - further subscribers join the same generation and are handed the cached
//!   init segment so they can build a fresh `MediaSource`;
//! - when the last subscriber goes away the generation is stopped, with a
//!   short grace window in which a rejoining subscriber cancels the stop;
//! - a source failure or clean end finishes the generation: subscribers see
//!   [`AudioEvent::Failed`] (or a closed channel) and a later `subscribe()`
//!   starts fresh.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::audio::source::{CaptureSource, SourceError, SourceEvent, SourceExit, StopHandle};
use crate::audio::stats::SharedAudioStats;
use crate::audio::webm::{
    cluster_duration_ms, DEFAULT_MAX_SEGMENT_SIZE, Segment, WebmSegmenter,
};

/// B5: default max chunks one subscriber may buffer ahead of the pump
/// before the oldest are dropped (8 × 60 ms ≈ 480 ms ceiling; was 64 ×
/// 200 ms ≈ 12.8 s). `--audio-subscriber-queue`.
pub const DEFAULT_SUBSCRIBER_QUEUE: usize = 8;
/// Grace window between the last subscriber leaving and the child being
/// killed; a subscriber joining inside it cancels the stop.
const STOP_JOIN_GRACE: Duration = Duration::from_millis(100);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One item on the audio broadcast channel.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// The WebM init segment (EBML + Segment header + Info + Tracks). Every
    /// client must see this before its first media chunk.
    Init(Vec<u8>, i64),
    /// One complete WebM cluster, ready for `SourceBuffer.appendBuffer`.
    Media(Vec<u8>, i64),
    /// The generation has ended (source failure, clean end, or segmenter
    /// error). The channel closes shortly after.
    Failed,
}

/// Errors from [`AudioBroadcaster::subscribe`].
#[derive(Debug)]
pub enum AudioError {
    /// The broadcaster has been shut down; no new captures will start.
    Shutdown,
    /// The capture source failed to start.
    Source(SourceError),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shutdown => write!(f, "audio broadcaster is shut down"),
            Self::Source(e) => write!(f, "{e}"),
        }
    }
}

impl Error for AudioError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(e) => Some(e),
            _ => None,
        }
    }
}

/// Capture state, used for logging only (there is no public status RPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No generation; the broadcaster is fresh or was shut down.
    Idle,
    /// A generation is running but has not produced a media segment yet.
    Starting,
    /// Media segments are flowing.
    Capturing,
    /// The last generation ended cleanly (e.g. its file source finished).
    Unavailable,
    /// The last generation failed (source error or malformed stream).
    Failed,
}

/// One active capture: the shared broadcast sender, cached init segment,
/// stop handle, and subscriber accounting.
struct Active {
    id: u64,
    sender: broadcast::Sender<AudioEvent>,
    cached_init: Option<Vec<u8>>,
    stop: StopHandle,
    subscribers: usize,
    stopping: bool,
    /// The source has ended and the generation must not accept new
    /// subscribers while its stop is being awaited.
    finished: bool,
}

struct State {
    status: Status,
    shutdown: bool,
    generation: Option<Active>,
}

struct Inner {
    source: Arc<dyn CaptureSource>,
    /// Serializes generation creation so concurrent `subscribe()` calls
    /// never start two capture processes.
    create: tokio::sync::Mutex<()>,
    state: StdMutex<State>,
    next_id: AtomicU64,
    /// B10 pipeline counters, shared with the capture source (xruns, source
    /// channel stalls) and the 5-second reporter.
    stats: SharedAudioStats,
    /// B5: per-subscriber broadcast capacity (`--audio-subscriber-queue`),
    /// read when each generation starts.
    subscriber_queue: AtomicUsize,
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn status(&self) -> Status {
        self.state().status
    }
}

/// Fans one capture generation out to any number of subscribers.
#[derive(Clone)]
pub struct AudioBroadcaster {
    inner: Arc<Inner>,
}

impl AudioBroadcaster {
    /// `source` is started lazily on the first subscriber and restarted for
    /// every new generation. Tests get their counters here; production shares
    /// one handle with the capture source via [`Self::with_stats`].
    #[cfg(test)]
    pub fn new(source: Arc<dyn CaptureSource>) -> Self {
        Self::with_stats(source, std::sync::Arc::new(crate::audio::stats::AudioStats::new()))
    }

    /// Create a broadcaster whose pipeline counters live in `stats` (the
    /// serve command passes the same handle to the capture source so the
    /// xrun / channel-stall counters land in one place).
    pub fn with_stats(source: Arc<dyn CaptureSource>, stats: SharedAudioStats) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                create: tokio::sync::Mutex::new(()),
                state: StdMutex::new(State {
                    status: Status::Idle,
                    shutdown: false,
                    generation: None,
                }),
                next_id: AtomicU64::new(1),
                stats,
                subscriber_queue: AtomicUsize::new(DEFAULT_SUBSCRIBER_QUEUE),
            }),
        }
    }

    /// B5: set the per-subscriber broadcast capacity (call before the first
    /// subscriber; the capacity is read when each generation starts).
    pub fn with_subscriber_queue(self, capacity: usize) -> Self {
        self.inner
            .subscriber_queue
            .store(capacity.max(1), Ordering::Relaxed);
        self
    }

    /// The pipeline's B10 counters (shared with the capture source).
    pub fn stats(&self) -> SharedAudioStats {
        Arc::clone(&self.inner.stats)
    }

    /// B10: register a gRPC listener with the shared stats; the returned
    /// id is used to record its `Lagged` drops and its removal when the
    /// stream ends.
    pub fn register_subscriber(&self) -> u64 {
        self.inner.stats.subscriber_started()
    }

    /// Wait (up to `deadline`) for `gen_id`'s init segment to be cached,
    /// returning a copy.
    ///
    /// The first subscriber's `cached_init` is `None` at `subscribe()` time:
    /// the pump caches the init a moment later, after the source emits its
    /// header. The one-time `Init` channel event cannot be the fallback — a
    /// fast source can drop it out of the bounded channel before a slow
    /// client's receiver reads it, leaving that client without an init and
    /// stuck in "connecting". Fetching the init from the cache is reliable:
    /// the pump caches it before it sends any media, so by the time media is
    /// flowing the cache is populated.
    pub async fn wait_for_init(&self, gen_id: u64, deadline: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + deadline;
        loop {
            let (init, gen_present) = {
                let state = self.inner.state();
                match state.generation.as_ref() {
                    Some(a) if a.id == gen_id => (a.cached_init.clone(), true),
                    _ => (None, false),
                }
            };
            if let Some(init) = init {
                return Some(init);
            }
            // The generation ended before producing an init (a broken
            // source): stop waiting; the caller falls back to the channel,
            // which will surface the failure.
            if !gen_present || Instant::now() >= end {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Current capture state (for logs and tests).
    #[allow(dead_code)]
    pub fn status(&self) -> Status {
        self.inner.status()
    }

    /// Join the current generation, starting one if none is running.
    ///
    /// The returned subscription carries the cached init segment (if the
    /// generation already produced one). Dropping it removes the subscriber;
    /// when the count reaches zero the generation is stopped.
    pub async fn subscribe(&self) -> Result<AudioSubscription, AudioError> {
        // Held for the whole join-or-create so two concurrent subscribers
        // cannot each start a capture.
        let _creating = self.inner.create.lock().await;
        if self.inner.state().shutdown {
            return Err(AudioError::Shutdown);
        }
        {
            let mut state = self.inner.state();
            if let Some(active) = &mut state.generation && !active.finished {
                active.subscribers += 1;
                active.stopping = false;
                debug!(gen_id = active.id, "audio subscriber joined running generation");
                return Ok(AudioSubscription {
                    sender: active.sender.clone(),
                    cached_init: active.cached_init.clone(),
                    inner: Arc::clone(&self.inner),
                    gen_id: active.id,
                });
            }
        }

        // A source can finish before its pump has completed cleanup. Do not
        // join that dead generation: wait for its stop handle so resources
        // (in particular the ALSA device) are released, then start fresh.
        let finished = {
            let state = self.inner.state();
            state
                .generation
                .as_ref()
                .filter(|active| active.finished)
                .map(|active| (active.id, active.stop.clone()))
        };
        if let Some((gen_id, stop)) = finished {
            stop.stop().await;
            let mut state = self.inner.state();
            if state.generation.as_ref().is_some_and(|active| active.id == gen_id) {
                state.generation = None;
                set_status_locked(&mut state, Status::Unavailable);
            }
        }
        self.start_generation().await
    }

    /// Stop the current capture generation: kill the capture process, await
    /// its exit (releasing any device it holds), and clear the generation.
    /// Listeners see a `Failed` event and a closed channel; a later
    /// `subscribe()` starts fresh. No-op when idle.
    ///
    /// Needed because a stopped gRPC listener does not reliably close the
    /// underlying connection (keep-alive pooling), so the
    /// last-subscriber stop alone cannot be trusted to release the device.
    pub async fn stop_capture(&self) {
        let (gen_id, stop) = {
            let mut state = self.inner.state();
            let Some(active) = state.generation.as_mut() else {
                return;
            };
            // Mark stopping so the pump classifies the kill as an
            // Unavailable exit rather than a failure.
            active.stopping = true;
            (active.id, active.stop.clone())
        };
        debug!(gen_id, "audio generation stopped (StopCapture)");
        stop.stop().await;
        // Id-gated: if the generation already ended (and a new one started),
        // leave the newer one alone.
        let mut state = self.inner.state();
        if state.generation.as_ref().is_some_and(|a| a.id == gen_id) {
            state.generation = None;
            set_status_locked(&mut state, Status::Unavailable);
        }
    }

    /// Stop the capture (if any), await the child's exit, and refuse all
    /// further subscribers.
    pub async fn shutdown(&self) {
        let stop = {
            let mut state = self.inner.state();
            state.shutdown = true;
            state.status = Status::Idle;
            state.generation.take().map(|active| active.stop)
        };
        if let Some(stop) = stop {
            stop.stop().await;
        }
    }

    async fn start_generation(&self) -> Result<AudioSubscription, AudioError> {
        let handle = match self.inner.source.start().await {
            Ok(handle) => handle,
            Err(e) => {
                info!(error = %e, "audio capture failed to start");
                set_status(&self.inner, None, Status::Failed);
                return Err(AudioError::Source(e));
            }
        };
        let (sender, _initial_receiver) =
            broadcast::channel(self.inner.subscriber_queue.load(Ordering::Relaxed));
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let started_at = Instant::now();
        let stop = handle.stop_handle();
        {
            let mut state = self.inner.state();
            state.generation = Some(Active {
                id,
                sender: sender.clone(),
                cached_init: None,
                stop,
                subscribers: 1,
                stopping: false,
                finished: false,
            });
        }
        info!(gen_id = id, "audio capture started");
        set_status(&self.inner, Some(id), Status::Starting);
        tokio::spawn(run_generation(
            Arc::clone(&self.inner),
            id,
            started_at,
            sender.clone(),
            handle.into_events(),
        ));
        Ok(AudioSubscription {
            sender,
            cached_init: None,
            inner: Arc::clone(&self.inner),
            gen_id: id,
        })
    }
}

/// A single subscriber's view of a generation.
///
/// Dropping it decrements the generation's subscriber count; the last
/// subscriber triggers the (grace-windowed) stop.
pub struct AudioSubscription {
    /// Keeps the channel (and the cached init) alive; also the source of
    /// fresh receivers. A `Receiver` is deliberately not stored here: an
    /// unpolled receiver would stall the broadcast channel once its buffer
    /// fills.
    sender: broadcast::Sender<AudioEvent>,
    cached_init: Option<Vec<u8>>,
    inner: Arc<Inner>,
    gen_id: u64,
}

impl AudioSubscription {
    /// The init segment cached by this generation, for clients joining after
    /// it started.
    pub fn cached_init(&self) -> Option<&[u8]> {
        self.cached_init.as_deref()
    }

    /// The id of the generation this subscription belongs to.
    pub fn gen_id(&self) -> u64 {
        self.gen_id
    }

    /// A fresh receiver bound to the generation's event channel, also
    /// delivering events still in the channel buffer. The subscription
    /// itself stays alive for as long as the subscriber count must stay
    /// positive.
    pub fn resubscribe(&self) -> broadcast::Receiver<AudioEvent> {
        self.sender.subscribe()
    }
}

impl Drop for AudioSubscription {
    fn drop(&mut self) {
        let inner = Arc::clone(&self.inner);
        let gen_id = self.gen_id;
        let is_last = {
            let mut state = inner.state();
            let shutdown = state.shutdown;
            let Some(active) = state.generation.as_mut().filter(|a| a.id == gen_id) else {
                return;
            };
            active.subscribers = active.subscribers.saturating_sub(1);
            debug!(gen_id, remaining = active.subscribers, "audio subscriber dropped");
            let is_last = active.subscribers == 0 && !shutdown;
            if is_last {
                active.stopping = true;
            }
            is_last
        };
        if !is_last {
            return;
        }
        // Synchronous drop: the spawned task performs the awaited stop.
        let Some(handle) = tokio::runtime::Handle::try_current().ok() else {
            warn!(
                "last audio subscriber dropped outside a runtime; capture will end when the source does"
            );
            return;
        };
        handle.spawn(run_stop_task(Arc::clone(&inner), gen_id));
    }
}

/// Pump one generation: source bytes → segmenter → broadcast events.
async fn run_generation(
    inner: Arc<Inner>,
    gen_id: u64,
    started_at: Instant,
    sender: broadcast::Sender<AudioEvent>,
    mut events: mpsc::Receiver<SourceEvent>,
) {
    let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
    let mut exit = None;
    'gen_loop: while let Some(event) = events.recv().await {
        match event {
            SourceEvent::Bytes(bytes) => {
                let segments = match segmenter.feed(&bytes) {
                    Ok(segments) => segments,
                    Err(e) => {
                        warn!(error = %e, "audio segmenter error; ending generation");
                        exit = Some(GenerationExit::Failed);
                        break 'gen_loop;
                    }
                };
                for segment in segments {
                    let event = match segment {
                        Segment::Init(bytes) => {
                            cache_init(&inner, gen_id, bytes.clone());
                            AudioEvent::Init(bytes, elapsed_ms(started_at))
                        }
                        Segment::Media(bytes) => {
                            // B10: count what the pump emits — chunk count
                            // plus the cluster's own duration, read from its
                            // blocks (0 when unparseable: the chunk still
                            // counts, the duration sums skip it).
                            let duration_ms = cluster_duration_ms(&bytes).unwrap_or(0);
                            inner.stats.record_chunk(duration_ms);
                            debug!(
                                bytes = bytes.len(),
                                cluster_ms = duration_ms,
                                "audio chunk emitted"
                            );
                            set_status(&inner, Some(gen_id), Status::Capturing);
                            AudioEvent::Media(bytes, elapsed_ms(started_at))
                        }
                    };
                    if sender.send(event).is_err() {
                        // No consumer receivers left. The last subscriber's
                        // drop guard has started the grace-windowed stop;
                        // keep pumping (discarding output) so a rejoiner
                        // inside the grace window finds a live generation.
                        continue 'gen_loop;
                    }
                }
            }
            SourceEvent::End(source_exit) => {
                exit = Some(classify_end(&inner, gen_id, source_exit));
                break;
            }
        }
    }
    // Publish the terminal state before sending Failed. A subscriber can
    // receive Failed and immediately subscribe again; it must not attach to
    // this generation while cleanup is still in progress.
    mark_finished(&inner, gen_id);
    let _ = sender.send(AudioEvent::Failed);
    // Always clear the generation: if the source task panicked without
    // sending an `End` event, `exit` is still `None`, and skipping this
    // would leave a dead generation that a rejoiner latches onto (its pump
    // is gone, so the client would reconnect in a loop).
    end_generation(&inner, gen_id, exit.unwrap_or(GenerationExit::Failed)).await;
    // Drop the sender so any remaining receivers see a closed channel.
}

#[derive(Debug)]
enum GenerationExit {
    /// Clean end or user stop: status becomes [`Status::Unavailable`].
    Unavailable,
    /// Failure: status becomes [`Status::Failed`].
    Failed,
}

/// Decide the terminal status for a source exit, considering whether this
/// generation is being stopped by its (now-absent) subscribers.
fn classify_end(inner: &Inner, gen_id: u64, source_exit: SourceExit) -> GenerationExit {
    let stopping = match inner.state().generation.as_ref() {
        Some(a) if a.id == gen_id => a.stopping,
        // Already cleared (user stop or shutdown): the non-zero exit caused
        // by our own kill is not a failure.
        _ => return GenerationExit::Unavailable,
    };
    if stopping || source_exit.is_clean() {
        return GenerationExit::Unavailable;
    }
    // Neither stopping nor clean: a genuine source failure.
    let SourceExit::Failed(reason) = source_exit else {
        // Logically unreachable (clean exits are handled above); degrade to
        // the semantically correct status instead of panicking the audio
        // pump task.
        return GenerationExit::Unavailable;
    };
    warn!(%reason, "audio capture failed");
    GenerationExit::Failed
}

/// Kill (if needed) and clear a finished generation. Id-gated: if a newer
/// generation already exists, or the stop task already cleared this one, this
/// is a no-op.
async fn end_generation(inner: &Inner, gen_id: u64, exit: GenerationExit) {
    let stop = inner
        .state()
        .generation
        .as_ref()
        .filter(|a| a.id == gen_id)
        .map(|a| a.stop.clone());
    if let Some(stop) = stop {
        stop.stop().await;
    }
    let mut state = inner.state();
    if state.generation.as_ref().is_some_and(|a| a.id == gen_id) {
        state.generation = None;
        set_status_locked(
            &mut state,
            match exit {
                GenerationExit::Unavailable => Status::Unavailable,
                GenerationExit::Failed => Status::Failed,
            },
        );
    }
}

/// Stop task spawned when the last subscriber leaves: give a rejoining
/// subscriber a grace window to cancel the stop, then kill the child and
/// clear the generation.
async fn run_stop_task(inner: Arc<Inner>, gen_id: u64) {
    let deadline = Instant::now() + STOP_JOIN_GRACE;
    loop {
        let (stopping, subscribers) = {
            let state = inner.state();
            match state.generation.as_ref() {
                Some(a) if a.id == gen_id => (a.stopping, a.subscribers),
                // The generation already ended on its own; nothing to stop.
                _ => return,
            }
        };
        // A joiner reset the flag or showed up: abort, no kill.
        if !stopping || subscribers > 0 {
            info!("audio stop cancelled; a subscriber re-joined");
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
    }
    let stop = inner
        .state()
        .generation
        .as_ref()
        .filter(|a| a.id == gen_id)
        .map(|a| a.stop.clone());
    debug!(gen_id, "audio generation stopped (last subscriber)");
    if let Some(stop) = stop {
        stop.stop().await;
    }
    let mut state = inner.state();
    if state.generation.as_ref().is_some_and(|a| a.id == gen_id) {
        state.generation = None;
        set_status_locked(&mut state, Status::Unavailable);
    }
}

fn cache_init(inner: &Inner, gen_id: u64, bytes: Vec<u8>) {
    let mut state = inner.state();
    if let Some(active) = state.generation.as_mut().filter(|a| a.id == gen_id) {
        active.cached_init = Some(bytes);
    }
}

/// Mark a generation terminal before its pump awaits source cleanup.
fn mark_finished(inner: &Inner, gen_id: u64) {
    let mut state = inner.state();
    if let Some(active) = state.generation.as_mut().filter(|a| a.id == gen_id) {
        active.finished = true;
    }
}

fn elapsed_ms(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis() as i64
}

fn set_status(inner: &Inner, gen_id: Option<u64>, status: Status) {
    let mut state = inner.state();
    if gen_id.is_some_and(|id| !state.generation.as_ref().is_some_and(|a| a.id == id)) {
        return;
    }
    set_status_locked(&mut state, status);
}

fn set_status_locked(state: &mut State, status: Status) {
    if state.status != status {
        info!(from = ?state.status, to = ?status, "audio status");
        state.status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::source::FakeSource;
    use crate::audio::webm::fixtures::build_fixture;
    use std::time::Instant;
    use tokio::time::timeout;

    const WAIT: Duration = Duration::from_secs(5);

    async fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + WAIT;
        while !f() {
            if Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        true
    }

    /// Receive the next event, failing the test on timeout or channel close.
    async fn next_event(rx: &mut broadcast::Receiver<AudioEvent>) -> AudioEvent {
        timeout(WAIT, rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed before event")
    }

    /// Skip events until the first `Media` is seen (returns it).
    async fn next_media(rx: &mut broadcast::Receiver<AudioEvent>) -> AudioEvent {
        loop {
            let event = next_event(rx).await;
            if matches!(event, AudioEvent::Media(_, _)) {
                return event;
            }
        }
    }

    /// Wait for a `Failed` event (the channel may close right after).
    async fn wait_for_failed(rx: &mut broadcast::Receiver<AudioEvent>) -> bool {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(AudioEvent::Failed)) => return true,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => return false,
                Err(_) => {}
            }
        }
        false
    }

    fn continuous_source(clusters: usize) -> Arc<FakeSource> {
        let (stream, init) = build_fixture(clusters);
        // A real capture emits the header exactly once; the fake loops only
        // the cluster bytes so the segmenter never sees a second Segment.
        Arc::new(
            FakeSource::new(stream)
                .with_head(init.len())
                .with_delay(Duration::from_micros(200)),
        )
    }

    #[tokio::test]
    async fn first_subscriber_starts_exactly_one_capture() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        assert_eq!(broadcaster.status(), Status::Idle);
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        assert_eq!(source.start_count(), 1);
        assert_eq!(broadcaster.status(), Status::Starting);
        assert!(matches!(next_event(&mut rx).await, AudioEvent::Init(_, _)));
        assert_eq!(broadcaster.status(), Status::Starting);
        next_media(&mut rx).await;
        assert_eq!(broadcaster.status(), Status::Capturing);
        drop(sub);
        assert!(
            wait_until(|| source.stop_count() >= 1).await,
            "stop not observed"
        );
    }

    #[tokio::test]
    async fn two_subscribers_share_one_generation() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub_a = broadcaster.subscribe().await.expect("subscribe a");
        let mut rx_a = sub_a.resubscribe();
        assert_eq!(source.start_count(), 1);
        assert!(matches!(
            next_event(&mut rx_a).await,
            AudioEvent::Init(_, _)
        ));
        // A joiner after the init was produced gets it via the cached copy.
        let sub_b = broadcaster.subscribe().await.expect("subscribe b");
        assert_eq!(source.start_count(), 1);
        assert!(
            sub_b.cached_init().is_some(),
            "late joiner must get the cached init"
        );
        // One more subscriber while running still does not restart.
        let sub_c = broadcaster.subscribe().await.expect("subscribe c");
        assert_eq!(source.start_count(), 1);
        drop(sub_c);
        // Two still listening: no stop yet.
        assert_eq!(source.stop_count(), 0);
    }

    #[tokio::test]
    async fn wait_for_init_returns_the_cached_init() {
        // The first subscriber's `cached_init` is `None` at `subscribe()`
        // time; the pump caches the init a moment later. `wait_for_init`
        // must hand that cached copy back, identical to what a late joiner
        // gets — this is the reliable path the gRPC `listen` uses instead of
        // the lossy channel's one-time Init event.
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let gen_id = sub.gen_id();
        let init = timeout(WAIT, broadcaster.wait_for_init(gen_id, Duration::from_secs(2)))
            .await
            .expect("wait_for_init must not hang")
            .expect("init must be cached");
        assert!(!init.is_empty(), "init must not be empty");
        // Same bytes a late joiner is handed (the cached copy).
        let late = broadcaster.subscribe().await.expect("late subscribe");
        let late_init = late.cached_init().expect("late joiner has cached init").to_vec();
        assert_eq!(init, late_init, "wait_for_init must return the cached init");
        drop(late);
        drop(sub);
    }

    #[tokio::test]
    async fn wait_for_init_returns_none_when_source_ends_without_init() {
        // A source that ends before emitting a header produces no init and
        // ends the generation cleanly. `wait_for_init` must return `None`
        // (not hang) so the caller falls back to the channel, which surfaces
        // the failure — a hung first subscriber would strand the client in
        // "connecting" forever.
        let source = Arc::new(FakeSource::new(Vec::new()).finite());
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let gen_id = sub.gen_id();
        let result = timeout(WAIT, broadcaster.wait_for_init(gen_id, Duration::from_secs(2)))
            .await
            .expect("wait_for_init must not hang");
        assert!(result.is_none(), "no init was produced");
        drop(sub);
    }

    #[tokio::test]
    async fn last_subscriber_drop_stops_the_source() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub_a = broadcaster.subscribe().await.expect("subscribe a");
        let sub_b = broadcaster.subscribe().await.expect("subscribe b");
        drop(sub_a);
        assert_eq!(source.stop_count(), 0, "stop before last drop");
        drop(sub_b);
        assert!(
            wait_until(|| source.stop_count() >= 1).await,
            "stop not observed after last drop"
        );
        assert!(
            wait_until(|| broadcaster.status() == Status::Unavailable).await,
            "status not Unavailable"
        );
        // A new subscriber starts a new generation.
        let sub_c = broadcaster.subscribe().await.expect("subscribe c");
        let mut rx_c = sub_c.resubscribe();
        assert_eq!(source.start_count(), 2);
        assert!(matches!(
            next_event(&mut rx_c).await,
            AudioEvent::Init(_, _)
        ));
        drop(sub_c);
        assert!(wait_until(|| source.stop_count() >= 2).await);
    }

    #[tokio::test]
    async fn late_subscriber_gets_cached_init_before_media() {
        let source = continuous_source(2);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let early = broadcaster.subscribe().await.expect("subscribe early");
        let mut rx_early = early.resubscribe();
        // Let the generation produce its init and some media.
        next_media(&mut rx_early).await;
        let late = broadcaster.subscribe().await.expect("subscribe late");
        assert!(
            late.cached_init().is_some(),
            "late subscriber must be handed the cached init"
        );
        // The late subscriber's channel delivers media in order.
        let mut rx_late = late.resubscribe();
        // The channel may replay the still-buffered Init first; media must
        // follow (the gRPC handler de-duplicates init chunks per stream).
        next_media(&mut rx_late).await;
        drop(early);
        drop(late);
        assert!(wait_until(|| source.stop_count() >= 1).await);
    }

    /// Two concurrent streams: the server-side contract the two-browser
    /// (KI-3) scenario relies on. A is the starter; B joins while the
    /// generation is running. B must get the cached init and then the
    /// *live tail* of the generation — its first media is strictly newer
    /// than A's current position (a fresh broadcast receiver can never be
    /// replayed the first seconds) and close behind it. A is undisturbed,
    /// and A's explicit StopCapture ends both streams.
    #[tokio::test]
    async fn late_joiner_stream_is_live_tail_not_replay() {
        // 40 fixture clusters (~28 chunks) at 20 ms per chunk: a long,
        // paced generation.
        let (stream, init) = build_fixture(40);
        let source = Arc::new(
            FakeSource::new(stream)
                .with_head(init.len())
                .with_delay(Duration::from_millis(20)),
        );
        let broadcaster = AudioBroadcaster::new(source.clone());

        // Stream A: the starter. Init, then a few media events.
        let sub_a = broadcaster.subscribe().await.expect("subscribe a");
        let mut rx_a = sub_a.resubscribe();
        assert!(matches!(
            next_event(&mut rx_a).await,
            AudioEvent::Init(_, _)
        ));
        let mut a_last_ts = 0i64;
        for _ in 0..5 {
            let AudioEvent::Media(_, ts) = next_media(&mut rx_a).await else {
                panic!("expected Media on A")
            };
            a_last_ts = ts;
        }

        // Stream B: the late joiner.
        let sub_b = broadcaster.subscribe().await.expect("subscribe b");
        assert!(
            sub_b.cached_init().is_some(),
            "late joiner must be handed the cached init"
        );
        let mut rx_b = sub_b.resubscribe();
        let AudioEvent::Media(_, b_first_ts) = next_media(&mut rx_b).await else {
            panic!("expected Media on B")
        };

        // No replay: B's first event is strictly newer than A's position —
        // B cannot be handed the generation's first seconds.
        assert!(
            b_first_ts > a_last_ts,
            "B first ts {b_first_ts} replayed A's past (A was at {a_last_ts})"
        );
        // Live tail: the gap is one chunk delay plus scheduling, not
        // seconds of backfill.
        assert!(
            b_first_ts - a_last_ts <= 200,
            "B first ts {b_first_ts} too far behind A (at {a_last_ts})"
        );

        // Both streams keep advancing. A chunk can carry two clusters, and
        // the pump stamps both with the same millisecond, so per-event the
        // timecode is non-decreasing and strict over a small window.
        let mut a_prev = a_last_ts;
        for _ in 0..4 {
            let AudioEvent::Media(_, ts) = next_media(&mut rx_a).await else {
                panic!("expected Media on A after B joined")
            };
            assert!(ts >= a_prev, "A timecodes must be non-decreasing ({ts} < {a_prev})");
            a_prev = ts;
        }
        assert!(a_prev > a_last_ts, "A must advance over a window");
        let mut b_prev = b_first_ts;
        for _ in 0..4 {
            let AudioEvent::Media(_, ts) = next_media(&mut rx_b).await else {
                panic!("expected Media on B")
            };
            assert!(ts >= b_prev, "B timecodes must be non-decreasing ({ts} < {b_prev})");
            b_prev = ts;
        }
        assert!(b_prev > b_first_ts, "B must advance over a window");

        // A explicit StopCapture ends both streams.
        broadcaster.stop_capture().await;
        assert!(wait_for_failed(&mut rx_a).await, "A not failed after stop");
        assert!(
            wait_for_failed(&mut rx_b).await,
            "B not failed after stop"
        );
        assert!(wait_until(|| source.stop_count() >= 1).await);
    }

    #[tokio::test]
    async fn rejoin_during_grace_cancels_the_stop() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        next_media(&mut rx).await;
        drop(sub);
        // Re-join inside the 100 ms grace window.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rejoined = broadcaster.subscribe().await.expect("rejoin");
        assert_eq!(
            source.start_count(),
            1,
            "rejoin must not start a new capture"
        );
        // The stop was cancelled: media keeps flowing, no kill happened.
        let mut rx2 = rejoined.resubscribe();
        next_media(&mut rx2).await;
        assert_eq!(source.stop_count(), 0, "stop must be cancelled");
        drop(rejoined);
        assert!(wait_until(|| source.stop_count() >= 1).await);
    }

    #[tokio::test]
    async fn finite_source_end_fails_generation_and_restart_works() {
        let (stream, _) = build_fixture(1);
        let source = Arc::new(
            FakeSource::new(stream)
                .finite()
                .with_delay(Duration::from_micros(200)),
        );
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        next_media(&mut rx).await;
        // The finite source ends on its own: Failed, then the generation is
        // cleared with status Unavailable.
        assert!(
            wait_for_failed(&mut rx).await,
            "no Failed event after clean end"
        );
        assert!(
            wait_until(|| broadcaster.status() == Status::Unavailable).await,
            "status not Unavailable after clean end"
        );
        // Re-subscribing starts a fresh generation that replays from the top.
        let sub2 = broadcaster.subscribe().await.expect("resubscribe");
        let mut rx2 = sub2.resubscribe();
        assert_eq!(source.start_count(), 2);
        assert!(matches!(next_event(&mut rx2).await, AudioEvent::Init(_, _)));
        drop(sub2);
    }

    #[tokio::test]
    async fn killed_source_fails_generation() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        next_media(&mut rx).await;
        source.kill();
        assert!(wait_for_failed(&mut rx).await, "no Failed event after kill");
        assert!(
            wait_until(|| broadcaster.status() == Status::Failed).await,
            "status not Failed"
        );
        // The generation was cleared: resubscribe starts fresh.
        let sub2 = broadcaster.subscribe().await.expect("resubscribe");
        assert_eq!(source.start_count(), 2);
        drop(sub2);
    }

    #[tokio::test]
    async fn explicit_stop_capture_kills_source_and_restart_works() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        next_media(&mut rx).await;
        broadcaster.stop_capture().await;
        assert!(
            wait_until(|| source.stop_count() >= 1).await,
            "stop not observed"
        );
        assert!(
            wait_until(|| broadcaster.status() == Status::Unavailable).await,
            "status not Unavailable"
        );
        drop(sub);
        // A new subscriber starts a fresh generation.
        let sub2 = broadcaster.subscribe().await.expect("resubscribe");
        let mut rx2 = sub2.resubscribe();
        assert_eq!(source.start_count(), 2);
        assert!(matches!(next_event(&mut rx2).await, AudioEvent::Init(_, _)));
        drop(sub2);
    }

    #[tokio::test]
    async fn shutdown_stops_capture_and_refuses_subscribers() {
        let source = continuous_source(1);
        let broadcaster = AudioBroadcaster::new(source.clone());
        let sub = broadcaster.subscribe().await.expect("subscribe");
        let mut rx = sub.resubscribe();
        next_media(&mut rx).await;
        drop(sub);
        // Let the last-subscriber stop finish so shutdown sees no generation.
        assert!(wait_until(|| source.stop_count() >= 1).await);
        broadcaster.shutdown().await;
        assert!(
            matches!(broadcaster.subscribe().await, Err(AudioError::Shutdown)),
            "subscribe after shutdown must fail"
        );
        // Shutdown with a live generation stops the child and awaits exit.
        let source2 = continuous_source(1);
        let broadcaster2 = AudioBroadcaster::new(source2.clone());
        let sub2 = broadcaster2.subscribe().await.expect("subscribe");
        let mut rx2 = sub2.resubscribe();
        next_media(&mut rx2).await;
        broadcaster2.shutdown().await;
        assert!(
            wait_until(|| source2.stop_count() >= 1).await,
            "shutdown must stop the capture"
        );
        drop(sub2);
    }
}
