use alloc::sync::Arc;
use core::time::Duration;

use rsnano_core::{Networks, NodeId, ProtocolInfo};
use rsnano_messages::MessageDeserializer;
use rsnano_network::Channel;
use rsnano_network_protocol::InboundMessageQueue;

pub(crate) async fn run_loopback_channel_adapter(
    loopback: Arc<Channel>,
    node_id: NodeId,
    network: Networks,
    inbound: Arc<InboundMessageQueue>,
) {
    loopback.set_node_id(node_id);
    let protocol = ProtocolInfo::default_for(network);
    let mut deserializer = MessageDeserializer::new(protocol);
    loop {
        if loopback.is_cancelled() {
            return;
        }

        if let Some(entry) = loopback.pop().await {
            deserializer.push(&entry.buffer);
            if let Some(Ok(m)) = deserializer.try_deserialize() {
                while !inbound.put(m.message.clone(), loopback.clone()) && !loopback.is_cancelled() {
                    kernel_common::async_rt::yield_now().await;
                }
            }
        }
    }
}
