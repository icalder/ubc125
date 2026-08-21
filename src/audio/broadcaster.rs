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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::audio::source::{CaptureSource, SourceError, SourceEvent, SourceExit, StopHandle};
use crate::audio::webm::{DEFAULT_MAX_SEGMENT_SIZE, Segment, WebmSegmenter};

/// Broadcast channel capacity per generation.
const BROADCAST_CAPACITY: usize = 64;
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
    /// every new generation.
    pub fn new(source: Arc<dyn CaptureSource>) -> Self {
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
            }),
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
            if let Some(active) = &mut state.generation {
                active.subscribers += 1;
                active.stopping = false;
                return Ok(AudioSubscription {
                    sender: active.sender.clone(),
                    cached_init: active.cached_init.clone(),
                    inner: Arc::clone(&self.inner),
                    gen_id: active.id,
                });
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
        let (sender, _initial_receiver) = broadcast::channel(BROADCAST_CAPACITY);
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
            });
        }
        info!("audio capture started");
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
    let _ = sender.send(AudioEvent::Failed);
    if let Some(exit) = exit {
        end_generation(&inner, gen_id, exit).await;
    }
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
        GenerationExit::Unavailable
    } else {
        // Neither stopping nor clean: a genuine source failure.
        let reason = match &source_exit {
            SourceExit::Failed(r) => r.as_str(),
            SourceExit::Clean => unreachable!("clean exit handled above"),
        };
        warn!(%reason, "audio capture failed");
        GenerationExit::Failed
    }
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
