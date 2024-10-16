mod actor;
pub mod driver;
mod protocol;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use actor::StatsActor;
use protocol::StatsCommand;

use crate::hub::Hub;

#[derive(Debug, Clone)]
pub struct DriverStats {
    input: Option<DriverStatsInner>,
    output: Option<DriverStatsInner>,
}

#[derive(Debug, Clone)]
pub struct DriverStatsInner {
    last_message_time: u64,

    total_bytes: u64,
    bytes_per_second: f64,
    average_bytes_per_second: f64,

    total_messages: u64,
    messages_per_second: f64,
    average_messages_per_second: f64,

    delay: f64,
    jitter: f64,
}

#[derive(Clone)]
pub struct Stats {
    sender: mpsc::Sender<StatsCommand>,
    task: Arc<Mutex<tokio::task::JoinHandle<()>>>,
}

impl Stats {
    pub async fn new(hub: Hub, update_period: tokio::time::Duration) -> Self {
        let (sender, receiver) = mpsc::channel(32);
        let actor = StatsActor::new(hub, update_period).await;
        let task = Arc::new(Mutex::new(tokio::spawn(actor.start(receiver))));
        Self { sender, task }
    }

    pub async fn driver_stats(&mut self) -> Result<Vec<(String, DriverStats)>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(StatsCommand::GetDriversStats {
                response: response_tx,
            })
            .await?;
        response_rx.await?
    }

    pub async fn set_period(&mut self, period: tokio::time::Duration) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(StatsCommand::SetPeriod {
                period,
                response: response_tx,
            })
            .await?;
        response_rx.await?
    }

    pub async fn reset(&mut self) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(StatsCommand::Reset {
                response: response_tx,
            })
            .await?;
        response_rx.await?
    }
}
