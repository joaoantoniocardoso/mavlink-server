use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::{Sink, SinkExt, Stream, StreamExt};
use mavlink_codec::{Packet, error::DecoderError};
use tracing::*;

use crate::{
    callbacks::{Callbacks, SyncMessageFilters},
    hub::dataplane::SinkReceiver,
    hub::{DataPlane, register_sink},
    protocol::Protocol,
    stats::accumulated::driver::AtomicDriverStats,
};

#[derive(Clone)]
pub struct SendReceiveContext {
    pub direction: crate::drivers::Direction,
    pub data_plane: DataPlane,
    pub on_message_output: Callbacks<Arc<Protocol>>,
    pub on_message_input: Callbacks<Arc<Protocol>>,
    pub filter_message_output: SyncMessageFilters<Arc<Protocol>>,
    pub filter_message_input: SyncMessageFilters<Arc<Protocol>>,
    pub stats: Arc<AtomicDriverStats>,
}

pub fn spawn_message_observers(
    data_plane: DataPlane,
    observers: Callbacks<Arc<Protocol>>,
    loopback_origin: Option<Arc<str>>,
) {
    if observers.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let mut sink = match loopback_origin {
            Some(origin) => data_plane.register_sink_with_origin(origin),
            None => data_plane.register_sink(),
        };

        loop {
            let Some(message) = sink.recv_next().await else {
                break;
            };

            for future in observers.call_all(message) {
                let _ = future.await;
            }
        }
    });
}

#[instrument(level = "debug", skip(writer, reader, context))]
pub async fn default_send_receive_run<S, T>(
    mut writer: S,
    mut reader: T,
    identifier: &str,
    context: &SendReceiveContext,
) -> Result<()>
where
    S: Sink<Packet, Error = std::io::Error> + std::marker::Unpin,
    T: Stream<Item = std::io::Result<std::result::Result<Packet, DecoderError>>>
        + std::marker::Unpin,
{
    if context.direction.receive_only() {
        return default_receive_task(&mut reader, identifier, context).await;
    }

    if context.direction.send_only() {
        return default_send_task(&mut writer, identifier, context).await;
    }

    tokio::select! {
        result = default_send_task(&mut writer, identifier, context) => {
            if let Err(error) = result {
                error!("Error in send task for {identifier}: {error:?}");
            }
        }
        result = default_receive_task(&mut reader, identifier, context) => {
            if let Err(error) = result {
                error!("Error in receive task for {identifier}: {error:?}");
            }
        }
    }

    Ok(())
}

/// Receives messages from a Stream and sends them to the HUB Channel
#[instrument(level = "debug", skip(reader, context))]
pub async fn default_receive_task<T>(
    reader: &mut T,
    identifier: &str,
    context: &SendReceiveContext,
) -> Result<()>
where
    T: Stream<Item = std::io::Result<std::result::Result<Packet, DecoderError>>>
        + std::marker::Unpin,
{
    let origin: Arc<str> = Arc::from(identifier);

    spawn_message_observers(
        context.data_plane.clone(),
        context.on_message_input.clone(),
        None,
    );

    'mainloop: loop {
        let packet = match reader.next().await {
            Some(Ok(Ok(packet))) => packet,
            Some(Ok(Err(decode_error))) => {
                error!("Failed to decode packet: {decode_error:?}");
                continue;
            }
            Some(Err(ref io_error)) => match io_error.kind() {
                std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof => {
                    warn!("Remote {identifier:?} disconnected: {io_error:?}");
                    break;
                }
                std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::Interrupted => {
                    warn!("Temporary IO error from {identifier:?}: {io_error:?}");
                    continue;
                }
                _ => {
                    return Err(anyhow!(
                        "Critical error trying to decode data from: {io_error:?}"
                    ));
                }
            },
            None => break,
        };

        let message = Arc::new(Protocol::new(Arc::clone(&origin), packet));

        trace!("Received message: {message:?}");

        context.stats.update_input(&message);

        if let Err(error) = context.filter_message_input.apply_all(message.clone()) {
            debug!("Dropping message: filter_message_input returned error: {error:?}");
            continue 'mainloop;
        }

        crate::hub::accumulate_hub_message(&message);

        if let Err(send_error) = context.data_plane.publish(message) {
            error!("Failed to send message to hub: {send_error:?}");
            continue;
        }

        trace!("Message sent to hub");
    }

    debug!("Driver receiver task stopped!");

    Ok(())
}

/// Receives messages from the HUB Channel and sends them to a Sink
#[instrument(level = "debug", skip(writer, context))]
pub async fn default_send_task<S>(
    writer: &mut S,
    identifier: &str,
    context: &SendReceiveContext,
) -> Result<()>
where
    S: Sink<Packet, Error = std::io::Error> + std::marker::Unpin,
{
    let origin = Arc::from(identifier);

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

        context.stats.update_output(&message);

        if let Err(error) = context.filter_message_output.apply_all(message.clone()) {
            debug!("Dropping message: filter_message_output returned error: {error:?}");
            continue 'mainloop;
        }

        let Some(packet) = message.wire() else {
            trace!("Skipping message with no wire representation for {identifier}");
            continue;
        };

        if let Err(error) = writer.send(packet.clone()).await {
            error!("Failed to send message: {error:?}");
            break;
        }

        trace!("Message sent to {identifier}: {:?}", packet.as_slice());
    }

    debug!("Driver sender task stopped!");

    Ok(())
}

pub async fn recv_from_sink(sink: &mut SinkReceiver) -> Option<Arc<Protocol>> {
    sink.recv_next().await
}
