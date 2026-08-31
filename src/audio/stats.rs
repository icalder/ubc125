//! Audio pipeline counters so buffering is visible (B10).
//!
//! Every change in BUFFERING-FIXES.md is "measure first, tune with numbers":
//! this module holds the counters the §6 harness reports on — what the pump
//! emits (chunk count, cluster durations), what each subscriber loses
//! (`Lagged` drops, R1/R3), and what capture loses (ALSA xruns, a full
//! source channel). The 5-second reporter in `src/cmd/serve.rs` logs a
//! [`AudioStats::snapshot`] window while capture is active.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Counters for one `Listen` subscriber: the chunks the fan-out broadcast
/// channel dropped for it because it could not keep up (R3: dropped, never
/// stalling the pipeline).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubscriberStats {
    /// Number of `Lagged` events observed by this subscriber.
    pub lag_events: u64,
    /// Chunks dropped by `Lagged(n)` for this subscriber (sum of `n`).
    pub lag_drops: u64,
}

/// Process-lifetime counters for the audio pipeline.
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Media chunks emitted by the pump (what subscribers can receive).
    pub chunks_produced: AtomicU64,
    /// Sum of cluster durations in ms over `chunks_produced` chunks.
    pub cluster_ms_sum: AtomicU64,
    /// Smallest cluster duration seen so far (0 = no chunks yet).
    pub cluster_ms_min: AtomicU64,
    /// ALSA xruns observed by the capture task (B2 watch item).
    pub xruns: AtomicU64,
    /// Times the capture task blocked more than one frame (20 ms) waiting
    /// for the pump to drain the source channel — a full source channel.
    pub channel_stalls: AtomicU64,
    next_subscriber: AtomicU64,
    subscribers: Mutex<HashMap<u64, SubscriberStats>>,
}

impl AudioStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener; returns its id for the per-subscriber counters.
    pub fn subscriber_started(&self) -> u64 {
        let id = self.next_subscriber.fetch_add(1, Ordering::Relaxed) + 1;
        self.subscribers
            .lock()
            .unwrap()
            .insert(id, SubscriberStats::default());
        id
    }

    /// Record a `Lagged(n)` for subscriber `id`: `n` chunks were dropped for
    /// it by the broadcast channel.
    pub fn subscriber_dropped(&self, id: u64, chunks: u64) {
        if let Some(stats) = self.subscribers.lock().unwrap().get_mut(&id) {
            stats.lag_events += 1;
            stats.lag_drops += chunks;
        }
    }

    /// Remove a subscriber (its stream ended).
    pub fn subscriber_stopped(&self, id: u64) {
        self.subscribers.lock().unwrap().remove(&id);
    }

    /// Count one media chunk emitted by the pump. `duration_ms` of 0 means
    /// the duration could not be parsed and is omitted from the duration
    /// sums (the chunk itself is still counted).
    pub fn record_chunk(&self, duration_ms: u64) {
        self.chunks_produced.fetch_add(1, Ordering::Relaxed);
        if duration_ms == 0 {
            return;
        }
        self.cluster_ms_sum.fetch_add(duration_ms, Ordering::Relaxed);
        let mut min = self.cluster_ms_min.load(Ordering::Relaxed);
        while min == 0 || duration_ms < min {
            match self
                .cluster_ms_min
                .compare_exchange_weak(min, duration_ms, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => min = actual,
            }
        }
    }

    /// A point-in-time copy for the 5-second reporter.
    pub fn snapshot(&self) -> AudioStatsSnapshot {
        let subscribers = self
            .subscribers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, stats)| stats.lag_events > 0)
            .map(|(id, stats)| (*id, stats.clone()))
            .collect();
        AudioStatsSnapshot {
            chunks_produced: self.chunks_produced.load(Ordering::Relaxed),
            cluster_ms_sum: self.cluster_ms_sum.load(Ordering::Relaxed),
            cluster_ms_min: {
                let min = self.cluster_ms_min.load(Ordering::Relaxed);
                if min == 0 {
                    None
                } else {
                    Some(min)
                }
            },
            xruns: self.xruns.load(Ordering::Relaxed),
            channel_stalls: self.channel_stalls.load(Ordering::Relaxed),
            subscribers,
        }
    }
}

/// A point-in-time copy of [`AudioStats`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStatsSnapshot {
    pub chunks_produced: u64,
    pub cluster_ms_sum: u64,
    pub cluster_ms_min: Option<u64>,
    pub xruns: u64,
    pub channel_stalls: u64,
    /// Subscribers that have dropped chunks, with their ids.
    pub subscribers: Vec<(u64, SubscriberStats)>,
}

impl AudioStatsSnapshot {
    /// True if any counter moved since `other` (used by the reporter to stay
    /// quiet when nothing is happening).
    pub fn moved(&self, other: &AudioStatsSnapshot) -> bool {
        self.chunks_produced != other.chunks_produced
            || self.xruns != other.xruns
            || self.channel_stalls != other.channel_stalls
            || self.subscribers != other.subscribers
    }
}

/// A shared handle to [`AudioStats`] (the capture task, the pump and the
/// listeners all record into the same instance).
pub type SharedAudioStats = Arc<AudioStats>;
