use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use mavlink;
use tokio::sync::broadcast;
use tracing::*;
use zenoh;

use crate::{
    callbacks::{Callbacks, MessageCallback},
    drivers::{Driver, DriverInfo, generic_tasks::SendReceiveContext, zenoh::session},
    mavlink_json::MAVLinkJSON,
    protocol::Protocol,
    stats::{
        accumulated::driver::{
            AccumulatedDriverStats, AccumulatedDriverStatsProvider, AtomicDriverStats,
        },
        driver::DriverUuid,
    },
};

const TOPIC_PREFIX: &str = "mavlink";
const DRIVER_IDENTIFIER: &str = "zenoh";

#[derive(Debug)]
pub struct Zenoh {
    name: arc_swap::ArcSwap<String>,
    uuid: DriverUuid,
    on_message_input: Callbacks<Arc<Protocol>>,
    on_message_output: Callbacks<Arc<Protocol>>,
    stats: Arc<AtomicDriverStats>,
}

pub struct ZenohBuilder(Zenoh);

impl ZenohBuilder {
    pub fn build(self) -> Zenoh {
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

impl Zenoh {
    #[instrument(level = "debug")]
    pub fn builder(name: &str) -> ZenohBuilder {
        let name = Arc::new(name.to_string());

        ZenohBuilder(Self {
            name: arc_swap::ArcSwap::new(name.clone()),
            uuid: Self::generate_uuid(&name),
            on_message_input: Callbacks::default(),
            on_message_output: Callbacks::default(),
            stats: Arc::new(AtomicDriverStats::new(name, &ZenohInfo)),
        })
    }

    #[instrument(level = "debug", skip_all)]
    async fn receive_task(
        context: &SendReceiveContext,
        session: Arc<zenoh::Session>,
    ) -> Result<()> {
        let subscriber = match session
            .declare_subscriber(format!("{TOPIC_PREFIX}/in"))
            .await
        {
            Ok(subscriber) => subscriber,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Failed to create subscriber for mavlink data: {error:?}"
                ));
            }
        };

        let origin: Arc<str> = Arc::from(DRIVER_IDENTIFIER);

        'mainloop: loop {
            let sample = match subscriber.recv_async().await {
                Ok(sample) => sample,
                Err(error) => {
                    error!("Failed to receive sample: no senders left: {error:?}");
                    break;
                }
            };

            let payload = sample.payload().to_bytes();

            // Fast path: strict-JSON reverse transcoder, seeding the JSON cache.
            let bus_message = if let Some(protocol) = crate::protocol::Protocol::from_json_bytes(
                Arc::clone(&origin),
                bytes::Bytes::copy_from_slice(&payload),
            ) {
                Arc::new(protocol)
            } else {
                // Fallback: permissive JSON5 via the typed serde path.
                let content = match json5::from_str::<
                    MAVLinkJSON<mavlink::dialects::ardupilotmega::MavMessage>,
                >(std::str::from_utf8(&payload).unwrap())
                {
                    Ok(content) => content,
                    Err(error) => {
                        debug!(
                            "Failed to parse message, not a valid MAVLinkMessage: {sample:?}. Error: {error:?}"
                        );
                        continue;
                    }
                };
                Arc::new(Protocol::from_mavlink_raw(
                    content.header.inner,
                    &content.message,
                    Arc::clone(&origin),
                ))
            };

            trace!("Received message: {bus_message:?}");

            context.stats.update_input(&bus_message);

            for future in context.on_message_input.call_all(bus_message.clone()) {
                if let Err(error) = future.await {
                    debug!("Dropping message: on_message_input callback returned error: {error:?}");
                    continue 'mainloop;
                }
            }

            crate::hub::accumulate_hub_message(&bus_message);

            if let Err(error) = context.hub_sender.send(bus_message) {
                error!("Failed to send message to hub: {error:?}");
                continue;
            }

            trace!("Message sent to hub");
        }

        debug!("Driver receiver task stopped!");

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    async fn send_task(context: &SendReceiveContext, session: Arc<zenoh::Session>) -> Result<()> {
        let mut hub_receiver = context.hub_sender.subscribe();
        let mut publishers = HashMap::new();

        'mainloop: loop {
            let message = match hub_receiver.recv().await {
                Ok(message) => message,
                Err(broadcast::error::RecvError::Closed) => {
                    error!("Hub channel closed!");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    warn!("Channel lagged by {count} messages.");
                    continue;
                }
            };

            if message.origin.as_ref().eq(DRIVER_IDENTIFIER) {
                continue; // Don't do loopback
            }

            context.stats.update_output(&message);

            for future in context.on_message_output.call_all(message.clone()) {
                if let Err(error) = future.await {
                    debug!(
                        "Dropping message: on_message_output callback returned error: {error:?}"
                    );
                    continue 'mainloop;
                }
            }

            use mavlink_codec::mavlink_json::{generated, rt};

            // Per-field fan-out needs the descriptor + wire frame; a frame we cannot transcode to
            // the wire (unknown JSON type) has nothing to publish here, so skip it.
            let Some(packet) = message.wire() else {
                debug!("Skipping message with no wire representation");
                continue;
            };

            // Resolve the descriptor for this frame; unknown ids are not in the
            // compiled dialect, so skip (same as the old typed path failing).
            let Some(desc) = generated::descriptor(packet.message_id()) else {
                debug!(
                    "Skipping message id {}: not in dialect",
                    packet.message_id()
                );
                continue;
            };
            let message_name = desc.name;

            // One pass: whole-message JSON + byte ranges of every field's value.
            let mut blob: Vec<u8> = Vec::with_capacity(256);
            let mut ranges = vec![(0u32, 0u32); desc.fields.len()];
            rt::to_json_indexed(packet, desc, &mut blob, &mut ranges);
            let blob = bytes::Bytes::from(blob);
            let json_string = std::str::from_utf8(&blob).unwrap();

            let out_topic_name = format!("{TOPIC_PREFIX}/out");
            Self::publish_json(&session, &mut publishers, &out_topic_name, json_string).await;

            let message_topic_name = format!(
                "mavlink/{}/{}/{}",
                packet.system_id(),
                packet.component_id(),
                message_name
            );
            Self::publish_json(&session, &mut publishers, &message_topic_name, json_string).await;

            // Per-field: slice each value straight out of the single blob (no re-serialize).
            for (field, &(start, end)) in desc.fields.iter().zip(ranges.iter()) {
                let field_topic_name = format!("{message_topic_name}/{}", field.name);
                let field_json = std::str::from_utf8(&blob[start as usize..end as usize]).unwrap();
                Self::publish_json(&session, &mut publishers, &field_topic_name, field_json).await;
            }
        }

        debug!("Driver sender task stopped!");

        Ok(())
    }

    async fn publish_json(
        session: &zenoh::Session,
        publishers: &mut HashMap<String, zenoh::pubsub::Publisher<'static>>,
        topic_name: &str,
        payload: &str,
    ) {
        if !publishers.contains_key(topic_name) {
            let topic = topic_name.to_string();
            let key_expr = match zenoh::key_expr::KeyExpr::try_from(topic.clone()) {
                Ok(key_expr) => key_expr,
                Err(error) => {
                    error!("Failed to create key expression for {topic_name}: {error:?}");
                    return;
                }
            };

            match session
                .declare_publisher(key_expr)
                .encoding(zenoh::bytes::Encoding::APPLICATION_JSON.with_schema("mavlink"))
                .congestion_control(zenoh::qos::CongestionControl::Block)
                .priority(zenoh::qos::Priority::RealTime)
                .express(false)
                .await
            {
                Ok(publisher) => {
                    publishers.insert(topic, publisher);
                }
                Err(error) => {
                    error!("Failed to create publisher for {topic_name}: {error:?}");
                    return;
                }
            }
        }

        let Some(publisher) = publishers.get(topic_name) else {
            return;
        };

        if let Err(error) = publisher
            .put(payload)
            .encoding(zenoh::bytes::Encoding::APPLICATION_JSON)
            .await
        {
            error!("Failed to send message to {topic_name}: {error:?}");
        } else {
            trace!("Message sent to {topic_name}: {payload:?}");
        }
    }
}

#[async_trait::async_trait]
impl Driver for Zenoh {
    #[instrument(level = "debug", skip(self, hub_sender))]
    async fn run(&self, hub_sender: broadcast::Sender<Arc<Protocol>>) -> Result<()> {
        let context = SendReceiveContext {
            direction: crate::drivers::Direction::Both,
            hub_sender,
            on_message_output: self.on_message_output.clone(),
            on_message_input: self.on_message_input.clone(),
            stats: self.stats.clone(),
        };

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        let mut first = true;
        loop {
            if first {
                first = false;
            } else {
                interval.tick().await;
            }

            debug!("Trying to connect...");

            let session = session::session().await;

            debug!("Successfully connected");

            tokio::select! {
                result = Zenoh::send_task(&context, session.clone()) => {
                    if let Err(error) = result {
                        error!("Error in send task: {error:?}");
                    }
                }
                result = Zenoh::receive_task(&context, session) => {
                    if let Err(error) = result {
                        error!("Error in receive task: {error:?}");
                    }
                }
            }

            debug!("Restarting connection loop...");
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn info(&self) -> Box<dyn DriverInfo> {
        Box::new(ZenohInfo)
    }

    fn name(&self) -> Arc<String> {
        self.name.load_full()
    }

    fn uuid(&self) -> &DriverUuid {
        &self.uuid
    }
}

#[async_trait::async_trait]
impl AccumulatedDriverStatsProvider for Zenoh {
    async fn stats(&self) -> AccumulatedDriverStats {
        self.stats.snapshot()
    }

    async fn reset_stats(&self) {
        self.stats.reset();
    }
}

pub struct ZenohInfo;
impl DriverInfo for ZenohInfo {
    fn name(&self) -> &'static str {
        "Zenoh"
    }
    fn valid_schemes(&self) -> &'static [&'static str] {
        &["zenoh"]
    }

    fn cli_example_legacy(&self) -> Vec<String> {
        let first_schema = self.valid_schemes()[0];

        vec![format!("{first_schema}:<IP>:<PORT>")]
    }

    fn cli_example_url(&self) -> Vec<String> {
        let first_schema = &self.valid_schemes()[0];
        vec![format!("{first_schema}://<IP>:<PORT>").to_string()]
    }

    fn create_endpoint_from_url(&self, url: &url::Url) -> Option<Arc<dyn Driver>> {
        println!("{}", &url);
        let _host = url.host_str().unwrap();
        let _port = url.port().unwrap();
        Some(Arc::new(Zenoh::builder("Zenoh").build()))
    }
}
