//! Capture sources: supervised producers of a WebM byte stream.
//!
//! Sources implement [`CaptureSource`] and are started by the
//! `AudioBroadcaster` on the first subscriber:
//! - [`CommandSource`] runs an arbitrary command line whose stdout is the
//!   WebM stream (the `--audio-cmd` test hook; E2E tests feed it `ubc125
//!   audio-tone` output);
//! - the production source is the in-process ALSA → Opus → WebM pipeline
//!   (`crate::audio::native`), which reuses the same event channel and stop
//!   plumbing as the command sources.
//!
//! Command-source supervision rules:
//! - stdout is read asynchronously with `AsyncReadExt` (no `spawn_blocking`);
//! - stderr is drained into a bounded ring and surfaced on non-clean exit;
//! - [`StopHandle::stop`] kills the child and **awaits its exit** so the
//!   ALSA device is released before the handle is considered stopped.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io::Error as IoError;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tracing::warn;

/// Default ALSA device for the scanner line-in (USB mic on the Pi).
pub const DEFAULT_AUDIO_DEVICE: &str = "hw:2";

/// Capacity of the source event channel.
pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 64;
/// Bytes of stderr retained for diagnostics.
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// Stdout read buffer size.
const STDOUT_READ_SIZE: usize = 64 * 1024;

/// One item from a running capture source.
#[derive(Debug)]
pub enum SourceEvent {
    /// A raw slice of the source's stdout. Pipe reads have no relationship
    /// to EBML boundaries; feed these bytes to the `WebmSegmenter`.
    Bytes(Vec<u8>),
    /// The source has terminated (normal end, failure, or kill).
    End(SourceExit),
}

/// How the source terminated.
#[derive(Debug, Clone)]
pub enum SourceExit {
    /// Process exited normally.
    Clean,
    /// Non-zero exit, kill, or I/O failure; the string carries the reason
    /// and the tail of stderr.
    Failed(String),
}

impl SourceExit {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// Errors starting a capture source.
#[derive(Debug)]
pub enum SourceError {
    /// The child process could not be spawned (missing binary, bad args).
    Spawn(IoError),
    /// An in-process source failed to start (e.g. the ALSA device could
    /// not be opened); the string carries the reason.
    Start(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to spawn capture process: {e}"),
            Self::Start(reason) => write!(f, "failed to start capture: {reason}"),
        }
    }
}

impl Error for SourceError {}

impl From<crate::audio::alsacapture::PcmError> for SourceError {
    fn from(e: crate::audio::alsacapture::PcmError) -> Self {
        Self::Start(e.to_string())
    }
}

/// A capture source that can start supervised WebM-producing processes.
///
/// `start` returns a boxed future so the trait stays object-safe
/// (`Arc<dyn CaptureSource>` in the broadcaster).
pub trait CaptureSource: Send + Sync {
    /// Start a new capture. Every `start` launches an independent process.
    fn start(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CaptureHandle, SourceError>> + Send + '_>>;
}

/// Shared stop state between a [`CaptureHandle`] and its reader task.
///
/// `pid` is a plain `std::sync::Mutex` on purpose: the critical section is
/// a single `u32` store, never held across an `.await`, so a momentary
/// thread block is harmless and a `tokio` mutex would buy nothing. Lock
/// poisoning is recovered (`into_inner`) rather than propagated, so a
/// panicking owner cannot cascade through stop/cleanup.
pub(crate) struct HandleStop {
    pid: std::sync::Mutex<Option<u32>>,
    pub(crate) cancel: watch::Sender<bool>,
    /// Persistent record that the reader task finished. `watch::Sender::send`
    /// drops values with no receivers, so a source that exits before
    /// `stop()` is called would otherwise leave the stopper hanging.
    exited_flag: AtomicBool,
    exited: watch::Sender<bool>,
}

impl HandleStop {
    pub(crate) fn new() -> Self {
        let (cancel, _) = watch::channel(false);
        let (exited, _) = watch::channel(false);
        Self {
            pid: std::sync::Mutex::new(None),
            cancel,
            exited_flag: AtomicBool::new(false),
            exited,
        }
    }

    /// Mark the reader task finished.
    pub(crate) fn mark_exited(&self) {
        self.exited_flag.store(true, Ordering::SeqCst);
        // Best effort: a send with no subscribers is a deliberate no-op —
        // the flag above carries the state, not this channel.
        let _ = self.exited.send(true);
    }
}

/// A running capture: an event stream plus a stop handle.
pub struct CaptureHandle {
    pub(crate) rx: mpsc::Receiver<SourceEvent>,
    pub(crate) stop: Arc<HandleStop>,
}

impl CaptureHandle {
    /// Take ownership of the event stream (for the pump task). `recv`
    /// returns the final [`SourceEvent::End`] and then `None` once the
    /// source task has finished.
    pub fn into_events(self) -> mpsc::Receiver<SourceEvent> {
        self.rx
    }

    /// A cloneable stop handle for this capture.
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(Arc::clone(&self.stop))
    }
}

/// A cloneable handle that stops a capture: kills the child process and
/// awaits its exit so any device (e.g. ALSA) is released. Idempotent and
/// safe to call after the source has already ended.
#[derive(Clone)]
pub struct StopHandle(Arc<HandleStop>);

impl StopHandle {
    pub async fn stop(&self) {
        // The guard is scoped so it is dropped before the awaits below
        // (a `std` MutexGuard held across an await would make the future
        // non-Send).
        {
            let pid = self
                .0
                .pid
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // SIGKILL: command sources handle no signals in our usage; the
            // kill must release the ALSA device immediately.
            if let Some(pid) = *pid {
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        // Best effort: a send with no subscribers is a deliberate no-op.
        let _ = self.0.cancel.send(true);
        if self.0.exited_flag.load(Ordering::SeqCst) {
            return;
        }
        let mut exited = self.0.exited.subscribe();
        while !*exited.borrow_and_update() {
            if exited.changed().await.is_err() {
                break;
            }
        }
    }
}

/// A capture source for an arbitrary command line; its stdout is the WebM
/// byte stream and its stderr is drained for diagnostics.
#[derive(Debug, Clone)]
pub struct CommandSource {
    argv: Vec<String>,
    label: String,
}

impl CommandSource {
    pub fn new(argv: Vec<String>) -> Self {
        let label = argv.first().cloned().unwrap_or_default();
        Self { argv, label }
    }
}

impl CaptureSource for CommandSource {
    fn start(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CaptureHandle, SourceError>> + Send + '_>> {
        // `run_capture` is fully synchronous; the boxed future exists only
        // for the object-safe trait signature and completes on first poll.
        Box::pin(async move { run_capture(&self.argv, &self.label) })
    }
}

/// Spawn and supervise a WebM-producing child process.
fn run_capture(argv: &[String], label: &str) -> Result<CaptureHandle, SourceError> {
    let stop = Arc::new(HandleStop::new());
    let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    if argv.is_empty() {
        return Err(SourceError::Spawn(IoError::new(
            std::io::ErrorKind::InvalidInput,
            "empty command line",
        )));
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(SourceError::Spawn)?;
    // `take()` only fails if this Command is changed to non-piped stdio;
    // surface that as a source error instead of panicking the broadcaster
    // task that started us.
    let stdout = child.stdout.take().ok_or_else(|| {
        SourceError::Spawn(IoError::other("child stdout was not piped"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        SourceError::Spawn(IoError::other("child stderr was not piped"))
    })?;
    // The task below owns the child (to `wait()` on it); stop() kills it by
    // pid, so it can interrupt the pump phase without owning it.
    *stop.pid.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = child.id();

    let label = label.to_string();
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let stderr_task = tokio::spawn(drain_stderr(stderr, Arc::clone(&stderr_tail)));

    let task_stop = Arc::clone(&stop);
    tokio::spawn(async move {
        let pump_exit = pump_stdout(stdout, &tx, &label).await;
        // Kill before waiting so a wedged child (e.g. blocked on a full
        // pipe) cannot hang the shutdown path. No-op if it already exited.
        if let Err(e) = child.start_kill() {
            // ESRCH means the child already exited (the expected no-op);
            // any other failure would leak a running child, so log it.
            if e.raw_os_error() != Some(libc::ESRCH) {
                warn!("{label}: failed to kill child: {e}");
            }
        }
        let status = child.wait().await;
        stderr_task.await.ok();
        let exit = match status {
            Ok(s) if s.success() => pump_exit,
            Ok(s) => SourceExit::Failed(format!(
                "{label} exited with {s}: {}",
                stderr_snapshot(&stderr_tail)
            )),
            Err(e) => SourceExit::Failed(format!("{label}: wait failed: {e}")),
        };
        let _ = tx.send(SourceEvent::End(exit)).await;
        task_stop.mark_exited();
    });

    Ok(CaptureHandle { rx, stop })
}

/// Pump stdout into the event channel until EOF or reader loss.
async fn pump_stdout(
    mut stdout: tokio::process::ChildStdout,
    tx: &mpsc::Sender<SourceEvent>,
    label: &str,
) -> SourceExit {
    let mut buf = vec![0u8; STDOUT_READ_SIZE];
    loop {
        match stdout.read(&mut buf).await {
            Ok(0) => return SourceExit::Clean,
            Ok(n) => {
                if tx
                    .send(SourceEvent::Bytes(buf[..n].to_vec()))
                    .await
                    .is_err()
                {
                    // Reader gone: nothing more to do; the child is killed
                    // by the caller.
                    return SourceExit::Clean;
                }
            }
            Err(e) => {
                warn!("{label}: capture stdout read error: {e}");
                return SourceExit::Failed(format!("{label}: stdout read error: {e}"));
            }
        }
    }
}

/// Drain stderr into a bounded ring buffer for later diagnostics.
async fn drain_stderr(mut stderr: tokio::process::ChildStderr, ring: Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut ring = ring.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                ring.extend_from_slice(&buf[..n]);
                if ring.len() > STDERR_TAIL_BYTES {
                    let excess = ring.len() - STDERR_TAIL_BYTES;
                    ring.drain(..excess);
                }
            }
        }
    }
}

/// Up to the last five non-empty stderr lines, joined for log messages.
fn stderr_snapshot(ring: &Arc<Mutex<Vec<u8>>>) -> String {
    let bytes = ring
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(5);
    lines[start..].join(" | ")
}

/// Test double: a "running process" that replays a fixture byte stream.
///
/// Loops the fixture continuously (like live audio) until stopped or killed;
/// `finite()` ends with `End(Clean)` after one pass. Counts `start` and stop
/// observations so tests can assert lifecycle behavior.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct FakeSource {
    fixture: Vec<u8>,
    /// Prefix emitted only once; when the stream loops, playback resumes
    /// after it (models a WebM header that is not re-emitted).
    head_len: usize,
    chunk_size: usize,
    delay: Duration,
    loop_forever: bool,
    start_count: Arc<AtomicUsize>,
    stop_count: Arc<AtomicUsize>,
    kill: Arc<AtomicBool>,
}

#[cfg(test)]
impl FakeSource {
    /// `fixture` is the full byte stream; it is emitted in 37-byte chunks.
    pub fn new(fixture: Vec<u8>) -> Self {
        Self {
            fixture,
            head_len: 0,
            chunk_size: 37,
            delay: Duration::ZERO,
            loop_forever: true,
            start_count: Arc::new(AtomicUsize::new(0)),
            stop_count: Arc::new(AtomicUsize::new(0)),
            kill: Arc::new(AtomicBool::new(false)),
        }
    }

    /// End with `End(Clean)` after one pass instead of looping.
    pub fn finite(mut self) -> Self {
        self.loop_forever = false;
        self
    }

    /// Pause between chunks so the stream is not instant.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Emit the first `head_len` bytes once; loops resume after them.
    pub fn with_head(mut self, head_len: usize) -> Self {
        self.head_len = head_len;
        self
    }

    pub fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
    }

    /// Number of stop() calls observed by a running source task.
    pub fn stop_count(&self) -> usize {
        self.stop_count.load(Ordering::SeqCst)
    }

    /// Simulate a crash: the running source ends with `End(Failed)`.
    pub fn kill(&self) {
        self.kill.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl CaptureSource for FakeSource {
    fn start(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<CaptureHandle, SourceError>> + Send + '_>> {
        Box::pin(async move {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            let stop = Arc::new(HandleStop::new());
            let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            let source = self.clone();
            // Subscribe before spawning so a stop() racing ahead of the
            // task's first poll still delivers (watch drops sends with no
            // receivers).
            let mut cancel = stop.cancel.subscribe();
            let task_stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let mut offset = 0usize;
                let exit = loop {
                    if source.kill.load(Ordering::SeqCst) {
                        break SourceExit::Failed("fake source killed".into());
                    }
                    if offset < source.fixture.len() {
                        let end = (offset + source.chunk_size).min(source.fixture.len());
                        tokio::select! {
                            res = tx.send(SourceEvent::Bytes(
                                source.fixture[offset..end].to_vec(),
                            )) => {
                                if res.is_err() {
                                    break SourceExit::Clean;
                                }
                            }
                            _ = cancel.changed() => {
                                source.stop_count.fetch_add(1, Ordering::SeqCst);
                                break SourceExit::Clean;
                            }
                        }
                        offset = end;
                    }
                    if offset >= source.fixture.len() && !source.loop_forever {
                        break SourceExit::Clean;
                    }
                    if offset >= source.fixture.len() {
                        offset = source.head_len;
                    }
                    if *cancel.borrow_and_update() {
                        source.stop_count.fetch_add(1, Ordering::SeqCst);
                        break SourceExit::Clean;
                    }
                    if !source.delay.is_zero() {
                        tokio::time::sleep(source.delay).await;
                    }
                };
                let _ = tx.send(SourceEvent::End(exit)).await;
                task_stop.mark_exited();
            });
            Ok(CaptureHandle { rx, stop })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::webm::fixtures::build_fixture;
    use crate::audio::webm::{DEFAULT_MAX_SEGMENT_SIZE, Segment, WebmSegmenter};
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

    #[tokio::test]
    async fn fake_source_streams_fixture_and_ends_clean() {
        let fixture: Vec<u8> = (0..200u16).map(|i| i as u8).collect();
        let source = FakeSource::new(fixture.clone()).finite();
        let handle = source.start().await.unwrap();
        let _stop = handle.stop_handle();
        let mut events = handle.into_events();
        let mut data = Vec::new();
        let mut ended = false;
        while let Some(event) = timeout(WAIT, events.recv()).await.expect("timed out") {
            match event {
                SourceEvent::Bytes(b) => data.extend_from_slice(&b),
                SourceEvent::End(SourceExit::Clean) => {
                    ended = true;
                    break;
                }
                SourceEvent::End(e) => panic!("unexpected exit: {e:?}"),
            }
        }
        assert!(ended);
        assert_eq!(data, fixture);
        assert_eq!(source.start_count(), 1);
    }

    #[tokio::test]
    async fn fake_source_kill_ends_failed() {
        let source = FakeSource::new(vec![0u8; 1024]).with_delay(Duration::from_millis(1));
        let handle = source.start().await.unwrap();
        let _stop = handle.stop_handle();
        let mut events = handle.into_events();
        source.kill();
        let mut failed = false;
        while let Some(event) = timeout(WAIT, events.recv()).await.expect("timed out") {
            if matches!(event, SourceEvent::End(SourceExit::Failed(_))) {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }

    #[tokio::test]
    async fn stop_terminates_running_fake_and_is_counted() {
        let source = FakeSource::new(vec![0u8; 1024]).with_delay(Duration::from_millis(1));
        let handle = source.start().await.unwrap();
        let stop = handle.stop_handle();
        timeout(WAIT, stop.stop()).await.expect("stop timed out");
        assert!(
            wait_until(|| source.stop_count() >= 1).await,
            "stop not observed"
        );
        // Idempotent: a second stop returns immediately.
        timeout(WAIT, stop.stop())
            .await
            .expect("second stop timed out");
        drop(handle);
    }

    #[tokio::test]
    async fn fake_source_output_is_segmentable_webm() {
        let (stream, _init) = build_fixture(3);
        let source = FakeSource::new(stream)
            .finite()
            .with_delay(Duration::from_micros(50));
        let handle = source.start().await.unwrap();
        let _stop = handle.stop_handle();
        let mut events = handle.into_events();
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        while let Some(event) = timeout(WAIT, events.recv()).await.expect("timed out") {
            match event {
                SourceEvent::Bytes(b) => segments.extend(segmenter.feed(&b).unwrap()),
                SourceEvent::End(e) => {
                    assert!(e.is_clean(), "unexpected exit: {e:?}");
                    break;
                }
            }
        }
        assert_eq!(segments.len(), 4, "init + 3 clusters");
        assert!(matches!(segments[0], Segment::Init(_)));
        assert!(segments[1..].iter().all(|s| matches!(s, Segment::Media(_))));
    }

}
