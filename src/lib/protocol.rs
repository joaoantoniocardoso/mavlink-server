use std::{
    future::Future,
    io::Cursor,
    ops::{Deref, DerefMut},
};

use mavlink::{ardupilotmega::MavMessage, MAVLinkV2MessageRaw};
use serde::Serialize;

use tracing::*;

#[derive(Debug, PartialEq, Serialize)]
pub struct Protocol {
    pub origin: String,
    pub timestamp: u64,
    #[serde(skip)]
    message: MAVLinkV2MessageRaw,
}

impl Protocol {
    pub fn new(origin: &str, message: MAVLinkV2MessageRaw) -> Self {
        Self {
            origin: origin.to_string(),
            timestamp: chrono::Utc::now().timestamp_micros() as u64,
            message,
        }
    }

    pub fn new_with_timestamp(timestamp: u64, origin: &str, message: MAVLinkV2MessageRaw) -> Self {
        Self {
            origin: origin.to_string(),
            timestamp,
            message,
        }
    }
}

pub async fn read_all_messages<F, Fut>(
    origin: &str,
    buf: &mut Vec<u8>,
    discard_invalid_checksum: bool,
    process_message: F,
) where
    F: Fn(Protocol) -> Fut,
    Fut: Future<Output = ()>,
{
    let reader = Cursor::new(buf.as_slice());
    let mut reader: mavlink::async_peek_reader::AsyncPeekReader<Cursor<&[u8]>, 280> =
        mavlink::async_peek_reader::AsyncPeekReader::new(reader);

    let message = loop {
        match mavlink::read_v2_raw_message_async::<MavMessage, _>(&mut reader).await {
            Ok(message) => {
                break Some(message);
            }
            Err(error) => {
                match error {
                    mavlink::error::MessageReadError::Io(_) => (),
                    mavlink::error::MessageReadError::Parse(ref parse_error) => {
                        error!("Failed to parse MAVLink message: {error:?}");

                        if let mavlink::error::ParserError::InvalidCRC {
                            crc: _,
                            calculated_crc: _,
                            message,
                        } = parse_error
                        {
                            if !discard_invalid_checksum {
                                let mavlink::MAVLinkMessageRaw::V2(message) =
                                    message.as_ref().to_owned()
                                else {
                                    continue;
                                };
                                break Some(message);
                            }
                        }
                    }
                }

                break None;
            }
        }
    };

    if let Some(message) = message {
        let message = Protocol::new(origin, message);

        trace!("Parsed message: {:?}", message.raw_bytes());
        process_message(message).await;
    }

    let bytes_read = reader.reader_ref().position() as usize;
    buf.drain(..bytes_read);
}

impl Deref for Protocol {
    type Target = MAVLinkV2MessageRaw;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl DerefMut for Protocol {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.message
    }
}
