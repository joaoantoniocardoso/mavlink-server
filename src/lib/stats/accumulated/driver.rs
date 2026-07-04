use std::sync::Arc;

use indexmap::IndexMap;
use serde::Serialize;

use super::{AccumulatedStatsInner, AtomicAccumulatedStats};
use crate::{drivers::DriverInfo, protocol::Protocol, stats::driver::DriverUuid};

pub type AccumulatedDriversStats = IndexMap<DriverUuid, AccumulatedDriverStats>;

/// Lock-free per-driver stats updated on the hot path. Drivers hold this
/// directly (behind an `Arc`) instead of an `Arc<RwLock<AccumulatedDriverStats>>`;
/// readers take a [`AtomicDriverStats::snapshot`] once per stats period.
#[derive(Debug)]
pub struct AtomicDriverStats {
    pub name: Arc<String>,
    pub driver_type: &'static str,
    pub input: AtomicAccumulatedStats,
    pub output: AtomicAccumulatedStats,
}

impl AtomicDriverStats {
    pub fn new(name: Arc<String>, info: &dyn DriverInfo) -> Self {
        Self {
            name,
            driver_type: info.name(),
            input: AtomicAccumulatedStats::default(),
            output: AtomicAccumulatedStats::default(),
        }
    }

    pub fn update_input(&self, message: &Arc<Protocol>) {
        self.input.update(message);
    }

    pub fn update_output(&self, message: &Arc<Protocol>) {
        self.output.update(message);
    }

    pub fn snapshot(&self) -> AccumulatedDriverStats {
        AccumulatedDriverStats {
            name: self.name.clone(),
            driver_type: self.driver_type,
            stats: AccumulatedDriverStatsInner {
                input: self.input.snapshot(),
                output: self.output.snapshot(),
            },
        }
    }

    pub fn reset(&self) {
        self.input.reset();
        self.output.reset();
    }
}

#[async_trait::async_trait]
pub trait AccumulatedDriverStatsProvider {
    async fn stats(&self) -> AccumulatedDriverStats;
    async fn reset_stats(&self);
}

#[derive(Debug, Clone, Serialize)]
pub struct AccumulatedDriverStats {
    pub name: Arc<String>,
    pub driver_type: &'static str,
    pub stats: AccumulatedDriverStatsInner,
}

impl AccumulatedDriverStats {
    pub fn new(name: Arc<String>, info: &dyn DriverInfo) -> Self {
        Self {
            name,
            driver_type: info.name(),
            stats: AccumulatedDriverStatsInner::default(),
        }
    }
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct AccumulatedDriverStatsInner {
    pub input: Option<AccumulatedStatsInner>,
    pub output: Option<AccumulatedStatsInner>,
}

impl AccumulatedDriverStatsInner {
    pub fn update_input(&mut self, message: &Arc<Protocol>) {
        if let Some(stats) = self.input.as_mut() {
            stats.update(message);
        } else {
            self.input.replace(AccumulatedStatsInner::new(message));
        }
    }

    pub fn update_output(&mut self, message: &Arc<Protocol>) {
        if let Some(stats) = self.output.as_mut() {
            stats.update(message);
        } else {
            self.output.replace(AccumulatedStatsInner::new(message));
        }
    }
}
