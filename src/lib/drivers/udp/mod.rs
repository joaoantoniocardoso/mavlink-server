use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use futures::{Sink, SinkExt};
use mavlink_codec::Packet;
use tracing::*;

use super::generic_tasks::{SendReceiveContext, spawn_message_observers};
use crate::hub::register_sink;

pub mod client;
pub mod server;

/// Receives messages from the HUB Channel and sends them to a Sink
#[instrument(level = "debug", skip(writer, context))]
pub(crate) async fn udp_send_task<S>(
    writer: &mut S,
    remote_addr: &SocketAddr,
    context: &SendReceiveContext,
) -> Result<()>
where
    S: Sink<(Packet, SocketAddr), Error = std::io::Error> + std::marker::Unpin,
{
    let origin = Arc::from(remote_addr.to_string());

    spawn_message_observers(
        context.data_plane.clone(),
        context.on_message_output.clone(),
        Some(Arc::clone(&origin)),
    );

    let mut sink = register_sink(Some(origin)).await?;

    'mainloop: loop {
        let Some(message) = sink.recv_next().await else {
            error!("Hub channel closed!");
            break;
        };

        if context.direction.can_send() {
            context.stats.update_output(&message);

            if let Err(error) = context.filter_message_output.apply_all(message.clone()) {
                debug!(
                    client = ?remote_addr, "Dropping message: filter_message_output returned error: {error:?}"
                );
                continue 'mainloop;
            }

            let Some(packet) = message.wire() else {
                trace!(client = ?remote_addr, "Skipping message with no wire representation");
                continue;
            };

            if let Err(io_error) = writer.send((packet.clone(), *remote_addr)).await {
                match io_error.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        trace!(client = ?remote_addr, "Failed send message: {io_error}");
                        continue;
                    }
                    _ => {
                        error!(client = ?remote_addr, "Failed to send message: {io_error:?}");
                    }
                }
                break;
            }

            trace!("Message sent to {remote_addr}: {:?}", packet.as_slice());
        }
    }
    Ok(())
}
