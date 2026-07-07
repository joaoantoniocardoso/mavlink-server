use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock timestamp in microseconds since the Unix epoch.
///
/// Each call reads the clock directly so message timestamps and derived delay
/// and jitter stats retain microsecond resolution. On `armv7-*-musl` there is no
/// VDSO fast path, so every call is a real `clock_gettime` syscall.
pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_micros() as u64
}
