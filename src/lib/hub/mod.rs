mod actor;
pub mod dataplane;
mod protocol;

use std::sync::{Arc, Mutex, OnceLock};

use actor::HubActor;
use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use protocol::HubCommand;
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::{
    cli,
    drivers::{Driver, DriverInfo},
    protocol::Protocol,
    runtime::{self, PlaneHandles},
    stats::{
        accumulated::{
            AccumulatedStatsInner, AtomicAccumulatedStats,
            driver::AccumulatedDriversStats,
            messages::{AccumulatedHubMessagesStats, AtomicHubMessagesStats},
        },
        driver::DriverUuid,
    },
};

static HUB: OnceLock<Hub> = OnceLock::new();

static NAMES_MAP: OnceLock<Arc<Mutex<IndexMap<String, u32>>>> = OnceLock::new();

static HUB_STATS: OnceLock<AtomicAccumulatedStats> = OnceLock::new();
static HUB_MESSAGES_STATS: OnceLock<AtomicHubMessagesStats> = OnceLock::new();

fn hub_stats_accumulator() -> &'static AtomicAccumulatedStats {
    HUB_STATS.get_or_init(AtomicAccumulatedStats::default)
}

fn hub_messages_stats_accumulator() -> &'static AtomicHubMessagesStats {
    HUB_MESSAGES_STATS.get_or_init(AtomicHubMessagesStats::default)
}

/// Accumulates a message into the hub-level stats. Called inline from every path
/// that publishes to the hub broadcast, replacing the former dedicated stats
/// subscriber task.
pub fn accumulate_hub_message(message: &Arc<Protocol>) {
    hub_stats_accumulator().update(message);
    hub_messages_stats_accumulator().update(message);
}

pub(crate) fn hub_stats_snapshot() -> AccumulatedStatsInner {
    hub_stats_accumulator().snapshot().unwrap_or_default()
}

pub(crate) fn hub_messages_stats_snapshot() -> AccumulatedHubMessagesStats {
    hub_messages_stats_accumulator().snapshot()
}

pub(crate) fn reset_hub_accumulators() {
    hub_stats_accumulator().reset();
    hub_messages_stats_accumulator().reset();
}

struct Hub {
    sender: mpsc::Sender<HubCommand>,
    _task: Arc<Mutex<tokio::task::JoinHandle<()>>>,
}

pub fn init() {
    hub();
}

fn hub() -> &'static Hub {
    HUB.get_or_init(|| {
        let handles = runtime::handles();
        Hub::new(
            10000,
            Arc::new(RwLock::new(cli::mavlink_system_id())),
            Arc::new(RwLock::new(cli::mavlink_component_id())),
            Arc::new(RwLock::new(cli::mavlink_heartbeat_frequency())),
            handles,
        )
    })
}

impl Hub {
    fn new(
        buffer_size: usize,
        component_id: Arc<RwLock<u8>>,
        system_id: Arc<RwLock<u8>>,
        frequency: Arc<RwLock<f32>>,
        handles: PlaneHandles,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(32);
        let hub = HubActor::new(
            buffer_size,
            component_id,
            system_id,
            frequency,
            handles.data,
        );
        let _task = Arc::new(Mutex::new(runtime::spawn_control(hub.start(receiver))));

        Self { sender, _task }
    }
}

pub async fn add_driver(driver: Arc<dyn Driver>) -> Result<DriverUuid> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::AddDriver {
            driver,
            response: response_tx,
        })
        .await?;
    response_rx.await?
}

pub async fn remove_driver(uuid: DriverUuid) -> Result<()> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::RemoveDriver {
            uuid,
            response: response_tx,
        })
        .await?;
    response_rx.await?
}

pub async fn drivers() -> Result<IndexMap<DriverUuid, Box<dyn DriverInfo>>> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::GetDrivers {
            response: response_tx,
        })
        .await?;
    let res = response_rx.await?;
    Ok(res)
}

pub use dataplane::{DataPlane, PublishError, SinkReceiver, SinkRecvError};

pub async fn data_plane() -> Result<DataPlane> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::GetSender {
            response: response_tx,
        })
        .await?;
    response_rx.await.map_err(|_| anyhow!("Hub actor dropped"))
}

pub async fn register_sink(loopback_origin: Option<Arc<str>>) -> Result<SinkReceiver> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::RegisterSink {
            loopback_origin,
            response: response_tx,
        })
        .await?;
    response_rx.await.map_err(|_| anyhow!("Hub actor dropped"))
}

pub async fn register_control_sink(
    loopback_origin: Option<Arc<str>>,
) -> Result<mpsc::Receiver<Arc<Protocol>>> {
    let (tx, rx) = mpsc::channel(1024);
    let mut sink = register_sink(loopback_origin).await?;

    runtime::spawn_data(async move {
        loop {
            let Some(message) = sink.recv_next().await else {
                break;
            };

            if tx.send(message).await.is_err() {
                break;
            }
        }
    });

    Ok(rx)
}

pub async fn sender() -> Result<DataPlane> {
    data_plane().await
}

pub async fn drivers_stats() -> Result<AccumulatedDriversStats> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::GetDriversStats {
            response: response_tx,
        })
        .await?;
    let res = response_rx.await?;
    Ok(res)
}

pub async fn hub_stats() -> Result<AccumulatedStatsInner> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::GetHubStats {
            response: response_tx,
        })
        .await?;
    let res = response_rx.await?;
    Ok(res)
}

pub async fn hub_messages_stats() -> Result<AccumulatedHubMessagesStats> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::GetHubMessagesStats {
            response: response_tx,
        })
        .await?;
    let res = response_rx.await?;
    Ok(res)
}

pub async fn reset_all_stats() -> Result<()> {
    let (response_tx, response_rx) = oneshot::channel();
    hub()
        .sender
        .send(HubCommand::ResetAllStats {
            response: response_tx,
        })
        .await?;
    response_rx.await?
}

pub fn generate_new_default_name(prefix: &str) -> Result<String> {
    let names_map = NAMES_MAP.get_or_init(|| Arc::new(Mutex::new(IndexMap::new())));
    let mut generated_names = names_map.lock().unwrap();

    let num = generated_names
        .entry(prefix.to_owned())
        .and_modify(|n| {
            if *n == u32::MAX {
                // still panic-free; the Err below will be returned
            } else {
                *n = n.saturating_add(1);
            }
        })
        .or_insert(0);

    if *num == u32::MAX {
        return Err(anyhow!(
            "No indexes are left for the given prefix {prefix:?}. Current index: {num:?}."
        ));
    }

    Ok(format!("{prefix}{num}"))
}
