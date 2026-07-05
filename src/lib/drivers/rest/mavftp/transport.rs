use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};

use anyhow::{Result, anyhow};
use tokio::sync::mpsc;
use tracing::*;

use crate::{hub, protocol::Protocol};

use super::protocol::{FtpOpcode, FtpPayload};

const FTP_MESSAGE_ID: u32 = 110;
static SEQ_NUMBER: AtomicU16 = AtomicU16::new(0);

#[instrument(level = "debug", fields(opcode = ?opcode))]
pub(super) fn new_request(opcode: FtpOpcode) -> FtpPayload {
    FtpPayload::new_request(opcode, next_seq())
}

#[instrument(level = "debug")]
pub(super) async fn subscribe() -> Result<mpsc::Receiver<Arc<Protocol>>> {
    hub::register_control_sink(None).await
}

#[instrument(
    level = "debug",
    skip(payload),
    fields(opcode = ?payload.opcode, seq_number = payload.seq_number)
)]
pub(super) fn send_ftp_message(target_system: u8, target_component: u8, payload: &FtpPayload) {
    crate::drivers::rest::control::send_mavlink_message(build_ftp_message(
        target_system,
        target_component,
        payload,
    ));
}

#[instrument(level = "debug", skip(receiver))]
pub(super) async fn recv_ftp(
    receiver: &mut mpsc::Receiver<Arc<Protocol>>,
    target_system: u8,
    target_component: u8,
    expected_seq: Option<u16>,
) -> Result<FtpPayload> {
    loop {
        let Some(protocol) = receiver.recv().await else {
            return Err(anyhow!("Hub control sink closed"));
        };

        if protocol.message_id() != Some(FTP_MESSAGE_ID)
            || protocol.system_id() != Some(target_system)
            || protocol.component_id() != Some(target_component)
        {
            continue;
        }

        let Ok((_header, msg)) = protocol
            .to_mavlink::<mavlink::dialects::ardupilotmega::MavMessage>()
            .await
        else {
            continue;
        };

        if let mavlink::dialects::ardupilotmega::MavMessage::FILE_TRANSFER_PROTOCOL(ftp) = msg {
            let resp = FtpPayload::decode(&ftp.payload)?;
            if let Some(seq) = expected_seq
                && (resp.seq_number != seq + 1
                    || (resp.opcode != FtpOpcode::Ack && resp.opcode != FtpOpcode::Nak))
            {
                continue;
            }
            return Ok(resp);
        }
    }
}

#[instrument(
    level = "debug",
    skip(payload),
    fields(opcode = ?payload.opcode, seq_number = payload.seq_number)
)]
fn build_ftp_message(
    target_system: u8,
    target_component: u8,
    payload: &FtpPayload,
) -> mavlink::dialects::ardupilotmega::MavMessage {
    mavlink::dialects::ardupilotmega::MavMessage::FILE_TRANSFER_PROTOCOL(
        mavlink::dialects::ardupilotmega::FILE_TRANSFER_PROTOCOL_DATA {
            target_network: 0,
            target_system,
            target_component,
            payload: payload.encode(),
        },
    )
}

#[instrument(level = "debug")]
fn next_seq() -> u16 {
    SEQ_NUMBER.fetch_add(1, Ordering::Relaxed)
}
