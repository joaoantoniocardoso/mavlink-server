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

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::protocol::Protocol;
    use bytes::BufMut;
    use mavlink::{Message, MessageData};
    use mavlink_codec::{Packet, v2::V2Packet};

    fn heartbeat_frame(origin: Arc<str>) -> Arc<Protocol> {
        let header = mavlink::MavHeader::default();
        let data = mavlink::dialects::ardupilotmega::MavMessage::default_message_from_id(
            mavlink::dialects::ardupilotmega::HEARTBEAT_DATA::ID,
        )
        .unwrap();
        let buf = bytes::BytesMut::with_capacity(V2Packet::MAX_PACKET_SIZE);
        let mut writer = buf.writer();
        mavlink::write_v2_msg(&mut writer, header, &data).unwrap();
        let packet = Packet::V2(V2Packet::new(writer.into_inner().freeze()));
        Arc::new(Protocol::new_with_timestamp(0, origin, packet))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_consumer_does_not_block_other_sinks() {
        let data_plane = DataPlane::new(256);
        let origin: Arc<str> = Arc::from("source");

        let slow_plane = data_plane.clone();
        tokio::spawn(async move {
            let mut sink = slow_plane.register_sink();
            while let Some(_message) = sink.recv_next().await {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let mut fast_sink = data_plane.register_sink_with_origin("fast");

        let publish_count = 5;
        for _ in 0..publish_count {
            let frame = heartbeat_frame(Arc::clone(&origin));
            let sent_at = Instant::now();
            data_plane.publish(frame).unwrap();

            let received = tokio::time::timeout(Duration::from_millis(20), async {
                loop {
                    if fast_sink.recv_next().await.is_some() {
                        return sent_at.elapsed();
                    }
                }
            })
            .await
            .expect("fast sink should receive before slow consumer blocks the runtime");

            assert!(
                received < Duration::from_millis(15),
                "forwarding latency {received:?} exceeded threshold with slow consumer present"
            );
        }
    }
}
