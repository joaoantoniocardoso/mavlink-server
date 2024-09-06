use crate::protocol::{read_all_messages, Protocol};
use anyhow::Result;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_serial;
use tracing::*;

use crate::drivers::{Driver, DriverExt, DriverInfo};

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

    #[instrument(level = "debug", skip(port))]
    async fn serial_receive_task(
        mut port: Box<dyn tokio_serial::SerialPort>,
        hub_sender: Arc<broadcast::Sender<Protocol>>,
    ) -> Result<()> {
        let mut buf = vec![0; 1024];
        let port_name = port.name().unwrap_or("unknown".to_string());

        loop {
            tokio::time::sleep(tokio::time::Duration::from_micros(1)).await;
            match port.read(&mut buf) {
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

    #[instrument(level = "debug", skip(port))]
    async fn serial_send_task(
        mut port: Box<dyn tokio_serial::SerialPort>,
        mut hub_receiver: broadcast::Receiver<Protocol>,
    ) -> Result<()> {
        loop {
            match hub_receiver.recv().await {
                Ok(message) => {
                    if let Err(error) = port.write_all(&message.raw_bytes()) {
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

        let port = match tokio_serial::new(&port_name, self.baud_rate)
            .timeout(Duration::from_millis(1000))
            .open()
        {
            Ok(port) => port,
            Err(error) => {
                error!("Failed to open serial port {port_name:?}: {error:?}");
                return Err(error.into());
            }
        };

        debug!("Serial successfully opened port {port_name:?}");

        loop {
            let hub_sender = Arc::new(hub_sender.clone());
            let hub_receiver = hub_sender.subscribe();

            tokio::select! {
                result = Serial::serial_send_task(port.try_clone()?, hub_receiver) => {
                    if let Err(e) = result {
                        error!("Error in serial receive task for {port_name}: {e:?}");
                    }
                }
                result = Serial::serial_receive_task(port.try_clone()?, hub_sender) => {
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
