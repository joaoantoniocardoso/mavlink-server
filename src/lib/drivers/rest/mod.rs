pub mod autopilot;
pub mod control;
pub mod data;
pub mod mavftp;

use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws;
use tokio::sync::broadcast;
use tracing::*;

use crate::{
    callbacks::{Callbacks, MessageCallback},
    drivers::{
        Driver, DriverInfo,
        generic_tasks::{SendReceiveContext, spawn_message_observers},
    },
    hub::{DataPlane, register_sink},
    mavlink_json::MAVLinkJSON,
    protocol::Protocol,
    stats::{
        accumulated::driver::{
            AccumulatedDriverStats, AccumulatedDriverStatsProvider, AtomicDriverStats,
        },
        driver::DriverUuid,
    },
    web::routes::v1::rest::websocket,
};

#[derive(Debug)]
pub struct Rest {
    name: arc_swap::ArcSwap<String>,
    uuid: DriverUuid,
    on_message_input: Callbacks<Arc<Protocol>>,
    on_message_output: Callbacks<Arc<Protocol>>,
    stats: Arc<AtomicDriverStats>,
}

pub struct RestBuilder(Rest);

impl RestBuilder {
    pub fn build(self) -> Rest {
        self.0
    }

    pub fn on_message_input<C>(self, callback: C) -> Self
    where
        C: MessageCallback<Arc<Protocol>>,
    {
        self.0.on_message_input.add_callback(callback.into_boxed());
        self
    }

    pub fn on_message_output<C>(self, callback: C) -> Self
    where
        C: MessageCallback<Arc<Protocol>>,
    {
        self.0.on_message_output.add_callback(callback.into_boxed());
        self
    }
}

impl Rest {
    #[instrument(level = "debug")]
    pub fn builder(name: &str) -> RestBuilder {
        let name = Arc::new(name.to_string());

        RestBuilder(Self {
            name: arc_swap::ArcSwap::new(name.clone()),
            uuid: Self::generate_uuid(&name),
            on_message_input: Callbacks::default(),
            on_message_output: Callbacks::default(),
            stats: Arc::new(AtomicDriverStats::new(name, &RestInfo)),
        })
    }

    #[instrument(level = "debug", skip_all)]
    async fn receive_task(
        context: &SendReceiveContext,
        ws_receiver: &mut broadcast::Receiver<String>,
    ) -> Result<()> {
        let origin: Arc<str> = Arc::from("Ws");

        while let Ok(message) = ws_receiver.recv().await {
            let Ok(content) = json5::from_str::<
                MAVLinkJSON<mavlink::dialects::ardupilotmega::MavMessage>,
            >(&message) else {
                warn!("Failed to parse message, not a valid MAVLinkMessage: {message:?}");
                continue;
            };

            let bus_message = Arc::new(Protocol::from_mavlink_raw(
                content.header.inner,
                &content.message,
                Arc::clone(&origin),
            ));

            trace!("Received message: {bus_message:?}");

            context.stats.update_input(&bus_message);

            if let Err(error) = context.filter_message_input.apply_all(bus_message.clone()) {
                debug!("Dropping message: filter_message_input returned error: {error:?}");
                continue;
            }

            crate::hub::accumulate_hub_message(&bus_message);

            if let Err(error) = context.data_plane.publish(bus_message) {
                error!("Failed to send message to hub: {error:?}");
                continue;
            }

            trace!("Message sent to hub");
        }

        debug!("Driver receiver task stopped!");

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    async fn control_send_task(
        context: &SendReceiveContext,
        control_receiver: &mut broadcast::Receiver<mavlink::dialects::ardupilotmega::MavMessage>,
    ) -> Result<()> {
        let header = mavlink::MavHeader {
            system_id: 255, // default system_id for gcs
            component_id: mavlink::dialects::ardupilotmega::MavComponent::MAV_COMP_ID_MISSIONPLANNER
                as u8,
            ..Default::default()
        };

        let origin: Arc<str> = Arc::from("Control");

        while let Ok(message) = control_receiver.recv().await {
            let bus_message = Arc::new(Protocol::from_mavlink_raw(
                header,
                &message,
                Arc::clone(&origin),
            ));

            trace!("Received message: {bus_message:?}");

            context.stats.update_input(&bus_message);

            if let Err(error) = context.filter_message_input.apply_all(bus_message.clone()) {
                debug!("Dropping message: filter_message_input returned error: {error:?}");
                continue;
            }

            crate::hub::accumulate_hub_message(&bus_message);

            if let Err(error) = context.data_plane.publish(bus_message) {
                error!("Failed to send message to hub: {error:?}");
                continue;
            }

            trace!("Message sent to hub");
        }

        debug!("Driver sender task stopped!");

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    async fn send_task(context: &SendReceiveContext) -> Result<()> {
        let mut sink = register_sink(Some(Arc::from("Ws"))).await?;

        spawn_message_observers(
            context.data_plane.clone(),
            context.on_message_output.clone(),
            Some(Arc::from("Ws")),
        );

        let origin = "Ws";
        let uuid = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, origin.as_bytes());

        'mainloop: loop {
            let Some(message) = sink.recv_next().await else {
                error!("Hub channel closed!");
                break;
            };

            context.stats.update_output(&message);

            if let Err(error) = context.filter_message_output.apply_all(message.clone()) {
                debug!("Dropping message: filter_message_output returned error: {error:?}");
                continue 'mainloop;
            }

            let message = message.clone();
            tokio::spawn(async move {
                let Ok(mavlink_json) = message
                    .to_mavlink_json::<mavlink::dialects::ardupilotmega::MavMessage>()
                    .await
                    .inspect_err(|error| debug!("Failed converting message to json: {error:?}"))
                else {
                    return;
                };

                let header = mavlink_json.header.inner;
                let mavlink_message = mavlink_json.message.clone();

                let json_text: std::borrow::Cow<'_, str> = match message.json() {
                    Some(bytes) => std::borrow::Cow::Borrowed(std::str::from_utf8(bytes).unwrap()),
                    None => std::borrow::Cow::Owned(parse_query(&mavlink_json)),
                };

                data::update((mavlink_json.header, mavlink_json.message));

                control::update((header, mavlink_message)).await;

                if websocket::has_clients().await {
                    websocket::broadcast(uuid, ws::Message::Text(json_text.into_owned().into()))
                        .await;
                }
            });
        }

        debug!("Driver sender task stopped!");

        Ok(())
    }
}

pub fn parse_query<T: serde::ser::Serialize>(message: &T) -> String {
    let error_message =
        "Not possible to parse mavlink message, please report this issue!".to_string();
    serde_json::to_string(&message).unwrap_or(error_message)
}

#[async_trait::async_trait]
impl Driver for Rest {
    #[instrument(level = "debug", skip(self, data_plane))]
    async fn run(&self, data_plane: DataPlane) -> Result<()> {
        let context = SendReceiveContext {
            direction: crate::drivers::Direction::Both,
            data_plane,
            on_message_output: self.on_message_output.clone(),
            on_message_input: self.on_message_input.clone(),
            filter_message_output: Default::default(),
            filter_message_input: Default::default(),
            stats: self.stats.clone(),
        };

        spawn_message_observers(
            context.data_plane.clone(),
            context.on_message_input.clone(),
            None,
        );

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        let mut first = true;
        loop {
            if first {
                first = false;
            } else {
                interval.tick().await;
            }

            let mut ws_receiver = websocket::create_message_receiver();
            let mut control_receiver = control::subscribe_mavlink_message();

            tokio::select! {
                result = Rest::send_task(&context) => {
                    if let Err(e) = result {
                        error!("Error in rest sender task: {e:?}");
                    }
                }
                result = Rest::receive_task(&context, &mut ws_receiver) => {
                    if let Err(e) = result {
                        error!("Error in rest receive task: {e:?}");
                    }
                }
                result = Rest::control_send_task(&context, &mut control_receiver) => {
                    if let Err(e) = result {
                        error!("Error in rest sender task: {e:?}");
                    }
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn info(&self) -> Box<dyn DriverInfo> {
        Box::new(RestInfo)
    }

    fn name(&self) -> Arc<String> {
        self.name.load_full()
    }

    fn uuid(&self) -> &DriverUuid {
        &self.uuid
    }
}

#[async_trait::async_trait]
impl AccumulatedDriverStatsProvider for Rest {
    async fn stats(&self) -> AccumulatedDriverStats {
        self.stats.snapshot()
    }

    async fn reset_stats(&self) {
        self.stats.reset();
    }
}

pub struct RestInfo;
impl DriverInfo for RestInfo {
    fn name(&self) -> &'static str {
        "Rest"
    }

    fn valid_schemes(&self) -> &'static [&'static str] {
        &[]
    }

    fn cli_example_legacy(&self) -> Vec<String> {
        vec![]
    }

    fn cli_example_url(&self) -> Vec<String> {
        vec![]
    }

    fn create_endpoint_from_url(&self, _url: &url::Url) -> Option<Arc<dyn Driver>> {
        None
    }
}
