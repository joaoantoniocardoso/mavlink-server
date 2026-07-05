pub mod driver;
pub mod messages;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use serde::Serialize;

use crate::protocol::Protocol;

#[derive(Clone, Debug, Serialize)]
pub struct AccumulatedStatsInner {
    pub last_message: Option<Arc<Protocol>>,
    pub last_update_us: u64,
    pub messages: u64,
    pub bytes: u64,
    pub delay: u64,
}

/// Lock-free counterpart of [`AccumulatedStatsInner`] used on the per-message
/// hot path. Writers call [`AtomicAccumulatedStats::update`] without any lock or
/// `.await`; readers take a [`AtomicAccumulatedStats::snapshot`] (done once per
/// stats period).
///
/// Only the counters needed to derive [`crate::stats::StatsInner`] are tracked.
/// The `last_message` payload is intentionally not retained here: it was
/// write-only in the previous accumulators (never read by any consumer), so
/// cloning the `Arc<Protocol>` on every message was pure overhead.
#[derive(Debug, Default)]
pub struct AtomicAccumulatedStats {
    last_update_us: AtomicU64,
    messages: AtomicU64,
    bytes: AtomicU64,
    delay: AtomicU64,
}

impl AtomicAccumulatedStats {
    pub fn update(&self, message: &Arc<Protocol>) {
        let now = crate::time::now_micros();

        self.last_update_us.store(now, Ordering::Relaxed);
        self.bytes
            .fetch_add(message.size() as u64, Ordering::Relaxed);
        self.messages.fetch_add(1, Ordering::Relaxed);
        self.delay
            .fetch_add(now.wrapping_sub(message.timestamp), Ordering::Relaxed);
    }

    /// Returns `None` until at least one message has been recorded, matching the
    /// `Option` semantics of the previous per-driver/hub accumulators.
    pub fn snapshot(&self) -> Option<AccumulatedStatsInner> {
        let messages = self.messages.load(Ordering::Relaxed);
        if messages == 0 {
            return None;
        }

        Some(AccumulatedStatsInner {
            last_message: None,
            last_update_us: self.last_update_us.load(Ordering::Relaxed),
            messages,
            bytes: self.bytes.load(Ordering::Relaxed),
            delay: self.delay.load(Ordering::Relaxed),
        })
    }

    pub fn reset(&self) {
        self.messages.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        self.delay.store(0, Ordering::Relaxed);
    }
}

impl Default for AccumulatedStatsInner {
    fn default() -> Self {
        Self {
            last_message: None,
            last_update_us: crate::time::now_micros(),
            messages: 0,
            bytes: 0,
            delay: 0,
        }
    }
}

impl AccumulatedStatsInner {
    fn new(message: &Arc<Protocol>) -> Self {
        let now = crate::time::now_micros();
        Self {
            last_message: Some(message.clone()),
            last_update_us: now,
            messages: 1,
            bytes: message.size() as u64,
            delay: now - message.timestamp,
        }
    }

    pub fn update(&mut self, message: &Arc<Protocol>) {
        self.last_message = Some(message.clone());
        self.last_update_us = crate::time::now_micros();
        self.bytes = self.bytes.wrapping_add(message.size() as u64);
        self.messages = self.messages.wrapping_add(1);
        self.delay = self
            .delay
            .wrapping_add(self.last_update_us - message.timestamp);
    }
}
