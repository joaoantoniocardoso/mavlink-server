use anyhow::Result;
use tokio::sync::oneshot;

use crate::stats::DriverStats;

pub enum StatsCommand {
    SetPeriod {
        period: tokio::time::Duration,
        response: oneshot::Sender<Result<()>>,
    },
    Reset {
        response: oneshot::Sender<Result<()>>,
    },
    GetDriversStats {
        response: oneshot::Sender<Result<Vec<(String, DriverStats)>>>,
    },
}
