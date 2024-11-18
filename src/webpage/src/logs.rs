use chrono::{DateTime, Utc};
use ringbuffer::{ConstGenericRingBuffer, RingBuffer};
use serde::Deserialize;

const BUFFERS_CAPACITY: usize = 1024;

type Logs = ConstGenericRingBuffer<LogSample, BUFFERS_CAPACITY>;

pub enum LogLevel {
    Info,
    Debug,
    Warn,
    Error,
    Trace,
}

struct LogSample {
    time: Utc,
    level: LogLevel,
    message: String,
}
