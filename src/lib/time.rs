use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

/// Coarse wall-clock timestamp in microseconds since the Unix epoch.
///
/// Reads are a single relaxed atomic load, so the per-message hot path never
/// enters the kernel for the time. On the `armv7-*-musl` target `clock_gettime`
/// has no VDSO fast path, so a real read is a full syscall; the hub, per-driver
/// stats, and [`crate::protocol::Protocol`] together read the clock roughly
/// `4 + endpoints` times per frame, which this collapses to zero syscalls.
///
/// The value is refreshed by a background task every [`REFRESH_INTERVAL`], so it
/// is stale by at most that interval. That granularity is intentional: the only
/// consumers are message timestamps and the derived throughput/`delay` stats,
/// none of which need sub-millisecond precision.
pub fn now_micros() -> u64 {
    let cached = NOW_MICROS.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }

    // Refresher not running yet (called before a runtime exists, e.g. in tests):
    // fall back to a real read and bring the refresher up if a runtime is now
    // available.
    ensure_refresher();
    match NOW_MICROS.load(Ordering::Relaxed) {
        0 => real_now_micros(),
        cached => cached,
    }
}

/// Upper bound on how stale [`now_micros`] can be.
const REFRESH_INTERVAL: Duration = Duration::from_millis(1);

static NOW_MICROS: AtomicU64 = AtomicU64::new(0);
static REFRESHER_STARTED: AtomicBool = AtomicBool::new(false);

/// Spawns the background refresher exactly once, as soon as a Tokio runtime is
/// available. Called from [`now_micros`] on the cold path (while the cache is
/// still zero) so no explicit init from `main` is required; retries on each call
/// until it wins the spawn, so a pre-runtime call cannot permanently disable it.
fn ensure_refresher() {
    if REFRESHER_STARTED.load(Ordering::Relaxed) {
        return;
    }

    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };

    if REFRESHER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    NOW_MICROS.store(real_now_micros(), Ordering::Relaxed);
    handle.spawn(async {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            NOW_MICROS.store(real_now_micros(), Ordering::Relaxed);
        }
    });
}

fn real_now_micros() -> u64 {
    chrono::Utc::now().timestamp_micros() as u64
}
