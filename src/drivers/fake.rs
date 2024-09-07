use std::sync::Arc;

use anyhow::Result;
use mavlink_server::callbacks::{Callbacks, MessageCallback};
use tokio::sync::broadcast;
use tracing::*;

use crate::{
    drivers::{Driver, DriverInfo},
    protocol::{read_all_messages, Protocol},
};

pub struct FakeSink {
    on_message: Callbacks<Arc<Protocol>>,
    print: bool,
}

impl FakeSink {
    pub fn builder() -> FakeSinkBuilder {
        FakeSinkBuilder(Self {
            on_message: Callbacks::new(),
            print: false,
        })
    }
}

pub struct FakeSinkBuilder(FakeSink);

impl FakeSinkBuilder {
    pub fn build(self) -> FakeSink {
        self.0
    }

    pub fn print(mut self) -> Self {
        self.0.print = true;
        self
    }

    pub fn on_message<C>(self, callback: C) -> Self
    where
        C: MessageCallback<Arc<Protocol>>,
    {
        self.0.on_message.add_callback(callback.into_boxed());
        self
    }
}

#[async_trait::async_trait]
impl Driver for FakeSink {
    async fn run(&self, hub_sender: broadcast::Sender<Arc<Protocol>>) -> Result<()> {
        let mut hub_receiver = hub_sender.subscribe();

        while let Ok(message) = hub_receiver.recv().await {
            for future in self.on_message.call_all(Arc::clone(&message)) {
                if let Err(error) = future.await {
                    debug!("Dropping message: on_message callback returned error: {error:?}");
                    continue;
                }
            }

            if self.print {
                println!("Message received: {message:?}");
            } else {
                trace!("Message received: {message:?}");
            }
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn info(&self) -> Box<dyn DriverInfo> {
        Box::new(FakeSinkInfo)
    }
}

pub struct FakeSinkInfo;
impl DriverInfo for FakeSinkInfo {
    fn name(&self) -> &str {
        "FakeSink"
    }

    fn valid_schemes(&self) -> Vec<String> {
        vec![
            "fakeclient".to_string(),
            "fakesink".to_string(),
            "fakec".to_string(),
        ]
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

pub struct FakeSource {
    period: std::time::Duration,
    on_message: Callbacks<Arc<Protocol>>,
}

impl FakeSource {
    pub fn builder(period: std::time::Duration) -> FakeSourceBuilder {
        FakeSourceBuilder(Self {
            period,
            on_message: Callbacks::new(),
        })
    }
}

pub struct FakeSourceBuilder(FakeSource);

impl FakeSourceBuilder {
    pub fn build(self) -> FakeSource {
        self.0
    }

    pub fn on_message<C>(self, callback: C) -> Self
    where
        C: MessageCallback<Arc<Protocol>>,
    {
        self.0.on_message.add_callback(callback.into_boxed());
        self
    }
}

#[async_trait::async_trait]
impl Driver for FakeSource {
    async fn run(&self, hub_sender: broadcast::Sender<Arc<Protocol>>) -> Result<()> {
        let mut sequence = 0;

        let mut buf: Vec<u8> = Vec::with_capacity(280);

        use mavlink::ardupilotmega::{
            MavAutopilot, MavMessage, MavModeFlag, MavState, MavType, HEARTBEAT_DATA,
        };

        loop {
            let header = mavlink::MavHeader {
                sequence,
                system_id: 1,
                component_id: 2,
            };
            let data = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
                custom_mode: 5,
                mavtype: MavType::MAV_TYPE_QUADROTOR,
                autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
                base_mode: MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED
                    | MavModeFlag::MAV_MODE_FLAG_STABILIZE_ENABLED
                    | MavModeFlag::MAV_MODE_FLAG_GUIDED_ENABLED
                    | MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
                system_status: MavState::MAV_STATE_STANDBY,
                mavlink_version: 3,
            });
            sequence = sequence.overflowing_add(1).0;

            buf.clear();
            mavlink::write_v2_msg(&mut buf, header, &data).expect("Failed to write message");

            let hub_sender_cloned = hub_sender.clone();
            read_all_messages("FakeSource", &mut buf, move |message| {
                let message = Arc::new(message);
                let hub_sender = hub_sender_cloned.clone();

                async move {
                    trace!("Fake message created: {message:?}");

                    for future in self.on_message.call_all(Arc::clone(&message)) {
                        if let Err(error) = future.await {
                            debug!(
                                "Dropping message: on_message callback returned error: {error:?}"
                            );
                            continue;
                        }
                    }

                    if let Err(error) = hub_sender.send(message) {
                        error!("Failed to send message to hub: {error:?}");
                    }
                }
            })
            .await;

            tokio::time::sleep(self.period).await;
        }
    }

    fn info(&self) -> Box<dyn DriverInfo> {
        Box::new(FakeSourceInfo)
    }
}

pub struct FakeSourceInfo;
impl DriverInfo for FakeSourceInfo {
    fn name(&self) -> &str {
        "FakeSource"
    }

    fn valid_schemes(&self) -> Vec<String> {
        vec![
            "fakesource".to_string(),
            "fakeserver".to_string(),
            "fakesrc".to_string(),
            "fakes".to_string(),
        ]
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

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use anyhow::Result;
    use tokio::sync::{broadcast, RwLock};

    use super::*;

    #[tokio::test]
    async fn loopback_test() -> Result<()> {
        let (hub_sender, _) = broadcast::channel(10000);

        let number_of_messages = 800;
        let message_period = tokio::time::Duration::from_micros(1);
        let timeout_time = tokio::time::Duration::from_secs(1);

        let source_messages = Arc::new(RwLock::new(Vec::<Arc<Protocol>>::with_capacity(1000)));
        let sink_messages = Arc::new(RwLock::new(Vec::<Arc<Protocol>>::with_capacity(1000)));

        // FakeSink and task
        let sink_messages_clone = sink_messages.clone();
        let sink = FakeSink::builder()
            .on_message(move |message: Arc<Protocol>| {
                let sink_messages = sink_messages_clone.clone();

                async move {
                    sink_messages.write().await.push(message);
                    Ok(())
                }
            })
            .build();
        let sink_task = tokio::spawn({
            let hub_sender = hub_sender.clone();

            async move { sink.run(hub_sender).await }
        });

        // FakeSource and task
        let source_messages_clone = source_messages.clone();
        let source = FakeSource::builder(message_period)
            .on_message(move |message: Arc<Protocol>| {
                let source_messages = source_messages_clone.clone();

                async move {
                    source_messages.write().await.push(message);
                    Ok(())
                }
            })
            .build();
        let source_task = tokio::spawn({
            let hub_sender = hub_sender.clone();

            async move { source.run(hub_sender).await }
        });

        // Monitoring task to wait the
        let sink_messages_clone = sink_messages.clone();
        let sink_monitor_task = tokio::spawn(async move {
            loop {
                if sink_messages_clone.read().await.len() >= number_of_messages {
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });
        let _ = tokio::time::timeout(timeout_time, sink_monitor_task)
            .await
            .expect(format!("sink messages: {:?}", sink_messages.read().await.len()).as_str());

        source_task.abort();
        sink_task.abort();

        // Compare the messages
        let source_messages = source_messages.read().await.clone();
        let sink_messages = sink_messages.read().await.clone();

        assert!(source_messages.len() >= number_of_messages);
        assert!(sink_messages.len() >= number_of_messages);
        assert_eq!(source_messages, sink_messages);

        Ok(())
    }
}
