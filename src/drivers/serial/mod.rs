use std::sync::Arc;

use anyhow::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    sync::{broadcast, Mutex},
};
use tokio_serial::{self, SerialPortBuilderExt, SerialStream};
use tracing::*;

use crate::{
    drivers::{Driver, DriverExt, DriverInfo},
    protocol::{read_all_messages, Protocol},
};

pub struct Serial {
    pub port_name: String,
    pub baud_rate: u32,
}

impl Serial {
    #[instrument(level = "debug")]
    pub fn new(port_name: &str, baud_rate: u32) -> Self {
        Self {
            port_name: port_name.to_string(),
            baud_rate,
        }
    }

    #[instrument(level = "debug", skip(reader, hub_sender))]
    async fn serial_receive_task(
        port_name: &str,
        reader: Arc<Mutex<ReadHalf<SerialStream>>>,
        hub_sender: broadcast::Sender<Protocol>,
    ) -> Result<()> {
        let mut buf = vec![0; 1024];

        loop {
            match reader.lock().await.read(&mut buf).await {
                // We got something
                Ok(bytes_received) if bytes_received > 0 => {
                    read_all_messages("serial", &mut buf, |message| async {
                        if let Err(error) = hub_sender.send(message) {
                            error!("Failed to send message to hub: {error:?}");
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

    #[instrument(level = "debug", skip(writer, hub_receiver))]
    async fn serial_send_task(
        port_name: &str,
        writer: Arc<Mutex<WriteHalf<SerialStream>>>,
        mut hub_receiver: broadcast::Receiver<Protocol>,
    ) -> Result<()> {
        loop {
            match hub_receiver.recv().await {
                Ok(message) => {
                    if let Err(error) = writer.lock().await.write_all(&message.raw_bytes()).await {
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
    async fn run(&self, hub_sender: broadcast::Sender<Protocol>) -> Result<()> {
        let port_name = self.port_name.clone();

        let (read, write) = match tokio_serial::new(&port_name, self.baud_rate)
            .timeout(tokio::time::Duration::from_secs(1))
            .open_native_async()
        {
            Ok(port) => {
                let (read, write) = tokio::io::split(port);
                let read = Arc::new(Mutex::new(read));
                let write = Arc::new(Mutex::new(write));
                (read, write)
            }
            Err(error) => {
                error!("Failed to open serial port {port_name:?}: {error:?}");
                return Err(error.into());
            }
        };

        debug!("Serial successfully opened port {port_name:?}");

        loop {
            let hub_sender = hub_sender.clone();
            let hub_receiver = hub_sender.subscribe();

            tokio::select! {
                result = Serial::serial_send_task(&port_name, write.clone(), hub_receiver) => {
                    if let Err(e) = result {
                        error!("Error in serial receive task for {port_name}: {e:?}");
                    }
                }
                result = Serial::serial_receive_task(&port_name, read.clone(), hub_sender.clone()) => {
                    if let Err(e) = result {
                        error!("Error in serial send task for {port_name}: {e:?}");
                    }
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: "Serial".to_string(),
        }
    }
}

pub struct SerialExt;
impl DriverExt for SerialExt {
    fn valid_schemes(&self) -> Vec<String> {
        vec!["serial".to_string()]
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

        Some(Arc::new(Serial::new(&port_name, baud_rate)))
    }
}
