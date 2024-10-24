use std::sync::Arc;

use anyhow::Result;
use mavlink_server::callbacks::{Callbacks, MessageCallback};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{broadcast, Mutex, RwLock},
};
use tokio_serial::{self, SerialPortBuilderExt};
use tracing::*;

use crate::{
    drivers::{Driver, DriverInfo},
    protocol::{read_all_messages, Protocol},
    stats::driver::{DriverStats, DriverStatsInfo},
};

pub struct Serial {
    pub port_name: String,
    pub baud_rate: u32,
    on_message_input: Callbacks<Arc<Protocol>>,
    on_message_output: Callbacks<Arc<Protocol>>,
    stats: Arc<RwLock<DriverStatsInfo>>,
}

pub struct SerialBuilder(Serial);

impl SerialBuilder {
    pub fn build(self) -> Serial {
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

impl Serial {
    #[instrument(level = "debug")]
    pub fn builder(port_name: &str, baud_rate: u32) -> SerialBuilder {
        SerialBuilder(Self {
            port_name: port_name.to_string(),
            baud_rate,
            on_message_input: Callbacks::new(),
            on_message_output: Callbacks::new(),
            stats: Arc::new(RwLock::new(DriverStatsInfo::default())),
        })
    }

    #[instrument(level = "debug", skip(port, on_message_input))]
    async fn serial_receive_task(
        port_name: &str,
        port: Arc<Mutex<tokio::io::ReadHalf<tokio_serial::SerialStream>>>,
        hub_sender: broadcast::Sender<Arc<Protocol>>,

        on_message_input: &Callbacks<Arc<Protocol>>,
    ) -> Result<()> {
        let mut buf = vec![0; 1024];

        loop {
            match port.lock().await.read(&mut buf).await {
                // We got something
                Ok(bytes_received) if bytes_received > 0 => {
                    read_all_messages("serial", &mut buf, |message| async {
                        let message = Arc::new(message);

                        for future in on_message_input.call_all(Arc::clone(&message)) {
                            if let Err(error) = future.await {
                                debug!("Dropping message: on_message_input callback returned error: {error:?}");
                                continue;
                            }
                        }

                        if let Err(error) = hub_sender.send(message) {
                            error!("Failed to send message to hub: {error:?}, from {port_name:?}");
                        }
                    })
                    .await;
                }
                // We got nothing
                Ok(_) => {
                    break;
                }
                // We got problems
                Err(error) => {
                    error!("Failed to receive serial message: {error:?}, from {port_name:?}");
                    break;
                }
            }
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(port, on_message_output))]
    async fn serial_send_task(
        port_name: &str,
        port: Arc<Mutex<tokio::io::WriteHalf<tokio_serial::SerialStream>>>,
        mut hub_receiver: broadcast::Receiver<Arc<Protocol>>,
        on_message_output: &Callbacks<Arc<Protocol>>,
    ) -> Result<()> {
        loop {
            match hub_receiver.recv().await {
                Ok(message) => {
                    for future in on_message_output.call_all(Arc::clone(&message)) {
                        if let Err(error) = future.await {
                            debug!("Dropping message: on_message_output callback returned error: {error:?}");
                            continue;
                        }
                    }

                    if let Err(error) = port.lock().await.write_all(&message.raw_bytes()).await {
                        error!("Failed to send serial message: {error:?}");
                        break;
                    }
                }
                Err(error) => {
                    error!("Failed to receive message from hub: {error:?}");
                }
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Driver for Serial {
    #[instrument(level = "debug", skip(self, hub_sender))]
    async fn run(&self, hub_sender: broadcast::Sender<Arc<Protocol>>) -> Result<()> {
        let port_name = self.port_name.clone();
        let (read, write) = match tokio_serial::new(&port_name, self.baud_rate)
            .timeout(tokio::time::Duration::from_secs(1))
            .open_native_async()
        {
            Ok(port) => {
                let (read, write) = tokio::io::split(port);
                (Arc::new(Mutex::new(read)), Arc::new(Mutex::new(write)))
            }
            Err(error) => {
                error!("Failed to open serial port {port_name:?}: {error:?}");
                return Err(error.into());
            }
        };
        loop {
            let hub_sender = hub_sender.clone();
            let hub_receiver = hub_sender.subscribe();

            tokio::select! {
                result = Serial::serial_send_task(&port_name, write.clone(), hub_receiver, &self.on_message_output) => {
                    if let Err(e) = result {
                        error!("Error in serial receive task for {port_name}: {e:?}");
                    }
                }
                result = Serial::serial_receive_task(&port_name, read.clone(), hub_sender, &self.on_message_input) => {
                    if let Err(e) = result {
                        error!("Error in serial send task for {port_name}: {e:?}");
                    }
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn info(&self) -> Box<dyn DriverInfo> {
        Box::new(SerialInfo)
    }
}

#[async_trait::async_trait]
impl DriverStats for Serial {
    async fn stats(&self) -> DriverStatsInfo {
        self.stats.read().await.clone()
    }

    async fn reset_stats(&self) {
        *self.stats.write().await = DriverStatsInfo {
            input: None,
            output: None,
        }
    }
}

pub struct SerialInfo;
impl DriverInfo for SerialInfo {
    fn name(&self) -> &str {
        "Serial"
    }

    fn valid_schemes(&self) -> Vec<String> {
        vec!["serial".to_string()]
    }

    fn cli_example_legacy(&self) -> Vec<String> {
        let first_schema = &self.valid_schemes()[0];
        vec![
            format!("{first_schema}:<PORT>:<BAUDRATE>"),
            format!("{first_schema}:/dev/ttyACM0:115200"),
            format!("{first_schema}:COM:57600"),
        ]
    }

    fn cli_example_url(&self) -> Vec<String> {
        let first_schema = &self.valid_schemes()[0];
        vec![
            format!("{first_schema}://<PORT>?baudrate=<BAUDRATE?>").to_string(),
            url::Url::parse(&format!("{first_schema}:///dev/ttyACM0?baudrate=115200"))
                .unwrap()
                .to_string(),
            url::Url::parse(&format!("{first_schema}://COM1?baudrate=57600"))
                .unwrap()
                .to_string(),
        ]
    }

    fn create_endpoint_from_url(&self, url: &url::Url) -> Option<Arc<dyn Driver>> {
        let port_name = url.path().to_string();
        let baud_rate = url
            .query_pairs()
            .find_map(|(key, value)| {
                if key == "baudrate" || key == "arg2" {
                    value.parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(115200); // Commun baudrate between flight controllers

        Some(Arc::new(Serial::builder(&port_name, baud_rate).build()))
    }
}
