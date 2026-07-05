use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::warn;

use crate::protocol::Protocol;

#[derive(Clone, Debug)]
pub struct DataPlane {
    inner: broadcast::Sender<Arc<Protocol>>,
}

#[derive(Debug)]
pub enum PublishError {
    NoReceivers,
}

#[derive(Debug)]
pub enum SinkRecvError {
    Closed,
    Lagged(u64),
}

pub struct SinkReceiver {
    inner: broadcast::Receiver<Arc<Protocol>>,
    loopback_origin: Option<Arc<str>>,
}

impl DataPlane {
    pub fn new(capacity: usize) -> Self {
        let (inner, _) = broadcast::channel(capacity);
        Self { inner }
    }

    pub fn from_sender(inner: broadcast::Sender<Arc<Protocol>>) -> Self {
        Self { inner }
    }

    pub fn publish(&self, frame: Arc<Protocol>) -> Result<usize, PublishError> {
        self.inner
            .send(frame)
            .map_err(|_| PublishError::NoReceivers)
    }

    pub fn register_sink(&self) -> SinkReceiver {
        SinkReceiver {
            inner: self.inner.subscribe(),
            loopback_origin: None,
        }
    }

    pub fn register_sink_with_origin(&self, origin: impl Into<Arc<str>>) -> SinkReceiver {
        SinkReceiver {
            inner: self.inner.subscribe(),
            loopback_origin: Some(origin.into()),
        }
    }

    pub fn receiver_count(&self) -> usize {
        self.inner.receiver_count()
    }
}

impl SinkReceiver {
    pub async fn recv(&mut self) -> Result<Arc<Protocol>, SinkRecvError> {
        loop {
            match self.inner.recv().await {
                Ok(message) => {
                    if let Some(ref origin) = self.loopback_origin {
                        if message.origin.as_ref().eq(origin.as_ref()) {
                            continue;
                        }
                    }
                    return Ok(message);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(SinkRecvError::Closed);
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Err(SinkRecvError::Lagged(count));
                }
            }
        }
    }

    pub async fn recv_next(&mut self) -> Option<Arc<Protocol>> {
        loop {
            match self.recv().await {
                Ok(message) => return Some(message),
                Err(SinkRecvError::Lagged(count)) => {
                    warn!("Channel lagged by {count} messages.");
                }
                Err(SinkRecvError::Closed) => return None,
            }
        }
    }
}
