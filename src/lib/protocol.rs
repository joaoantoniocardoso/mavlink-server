use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use mavlink_codec::{Packet, mavlink_json::MAVLinkMessage};
use serde::Serialize;

use crate::{
    cli,
    mavlink_json::{MAVLinkJSON, MAVLinkJSONHeader},
};

#[derive(Debug, Serialize)]
pub struct Protocol {
    pub origin: Arc<str>,
    pub timestamp: u64,
    // Dual-representation envelope: the wire `Packet` and/or its MAVLinkJSON text, each
    // materialized lazily (once) and refcounted so every sink shares a single transcode. A
    // JSON-sourced frame is never transcoded to the wire unless a binary sink actually asks for
    // it, and vice versa.
    #[serde(skip)]
    message: MAVLinkMessage,
}

impl PartialEq for Protocol {
    fn eq(&self, other: &Self) -> bool {
        // Identity is the frame + provenance; the other representation is a derived value.
        self.origin == other.origin
            && self.timestamp == other.timestamp
            && self.message.wire() == other.message.wire()
    }
}

impl Protocol {
    pub fn new(origin: impl Into<Arc<str>>, packet: Packet) -> Self {
        Self {
            origin: origin.into(),
            timestamp: chrono::Utc::now().timestamp_micros() as u64,
            message: MAVLinkMessage::from_packet(packet),
        }
    }

    pub fn new_with_timestamp(timestamp: u64, origin: impl Into<Arc<str>>, packet: Packet) -> Self {
        Self {
            origin: origin.into(),
            timestamp,
            message: MAVLinkMessage::from_packet(packet),
        }
    }

    pub fn from_mavlink_raw<M>(
        header: mavlink::MavHeader,
        message: &M,
        origin: impl Into<Arc<str>>,
    ) -> Self
    where
        M: mavlink::Message,
    {
        let packet = match cli::mavlink_version() {
            1 => {
                let mut message_raw = mavlink::MAVLinkV1MessageRaw::new();
                message_raw.serialize_message(header, message);
                Packet::from(message_raw)
            }
            2 => {
                let mut message_raw = mavlink::MAVLinkV2MessageRaw::new();
                message_raw.serialize_message(header, message);
                Packet::from(message_raw)
            }
            _ => unreachable!(),
        };

        Self {
            origin: origin.into(),
            timestamp: chrono::Utc::now().timestamp_micros() as u64,
            message: MAVLinkMessage::from_packet(packet),
        }
    }

    /// Builds a `Protocol` from MAVLinkJSON text without transcoding it to the wire: the frame
    /// stays JSON-only until a binary sink requests [`Protocol::wire`]. Returns `None` if the
    /// `"type"` tag is not in the compiled dialect or the JSON is malformed (a cheap tag scan, not
    /// a full parse), so the caller can fall back to a permissive (JSON5) parser.
    pub fn from_json_bytes(origin: impl Into<Arc<str>>, json: Bytes) -> Option<Self> {
        let message = MAVLinkMessage::from_json(json);
        message.message_id()?;
        Some(Self {
            origin: origin.into(),
            timestamp: chrono::Utc::now().timestamp_micros() as u64,
            message,
        })
    }

    /// The wire [`Packet`], transcoding it from JSON on first access if needed and caching the
    /// result. Returns `None` only for a JSON-sourced frame whose `"type"` is not in the compiled
    /// dialect (or whose text cannot be transcoded); a wire-sourced frame always yields its packet.
    pub fn wire(&self) -> Option<&Packet> {
        self.message.wire()
    }

    /// The MAVLinkJSON text for this frame, transcoded once on first access and shared (refcounted)
    /// across all JSON sinks. Returns `None` only when the message id is not in the compiled
    /// dialect. Byte-identical to `serde_json::to_string(&self.to_mavlink_json()?)`.
    pub fn json(&self) -> Option<&Bytes> {
        self.message.json()
    }

    /// The MAVLink message id, resolved cheaply from whichever representation is present.
    pub fn message_id(&self) -> Option<u32> {
        self.message.message_id()
    }

    /// The originating system id, resolved cheaply from whichever representation is present.
    pub fn system_id(&self) -> Option<u8> {
        self.message.system_id()
    }

    /// The originating component id, resolved cheaply from whichever representation is present.
    pub fn component_id(&self) -> Option<u8> {
        self.message.component_id()
    }

    /// The byte size of the already-materialized representation (wire frame or JSON text), used for
    /// throughput accounting without forcing a transcode.
    pub fn size(&self) -> usize {
        if self.message.has_wire() {
            self.message.wire().map_or(0, |packet| packet.packet_size())
        } else {
            self.message.json().map_or(0, |json| json.len())
        }
    }

    pub async fn to_mavlink<M>(&self) -> Result<(mavlink::MavHeader, M)>
    where
        M: mavlink::Message,
    {
        let Some(packet) = self.wire() else {
            return Err(anyhow::anyhow!(
                "Protocol has no wire representation to parse"
            ));
        };

        let mut reader = mavlink::async_peek_reader::AsyncPeekReader::new(packet.as_slice());

        mavlink::read_any_msg_async(&mut reader)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn to_mavlink_json<M>(&self) -> Result<MAVLinkJSON<M>>
    where
        M: mavlink::Message,
    {
        let (header, message) = self.to_mavlink().await?;

        let header = MAVLinkJSONHeader {
            inner: header,
            message_id: Some(mavlink::Message::message_id(&message)),
        };

        Ok(MAVLinkJSON { header, message })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn json_cache_matches_serde_baseline() {
        use mavlink::dialects::ardupilotmega::MavMessage;

        crate::cli::init_with(crate::cli::Args::parse_from(vec![
            std::env::args().next().unwrap_or_default(),
            "--allow-no-endpoints".to_string(),
        ]));

        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 2,
            sequence: 0,
        };
        let message = MavMessage::HEARTBEAT(mavlink::dialects::ardupilotmega::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::dialects::ardupilotmega::MavType::MAV_TYPE_ONBOARD_CONTROLLER,
            autopilot: mavlink::dialects::ardupilotmega::MavAutopilot::MAV_AUTOPILOT_INVALID,
            base_mode: mavlink::dialects::ardupilotmega::MavModeFlag::empty(),
            system_status: mavlink::dialects::ardupilotmega::MavState::MAV_STATE_STANDBY,
            mavlink_version: 0x3,
        });
        let proto = Protocol::from_mavlink_raw(header, &message, "test");

        let baseline = serde_json::to_string(
            &tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(proto.to_mavlink_json::<MavMessage>())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(proto.json().unwrap()).unwrap(),
            baseline
        );
    }
}
