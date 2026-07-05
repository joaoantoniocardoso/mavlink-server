use std::{ops::Div, sync::Arc};

use anyhow::{Context, Result, anyhow};
use indexmap::IndexMap;
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::*;

use crate::{
    cli,
    drivers::{Driver, DriverInfo},
    hub::{HubCommand, dataplane::DataPlane},
    protocol::Protocol,
    stats::{
        accumulated::{
            AccumulatedStatsInner, driver::AccumulatedDriversStats,
            messages::AccumulatedHubMessagesStats,
        },
        driver::DriverUuid,
    },
};

#[allow(dead_code)]
pub struct HubActor {
    drivers: IndexMap<DriverUuid, DriverRunner>,
    data_plane: DataPlane,
    sink_count: usize,
    component_id: Arc<RwLock<u8>>,
    system_id: Arc<RwLock<u8>>,
    heartbeat_task: tokio::task::JoinHandle<Result<()>>,
}

#[derive(Debug)]
struct DriverRunner {
    uuid: DriverUuid,
    driver: Arc<dyn Driver>,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Drop for DriverRunner {
    #[instrument(level = "debug")]
    fn drop(&mut self) {
        debug!("Aborting runner task for driver: {:?}", self.uuid);

        self.task.abort();
    }
}

impl HubActor {
    pub async fn start(mut self, mut receiver: mpsc::Receiver<HubCommand>) {
        while let Some(command) = receiver.recv().await {
            match command {
                HubCommand::AddDriver { driver, response } => {
                    let result = self.add_driver(driver).await;
                    let _ = response.send(result);
                }
                HubCommand::RemoveDriver { uuid, response } => {
                    let result = self.remove_driver(uuid).await;
                    let _ = response.send(result);
                }
                HubCommand::GetDrivers { response } => {
                    let drivers = self.drivers().await;
                    let _ = response.send(drivers);
                }
                HubCommand::GetSender { response } => {
                    let _ = response.send(self.get_sender());
                }
                HubCommand::RegisterSink {
                    loopback_origin,
                    response,
                } => {
                    let sink = match loopback_origin {
                        Some(origin) => self.data_plane.register_sink_with_origin(origin),
                        None => self.data_plane.register_sink(),
                    };
                    self.sink_count += 1;
                    let _ = response.send(sink);
                }
                HubCommand::GetDriversStats { response } => {
                    let drivers_stats = self.get_drivers_stats().await;
                    let _ = response.send(drivers_stats);
                }
                HubCommand::GetHubStats { response } => {
                    let hub_stats = self.get_hub_stats().await;
                    let _ = response.send(hub_stats);
                }
                HubCommand::GetHubMessagesStats { response } => {
                    let hub_messages_stats = self.get_hub_messages_stats().await;
                    let _ = response.send(hub_messages_stats);
                }
                HubCommand::ResetAllStats { response } => {
                    let _ = response.send(self.reset_all_stats().await);
                }
            }
        }
    }

    #[instrument(level = "debug")]
    pub fn new(
        buffer_size: usize,
        component_id: Arc<RwLock<u8>>,
        system_id: Arc<RwLock<u8>>,
        frequency: Arc<RwLock<f32>>,
    ) -> Self {
        let data_plane = DataPlane::new(buffer_size);

        let heartbeat_task = tokio::spawn({
            let data_plane = data_plane.clone();
            let component_id = component_id.clone();
            let system_id = system_id.clone();
            let frequency = frequency.clone();

            Self::heartbeat_task(data_plane, component_id, system_id, frequency)
        });

        Self {
            drivers: IndexMap::new(),
            data_plane,
            sink_count: 0,
            component_id,
            system_id,
            heartbeat_task,
        }
    }

    #[instrument(level = "debug", skip(self, driver))]
    async fn add_driver(&mut self, driver: Arc<dyn Driver>) -> Result<DriverUuid> {
        let uuid = *driver.uuid();

        let data_plane = self.get_sender();

        let task = tokio::spawn({
            let driver = driver.clone();

            async move {
                while let Err(error) = driver.run(data_plane.clone()).await {
                    error!(
                        "Driver runner ended with error. Restarting in 1 second... Error: {error:?}"
                    );

                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }

                info!("Driver runner ended.");

                Ok(())
            }
        });

        let driver_runner = DriverRunner { uuid, driver, task };

        if self.drivers.insert(uuid, driver_runner).is_some() {
            return Err(anyhow!(
                "Failed addinng driver: uuid {uuid:?} is already present"
            ));
        }

        Ok(uuid)
    }

    #[instrument(level = "debug", skip(self))]
    async fn remove_driver(&mut self, uuid: DriverUuid) -> Result<()> {
        self.drivers
            .swap_remove(&uuid)
            .context("Driver uuid {uuid:?} not found")?;
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    async fn drivers(&self) -> IndexMap<DriverUuid, Box<dyn DriverInfo>> {
        self.drivers
            .iter()
            .map(|(&id, driver_runner)| (id, driver_runner.driver.info()))
            .collect()
    }

    async fn heartbeat_task(
        data_plane: DataPlane,
        system_id: Arc<RwLock<u8>>,
        component_id: Arc<RwLock<u8>>,
        frequency: Arc<RwLock<f32>>,
    ) -> Result<()> {
        let message = mavlink::dialects::ardupilotmega::MavMessage::HEARTBEAT(
            mavlink::dialects::ardupilotmega::HEARTBEAT_DATA {
                custom_mode: 0,
                mavtype: mavlink::dialects::ardupilotmega::MavType::MAV_TYPE_ONBOARD_CONTROLLER, // or MAV_TYPE_ONBOARD_GENERIC
                autopilot: mavlink::dialects::ardupilotmega::MavAutopilot::MAV_AUTOPILOT_INVALID, // or MAV_AUTOPILOT_GENERIC?
                base_mode: mavlink::dialects::ardupilotmega::MavModeFlag::empty(),
                system_status: mavlink::dialects::ardupilotmega::MavState::MAV_STATE_STANDBY,
                mavlink_version: 0x3,
            },
        );

        let burst_size = 5;
        let mut burst_msgs_counter = 0;
        let mut do_burst = cli::send_initial_heartbeats();

        let origin: Arc<str> = Arc::from("");

        loop {
            let duration = if do_burst {
                if burst_msgs_counter == burst_size {
                    do_burst = false;
                }

                tokio::time::Duration::from_millis(100)
            } else {
                let frequency = *frequency.read().await;

                if frequency <= 0f32 {
                    // Avoid spin lock
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }

                tokio::time::Duration::from_secs_f32(1f32.div(frequency))
            };

            tokio::time::sleep(duration).await;

            if data_plane.receiver_count().eq(&0) {
                continue; // Don't try to send if the channel has no subscribers yet
            }

            let header = mavlink::MavHeader {
                system_id: *system_id.read().await,
                component_id: *component_id.read().await,
                ..Default::default()
            };

            let message = Arc::new(Protocol::from_mavlink_raw(
                header,
                &message,
                Arc::clone(&origin),
            ));

            crate::hub::accumulate_hub_message(&message);

            if let Err(_error) = data_plane.publish(message) {
                error!("Failed to send HEARTBEAT message: no receivers");
            }

            if do_burst && burst_msgs_counter < burst_size {
                burst_msgs_counter += 1;
            }
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn get_sender(&self) -> DataPlane {
        self.data_plane.clone()
    }

    #[instrument(level = "debug", skip(self))]
    async fn get_drivers_stats(&self) -> AccumulatedDriversStats {
        let mut drivers_stats = IndexMap::with_capacity(self.drivers.len());
        for (_id, driver_runner) in self.drivers.iter() {
            let stats = driver_runner.driver.stats().await;
            let uuid = *driver_runner.driver.uuid();

            drivers_stats.insert(uuid, stats);
        }

        drivers_stats
    }

    #[instrument(level = "debug", skip(self))]
    async fn get_hub_stats(&self) -> AccumulatedStatsInner {
        crate::hub::hub_stats_snapshot()
    }

    #[instrument(level = "debug", skip(self))]
    async fn get_hub_messages_stats(&self) -> AccumulatedHubMessagesStats {
        crate::hub::hub_messages_stats_snapshot()
    }

    #[instrument(level = "debug", skip(self))]
    async fn reset_all_stats(&mut self) -> Result<()> {
        for (_id, driver_runner) in self.drivers.iter() {
            driver_runner.driver.reset_stats().await;
        }

        crate::hub::reset_hub_accumulators();

        Ok(())
    }
}
