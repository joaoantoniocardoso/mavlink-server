use std::{io, sync::Mutex, time::Instant};

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use serde::Serialize;

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ResourceUsage {
    pub run_time: u64,
    pub cpu_usage: f32,
    pub memory_usage_bytes: u64,
    pub total_memory_bytes: u64,
}

struct CpuSample {
    utime: u64,
    stime: u64,
    at: Instant,
}

lazy_static! {
    static ref LAST_CPU: Mutex<Option<CpuSample>> = Mutex::new(None);
    static ref NUM_CPUS: f32 = std::thread::available_parallelism()
        .map(|cpus| cpus.get() as f32)
        .unwrap_or(1.0);
}

const CLOCK_TICKS_PER_SEC: f64 = 100.0;
const PAGE_SIZE: u64 = 4096;

pub fn usage() -> Result<ResourceUsage> {
    let (utime, stime, starttime, rss_pages) = read_proc_self_stat()?;
    let total_memory_bytes = read_mem_total_bytes()?;
    let run_time = process_run_time_secs(starttime)?;
    let cpu_usage = cpu_usage_since_last(utime, stime);

    Ok(ResourceUsage {
        run_time,
        cpu_usage,
        memory_usage_bytes: rss_pages * PAGE_SIZE,
        total_memory_bytes,
    })
}

fn read_proc_self_stat() -> Result<(u64, u64, u64, u64)> {
    let stat = std::fs::read_to_string("/proc/self/stat").context("read /proc/self/stat")?;
    let after_comm = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.trim())
        .context("parse /proc/self/stat comm field")?;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime = parse_stat_field(&fields, 11, "utime")?;
    let stime = parse_stat_field(&fields, 12, "stime")?;
    let starttime = parse_stat_field(&fields, 19, "starttime")?;
    let rss_pages = parse_stat_field(&fields, 21, "rss")?;
    Ok((utime, stime, starttime, rss_pages))
}

fn parse_stat_field(fields: &[&str], index: usize, name: &str) -> Result<u64> {
    fields
        .get(index)
        .with_context(|| format!("missing {name} in /proc/self/stat"))?
        .parse()
        .with_context(|| format!("invalid {name} in /proc/self/stat"))
}

fn read_mem_total_bytes() -> Result<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").context("read /proc/meminfo")?;
    for line in meminfo.lines() {
        if let Some(kb) = line.strip_prefix("MemTotal:") {
            let kb = kb
                .trim()
                .strip_suffix(" kB")
                .unwrap_or(kb.trim())
                .parse::<u64>()
                .context("parse MemTotal")?;
            return Ok(kb * 1024);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "MemTotal not found").into())
}

fn read_uptime_secs() -> Result<u64> {
    let uptime = std::fs::read_to_string("/proc/uptime").context("read /proc/uptime")?;
    let seconds = uptime
        .split_whitespace()
        .next()
        .context("empty /proc/uptime")?
        .parse::<f64>()
        .context("parse /proc/uptime")?;
    Ok(seconds as u64)
}

fn process_run_time_secs(starttime: u64) -> Result<u64> {
    let uptime = read_uptime_secs()?;
    let start_secs = (starttime as f64 / CLOCK_TICKS_PER_SEC) as u64;
    Ok(uptime.saturating_sub(start_secs))
}

fn cpu_usage_since_last(utime: u64, stime: u64) -> f32 {
    let mut last = LAST_CPU.lock().unwrap();
    let now = Instant::now();
    let total_ticks = utime + stime;

    let usage = if let Some(prev) = last.as_ref() {
        let elapsed = now.duration_since(prev.at).as_secs_f64();
        if elapsed > 0.0 {
            let delta_ticks = total_ticks.saturating_sub(prev.utime + prev.stime) as f64;
            ((delta_ticks / CLOCK_TICKS_PER_SEC) / elapsed * 100.0 / f64::from(*NUM_CPUS)) as f32
        } else {
            0.0
        }
    } else {
        0.0
    };

    *last = Some(CpuSample {
        utime,
        stime,
        at: now,
    });

    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_proc_self_stat_fields() {
        let (utime, stime, starttime, rss) = read_proc_self_stat().unwrap();
        assert!(utime > 0 || stime >= 0);
        assert!(starttime > 0);
        assert!(rss > 0);
    }

    #[test]
    fn usage_returns_nonzero_memory() {
        let usage = usage().unwrap();
        assert!(usage.memory_usage_bytes > 0);
        assert!(usage.total_memory_bytes > 0);
    }
}
