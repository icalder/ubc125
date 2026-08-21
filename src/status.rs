//! Shared scanner status poller: one `GLG` poll task fanned out to any
//! number of gRPC `GetStatus` subscribers (KI-2).
//!
//! The old design ran a singleton poller per stream: opening a new stream
//! cancelled the previous poller, so two clients cancelled each other in an
//! endless ping-pong that flapped both UIs' "OFFLINE" banners.
//!
//! Lifecycle (mirrors [`crate::audio::broadcaster::AudioBroadcaster`]):
//! - the first `subscribe()` starts the poll task;
//! - further subscribers join the same poller; a new `broadcast` receiver
//!   starts at the channel tail (no history replay), so each subscription
//!   also snapshots the last polled status and the stream delivers it
//!   before any live values — a late joiner immediately sees the current
//!   status;
//! - when the last subscriber leaves the poller stops, with a short grace
//!   window in which a rejoining subscriber cancels the stop.
//!
//! Bank mask: each broadcast carries the mask the server currently believes
//! (`StatusUpdate.banks`). The poller re-reads it from the radio every
//! `BANKS_REFRESH_EVERY` polls (bank buttons pressed on the unit itself),
//! and `StatusBroadcaster::set_banks` fast-forwards it after a successful
//! `SetEnabledBanks`, so a change made by any client reaches all subscribers
//! on the very next poll.
//!
//! Transient poll errors are logged and skipped so a serial hiccup does not
//! end the streams (the stream itself is the clients' liveness signal).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::constants::POLL_INTERVAL_MS;
use crate::scanner::ScannerClient;
use crate::types::{BankMask, ScanStatus};

/// Broadcast channel capacity: 64 statuses = 16 s at the 250 ms poll
/// cadence. A receiver that falls further behind gets `Lagged` and its
/// stream skips to the next value — status is latest-state, not an event
/// log, so skipping is harmless (and ending the stream on lag would flap
/// the client's offline banner, the very symptom KI-2 fixes).
const BROADCAST_CAPACITY: usize = 64;
/// Every Nth poll (counting from the first) re-reads the bank mask from
/// the radio, so bank buttons pressed on the unit itself reach the clients.
/// The PRG/SCG/EPG program-mode round-trip is far too frequent for the
/// 250 ms GLG cadence; 120 polls ≈ 30 s.
const BANKS_REFRESH_EVERY: u64 = 120;
/// Grace window between the last subscriber leaving and the poll task
/// stopping; a subscriber joining inside it cancels the stop.
const STOP_JOIN_GRACE: Duration = Duration::from_millis(100);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One broadcast status: the polled GLG plus the bank mask the server
/// currently believes. The mask is cached (it is not part of the GLG
/// reply): the poller refreshes it from the radio every
/// `BANKS_REFRESH_EVERY` polls, and `StatusBroadcaster::set_banks`
/// fast-forwards it after a successful `SetEnabledBanks`.
#[derive(Clone, Debug)]
pub struct StatusUpdate {
    pub status: ScanStatus,
    pub banks: BankMask,
}

/// One active poller: the shared broadcast sender and subscriber
/// accounting.
struct Active {
    id: u64,
    sender: broadcast::Sender<StatusUpdate>,
    subscribers: usize,
    /// Last successfully polled update, handed to late joiners (a new
    /// broadcast receiver starts at the channel tail and sees no history).
    last: Option<StatusUpdate>,
    /// A stop is pending (last subscriber left); a rejoin resets it.
    stopping: bool,
    /// Set by the stop task after the grace window; the poll task exits at
    /// its next check.
    poll_stop: Arc<AtomicBool>,
    /// Set by the poll task when it has finished; the stop task waits on
    /// it before clearing the generation, so a rejoiner never starts a
    /// second poll task while the first is still polling.
    poller_exited: Arc<AtomicBool>,
}

struct State {
    generation: Option<Active>,
}

struct Inner {
    client: Arc<StdMutex<ScannerClient>>,
    /// Serializes generation creation so concurrent `subscribe()` calls
    /// never start two poll tasks.
    create: tokio::sync::Mutex<()>,
    state: StdMutex<State>,
    next_id: AtomicU64,
    /// Bank mask the poller stamps onto each broadcast; refreshed from the
    /// radio (slow) and by `set_banks` (fast). Shared across generations so
    /// a new poller inherits the last known mask.
    banks: StdMutex<BankMask>,
    /// Polls between bank-mask radio refreshes (see `BANKS_REFRESH_EVERY`).
    refresh_every: u64,
}

/// Lock the bank cache, recovering from poison (a panic elsewhere must not
/// wedge the poller).
fn lock_banks(banks: &StdMutex<BankMask>) -> MutexGuard<'_, BankMask> {
    banks.lock().unwrap_or_else(|e| e.into_inner())
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Fans one shared status poller out to any number of `GetStatus`
/// subscribers.
#[derive(Clone)]
pub struct StatusBroadcaster {
    inner: Arc<Inner>,
}

impl StatusBroadcaster {
    pub fn new(client: Arc<StdMutex<ScannerClient>>) -> Self {
        Self::new_with_refresh_every(client, BANKS_REFRESH_EVERY)
    }

    /// Same as [`Self::new`] with a custom radio-refresh interval; the
    /// production cadence is the `BANKS_REFRESH_EVERY` constant.
    fn new_with_refresh_every(
        client: Arc<StdMutex<ScannerClient>>,
        refresh_every: u64,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                create: tokio::sync::Mutex::new(()),
                state: StdMutex::new(State { generation: None }),
                next_id: AtomicU64::new(1),
                banks: StdMutex::new(BankMask::new()),
                refresh_every,
            }),
        }
    }

    /// Update the cached bank mask so every subscriber sees it on the very
    /// next poll, without waiting for the slow radio refresh. Called after
    /// an authoritative read/write of the mask (`SetEnabledBanks`,
    /// `GetEnabledBanks`).
    pub fn set_banks(&self, mask: &BankMask) {
        *lock_banks(&self.inner.banks) = mask.clone();
    }

    /// True while a poll task is running (or in its stop grace window).
    /// For diagnostics and tests.
    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.inner.state().generation.is_some()
    }

    /// Number of poller generations started so far (diagnostics/tests).
    #[allow(dead_code)]
    pub fn generations_started(&self) -> u64 {
        self.inner.next_id.load(Ordering::SeqCst) - 1
    }

    /// Join the running poller, starting one if none is running.
    ///
    /// The returned subscription carries the generation's broadcast
    /// sender. Dropping it removes the subscriber; when the count reaches
    /// zero the poller is stopped (grace-windowed).
    pub async fn subscribe(&self) -> StatusSubscription {
        // Held for the whole join-or-create so two concurrent subscribers
        // cannot each start a poll task.
        let _creating = self.inner.create.lock().await;
        {
            let mut state = self.inner.state();
            if let Some(active) = &mut state.generation {
                active.subscribers += 1;
                active.stopping = false;
                return StatusSubscription {
                    sender: active.sender.clone(),
                    last: active.last.clone(),
                    inner: Arc::clone(&self.inner),
                    gen_id: active.id,
                };
            }
        }
        self.start_generation()
    }

    fn start_generation(&self) -> StatusSubscription {
        let (sender, initial_rx) = broadcast::channel(BROADCAST_CAPACITY);
        // Drop the initial receiver immediately: it would otherwise count
        // as a consumer and mask the "no receivers left" send error.
        drop(initial_rx);
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (poll_stop, poller_exited) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        {
            let mut state = self.inner.state();
            state.generation = Some(Active {
                id,
                sender: sender.clone(),
                last: None,
                subscribers: 1,
                stopping: false,
                poll_stop: poll_stop.clone(),
                poller_exited: poller_exited.clone(),
            });
        }
        info!("status poller started");
        tokio::spawn(run_poller(
            Arc::clone(&self.inner),
            id,
            self.inner.client.clone(),
            sender.clone(),
            poll_stop,
            poller_exited,
        ));
        StatusSubscription {
            sender,
            last: None,
            inner: Arc::clone(&self.inner),
            gen_id: id,
        }
    }
}

/// A single subscriber's view of the poller.
///
/// Dropping it decrements the generation's subscriber count; the last
/// subscriber triggers the (grace-windowed) stop.
pub struct StatusSubscription {
    /// Keeps the channel alive; also the source of fresh receivers. A
    /// `Receiver` is deliberately not stored here: see the audio
    /// broadcaster's group 5 findings on unpolled receivers.
    sender: broadcast::Sender<StatusUpdate>,
    /// Join-time snapshot of the last polled update (late-joiner fast
    /// path; the channel itself delivers only values sent after join).
    last: Option<StatusUpdate>,
    inner: Arc<Inner>,
    gen_id: u64,
}

impl StatusSubscription {
    /// The last polled update at join time, for clients that joined after
    /// the poller started.
    pub fn cached_status(&self) -> Option<&StatusUpdate> {
        self.last.as_ref()
    }

    /// A fresh receiver bound to the generation's channel; it delivers
    /// only values sent after it was created.
    pub fn resubscribe(&self) -> broadcast::Receiver<StatusUpdate> {
        self.sender.subscribe()
    }
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        let inner = Arc::clone(&self.inner);
        let gen_id = self.gen_id;
        let is_last = {
            let mut state = inner.state();
            let Some(active) = state.generation.as_mut().filter(|a| a.id == gen_id) else {
                return;
            };
            active.subscribers = active.subscribers.saturating_sub(1);
            let is_last = active.subscribers == 0;
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
            warn!("last status subscriber dropped outside a runtime; the poll task will keep running until the process exits");
            return;
        };
        handle.spawn(run_stop_task(Arc::clone(&inner), gen_id));
    }
}

/// One shared poll task: poll `GLG` every `POLL_INTERVAL_MS` and broadcast
/// the result, stamped with the current bank mask. Transient poll errors
/// are logged and skipped. The task only ends when the stop task sets
/// `poll_stop` (after the grace window) or the generation is cleared.
async fn run_poller(
    inner: Arc<Inner>,
    gen_id: u64,
    client: Arc<StdMutex<ScannerClient>>,
    sender: broadcast::Sender<StatusUpdate>,
    poll_stop: Arc<AtomicBool>,
    poller_exited: Arc<AtomicBool>,
) {
    let mut polls = 0;
    loop {
        if poll_stop.load(Ordering::Relaxed) || !generation_alive(&inner, gen_id) {
            break;
        }
        // The first poll (and every `refresh_every`-th after) re-reads the
        // bank mask from the radio, so unit-side bank button presses reach
        // the clients.
        let refresh_banks = polls % inner.refresh_every == 0;
        if let Some((status, banks)) = poll_once(&client, refresh_banks).await {
            if let Some(banks) = banks {
                *lock_banks(&inner.banks) = banks;
            }
            let update = StatusUpdate {
                status,
                banks: lock_banks(&inner.banks).clone(),
            };
            cache_last(&inner, gen_id, &update);
            // No consumer receivers (all streams gone, grace window
            // active): discard and keep polling so a rejoiner inside the
            // window finds a live poller.
            let _ = sender.send(update);
        }
        polls += 1;
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    poller_exited.store(true, Ordering::Relaxed);
    // Drop the sender so any remaining receivers see a closed channel.
}

/// True while this generation still exists (any state) in the broadcaster.
fn generation_alive(inner: &Inner, gen_id: u64) -> bool {
    inner
        .state()
        .generation
        .as_ref()
        .is_some_and(|a| a.id == gen_id)
}

/// Store the last polled update for late joiners. Id-gated: a stale poll
/// from an old generation never overwrites a newer one's value.
fn cache_last(inner: &Inner, gen_id: u64, update: &StatusUpdate) {
    let mut state = inner.state();
    if let Some(active) = state.generation.as_mut().filter(|a| a.id == gen_id) {
        active.last = Some(update.clone());
    }
}

/// One poll round-trip on the blocking pool: always `GLG`, plus — when
/// `refresh_banks` — a best-effort `SCG` bank-mask read. A failed refresh
/// keeps the GLG result (the cache stays untouched); a failed GLG poll
/// returns `None` (with a warning).
async fn poll_once(
    client: &Arc<StdMutex<ScannerClient>>,
    refresh_banks: bool,
) -> Option<(ScanStatus, Option<BankMask>)> {
    let client = client.clone();
    let poll = tokio::task::spawn_blocking(move || {
        let mut scanner = client
            .lock()
            .map_err(|e| format!("scanner mutex poisoned: {e}"))?;
        let status = scanner.get_status().map_err(|e| e.to_string())?;
        let banks = if refresh_banks {
            match scanner.get_banks() {
                Ok(banks) => Some(banks),
                Err(e) => {
                    warn!("bank mask refresh failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        Ok::<(ScanStatus, Option<BankMask>), String>((status, banks))
    })
    .await;
    match poll {
        Ok(Ok(result)) => Some(result),
        Ok(Err(e)) => {
            warn!("GetStatus poll failed: {e}");
            None
        }
        Err(e) => {
            warn!("GetStatus poll task failed: {e}");
            None
        }
    }
}

/// Stop task spawned when the last subscriber leaves: give a rejoining
/// subscriber a grace window to cancel the stop, then stop the poll task,
/// await its exit, and clear the generation.
async fn run_stop_task(inner: Arc<Inner>, gen_id: u64) {
    let deadline = Instant::now() + STOP_JOIN_GRACE;
    loop {
        let (stopping, subscribers) = {
            let state = inner.state();
            match state.generation.as_ref() {
                Some(a) if a.id == gen_id => (a.stopping, a.subscribers),
                // The generation was already cleared; nothing to stop.
                _ => return,
            }
        };
        // A joiner reset the flag or showed up: abort, no stop.
        if !stopping || subscribers > 0 {
            info!("status poller stop cancelled; a subscriber re-joined");
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
    }
    let (poll_stop, poller_exited) = {
        let state = inner.state();
        let Some(active) = state.generation.as_ref().filter(|a| a.id == gen_id) else {
            return;
        };
        (active.poll_stop.clone(), active.poller_exited.clone())
    };
    // Stop the poll task and await its exit before clearing, so a rejoiner
    // never starts a second poll task while the first is still polling.
    poll_stop.store(true, Ordering::Relaxed);
    while !poller_exited.load(Ordering::Relaxed) {
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
    }
    let mut state = inner.state();
    if state.generation.as_ref().is_some_and(|a| a.id == gen_id) {
        state.generation = None;
        info!("status poller stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::mock::{GLG_OK, mock_client};
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

    /// A bank-mask read reply used with the canned PRG/SCG/EPG/KEY,S,P
    /// program-mode cycle (alternating banks, like the fake scanner).
    const SCG_ALT: &str = "SCG,0101010101";
    /// One canned bank-refresh cycle: PRG, SCG reply, EPG, KEY,S,P reply.
    const SCG_CYCLE: [&str; 4] = ["PRG", SCG_ALT, "EPG", "KEY"];

    /// Receive the next update, failing the test on timeout or channel
    /// close.
    async fn next_status(rx: &mut broadcast::Receiver<StatusUpdate>) -> StatusUpdate {
        loop {
            match timeout(WAIT, rx.recv()).await {
                Ok(Ok(update)) => return update,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                _ => panic!("no status within {WAIT:?}"),
            }
        }
    }

    /// A broadcaster over a mock scanner with the given canned GLG
    /// responses, plus the mock's written-command log.
    fn status_broadcaster(
        responses: &[&str],
    ) -> (
        StatusBroadcaster,
        Arc<StdMutex<Vec<String>>>,
    ) {
        let (client, written) = mock_client(responses);
        (
            StatusBroadcaster::new(Arc::new(StdMutex::new(client))),
            written,
        )
    }

    #[tokio::test]
    async fn first_subscriber_starts_exactly_one_poller() {
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        let (broadcaster, written) = status_broadcaster(&responses);
        assert!(!broadcaster.is_active());
        let sub = broadcaster.subscribe().await;
        let mut rx = sub.resubscribe();
        assert!(broadcaster.is_active());
        assert_eq!(broadcaster.generations_started(), 1);
        let update = next_status(&mut rx).await;
        assert_eq!(update.status.frequency.to_string(), "123.9750");
        // The first poll also refreshes the bank mask from the radio.
        assert_eq!(update.banks, BankMask::from_scanner_response(SCG_ALT));
        drop(sub);
        assert!(
            wait_until(|| !broadcaster.is_active()).await,
            "poller did not stop after the last subscriber left"
        );
        assert_eq!(
            *written.lock().unwrap(),
            vec!["GLG\r", "PRG\r", "SCG\r", "EPG\r", "KEY,S,P\r"],
            "exactly one poll (GLG + the poll-0 bank refresh)"
        );
    }

    #[tokio::test]
    async fn two_subscribers_share_one_poller() {
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        responses.extend_from_slice(&[GLG_OK, GLG_OK, GLG_OK]);
        let (broadcaster, written) = status_broadcaster(&responses);
        let sub_a = broadcaster.subscribe().await;
        let mut rx_a = sub_a.resubscribe();
        next_status(&mut rx_a).await;
        // A joiner must not start a second poll task (KI-2: the old
        // singleton made each new stream cancel the previous poller).
        let sub_b = broadcaster.subscribe().await;
        assert_eq!(
            broadcaster.generations_started(),
            1,
            "join must not start a second poller"
        );
        // A keeps receiving after B joined — the KI-2 regression.
        next_status(&mut rx_a).await;
        // B got the last polled status at join time and keeps receiving
        // live values on its channel.
        assert!(
            sub_b.cached_status().is_some(),
            "late joiner must be handed the last polled status"
        );
        let mut rx_b = sub_b.resubscribe();
        next_status(&mut rx_b).await;
        // A leaves: the poller stays up for B.
        drop(sub_a);
        assert!(
            broadcaster.is_active(),
            "poller must stay up while a subscriber remains"
        );
        drop(sub_b);
        assert!(wait_until(|| !broadcaster.is_active()).await);
        // One bank refresh for the poller's life (poll 0), and at most
        // four GLG polls (three canned plus one in-flight after the stop).
        let sent: Vec<String> = written.lock().unwrap().clone();
        let count = |cmd: &str| {
            sent
                .iter()
                .filter(|c| **c == format!("{cmd}\r"))
                .count()
        };
        assert_eq!(count("SCG"), 1, "unexpected commands: {sent:?}");
        assert!(count("GLG") <= 4, "unexpected poll count: {sent:?}");
    }

    #[tokio::test]
    async fn rejoin_during_grace_cancels_the_stop() {
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        responses.push(GLG_OK);
        let (broadcaster, _written) = status_broadcaster(&responses);
        let sub = broadcaster.subscribe().await;
        let mut rx = sub.resubscribe();
        next_status(&mut rx).await;
        drop(sub);
        // Re-join inside the 100 ms grace window.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rejoined = broadcaster.subscribe().await;
        assert_eq!(
            broadcaster.generations_started(),
            1,
            "rejoin must not start a new poller"
        );
        // The stop was cancelled: the buffered value is still delivered.
        let mut rx2 = rejoined.resubscribe();
        next_status(&mut rx2).await;
        drop(rejoined);
        assert!(wait_until(|| !broadcaster.is_active()).await);
    }

    #[tokio::test]
    async fn late_subscriber_gets_cached_status() {
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        responses.extend_from_slice(&[GLG_OK, GLG_OK]);
        let (broadcaster, _written) = status_broadcaster(&responses);
        let early = broadcaster.subscribe().await;
        let mut rx_early = early.resubscribe();
        // Let two polls land so there is a "last" status to cache.
        next_status(&mut rx_early).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        next_status(&mut rx_early).await;
        let late = broadcaster.subscribe().await;
        // The join-time snapshot is what a new stream sends first, so a
        // late joiner sees the current status immediately instead of
        // waiting for the next poll.
        let update = late
            .cached_status()
            .expect("late joiner must be handed the last polled status");
        assert_eq!(update.status.frequency.to_string(), "123.9750");
        assert_eq!(update.banks, BankMask::from_scanner_response(SCG_ALT));
        // Its channel still receives subsequent live values.
        let mut rx_late = late.resubscribe();
        next_status(&mut rx_late).await;
        drop(early);
        drop(late);
        assert!(wait_until(|| !broadcaster.is_active()).await);
    }

    #[tokio::test]
    async fn set_banks_fast_forwards_the_streamed_mask() {
        // Poll 1: GLG + poll-0 radio refresh. Poll 2: GLG only.
        let mut responses = vec![GLG_OK];
        responses.extend_from_slice(&SCG_CYCLE);
        responses.push(GLG_OK);
        let (broadcaster, _written) = status_broadcaster(&responses);
        let sub = broadcaster.subscribe().await;
        let mut rx = sub.resubscribe();
        // The poll-0 refresh carries the radio's mask.
        let first = next_status(&mut rx).await;
        assert_eq!(first.banks, BankMask::from_scanner_response(SCG_ALT));
        // A successful SetEnabledBanks must reach subscribers on the very
        // next poll, long before the slow radio refresh.
        let all_on = BankMask::new();
        broadcaster.set_banks(&all_on);
        let second = next_status(&mut rx).await;
        assert_eq!(second.banks, all_on);
        drop(sub);
        assert!(wait_until(|| !broadcaster.is_active()).await);
    }

    #[tokio::test]
    async fn radio_side_bank_change_reaches_subscribers_via_refresh() {
        // refresh_every = 1 stands in for the production 120-poll cadence:
        // every poll re-reads the mask, so a bank button pressed on the
        // unit itself shows up on the very next poll.
        let responses: Vec<&str> = [GLG_OK]
            .into_iter()
            .chain(SCG_CYCLE)
            .chain([GLG_OK])
            .chain(["PRG", "SCG,0000000000", "EPG", "KEY"])
            .collect();
        let (client, _written) = mock_client(&responses);
        let broadcaster =
            StatusBroadcaster::new_with_refresh_every(Arc::new(StdMutex::new(client)), 1);
        let sub = broadcaster.subscribe().await;
        let mut rx = sub.resubscribe();
        let first = next_status(&mut rx).await;
        assert_eq!(first.banks, BankMask::from_scanner_response(SCG_ALT));
        // The radio now answers with all banks enabled (a button press);
        // the next poll's refresh must pick it up.
        let second = next_status(&mut rx).await;
        assert_eq!(second.banks, BankMask::from_scanner_response("SCG,0000000000"));
        drop(sub);
        assert!(wait_until(|| !broadcaster.is_active()).await);
    }
}
