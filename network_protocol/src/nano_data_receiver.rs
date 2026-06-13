use alloc::sync::{Arc, Weak};
use core::sync::atomic::Ordering;
use spin::{Mutex, RwLock};

use tracing::warn;

use rsnano_core::{NodeId, ProtocolInfo};
use rsnano_messages::*;
use rsnano_network::{Channel, ChannelMode, DataReceiver, Network, ReceiveResult, TrafficType};
use rsnano_stats::{DetailType, Direction, StatType, Stats};

use crate::{HandshakeProcess, HandshakeStats, HandshakeStatus, LatestKeepalives};

pub struct NanoDataReceiver {
    try_enqueue: Arc<dyn Fn(Message, Arc<Channel>) -> bool + Send + Sync>,
    channel: Arc<Channel>,
    handshake_process: HandshakeProcess,
    serializer: MessageSerializer,
    message_deserializer: MessageDeserializer,
    latest_keepalives: Arc<Mutex<LatestKeepalives>>,
    stats: Arc<Stats>,
    network: Weak<RwLock<Network>>,
    first_message: bool,
    node_id: NodeId,
    handshake_stats: Arc<HandshakeStats>,
    retry_enqueue: Option<Message>,
}

impl NanoDataReceiver {
    pub fn new(
        channel: Arc<Channel>,
        handshake_process: HandshakeProcess,
        message_deserializer: MessageDeserializer,
        try_enqueue: Arc<dyn Fn(Message, Arc<Channel>) -> bool + Send + Sync>,
        latest_keepalives: Arc<Mutex<LatestKeepalives>>,
        stats: Arc<Stats>,
        network: Weak<RwLock<Network>>,
        handshake_stats: Arc<HandshakeStats>,
        protocol: ProtocolInfo,
    ) -> Self {
        Self {
            try_enqueue,
            channel,
            handshake_process,
            serializer: MessageSerializer::new(protocol),
            message_deserializer,
            latest_keepalives,
            stats,
            network,
            first_message: true,
            node_id: NodeId::ZERO,
            handshake_stats,
            retry_enqueue: None,
        }
    }

    pub fn ensure_handshake(&mut self) {
        if self.channel.direction() == rsnano_network::ChannelDirection::Outbound {
            self.initiate_handshake();
        }
    }

    fn initiate_handshake(&mut self) {
        let peer = self.channel.peer_addr();
        let result = self.handshake_process.initiate_handshake(&peer);

        match result {
            Ok(_) => {
                tracing::debug!(
                    "Initiating handshake query for {} ({:?})",
                    peer,
                    self.channel.direction()
                );
                self.handshake_stats
                    .handshakes_sent
                    .fetch_add(1, Ordering::Relaxed);
                kernel_common::uart::putchar(b'q');
                self.handshake_stats
                    .initiate
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                warn!("Could not initiate handshake for {}: {:?}", peer, e);
                self.channel.close();
            }
        }
    }

    fn queue_realtime(&mut self, message: Message) -> ReceiveResult {
        tracing::debug!(
            "Queuing realtime message: {:?} from {}",
            message.message_type(),
            self.channel.peer_addr()
        );
        let enqueued = self.try_enqueue(message.clone());
        if enqueued {
            ReceiveResult::Continue
        } else {
            tracing::warn!(
                "Message queue full! Pausing receiver for {}",
                self.channel.peer_addr()
            );
            self.retry_enqueue = Some(message);
            ReceiveResult::Pause
        }
    }

    fn try_enqueue(&self, message: Message) -> bool {
        (self.try_enqueue)(message, self.channel.clone())
    }

    fn set_last_keepalive(&self, keepalive: Keepalive) {
        self.latest_keepalives
            .lock()
            .insert(self.channel.channel_id(), keepalive);
    }

    fn process_realtime(&mut self, message: Message) -> ReceiveResult {
        let process = match &message {
            Message::Keepalive(keepalive) => {
                kernel_common::uart::putchar(b'K');
                self.set_last_keepalive(keepalive.clone());
                true
            }
            Message::Publish(_) => {
                kernel_common::uart::putchar(b'B');
                true
            }
            Message::AscPullAck(_)
            | Message::AscPullReq(_)
            | Message::ConfirmAck(_)
            | Message::ConfirmReq(_)
            | Message::FrontierReq(_)
            | Message::TelemetryAck(_) => true,
            _ => false,
        };

        if process {
            self.queue_realtime(message)
        } else {
            kernel_common::uart::putchar(b'*');
            ReceiveResult::Continue
        }
    }

    fn to_realtime_connection(&self, node_id: &NodeId) -> bool {
        if self.channel.mode() != ChannelMode::Undefined {
            return false;
        }

        let Some(network) = self.network.upgrade() else {
            return false;
        };

        let result = network
            .read()
            .upgrade_to_realtime_connection(self.channel.channel_id(), *node_id);

        if let Some((channel, observers)) = result {
            for observer in observers {
                observer(channel.clone());
            }

            kernel_common::uart::putchar(b'u');
            self.stats
                .inc(StatType::TcpChannels, DetailType::ChannelAccepted);
            true
        } else {
            tracing::info!(
                channel_id = ?self.channel.channel_id(),
                peer = %self.channel.peer_addr(),
                %node_id,
                "Could not upgrade channel to realtime connection",
            );
            false
        }
    }

    fn process_message(&mut self, message: Message) -> ReceiveResult {
        self.stats.inc_dir(
            StatType::TcpServer,
            DetailType::from(message.message_type()),
            Direction::In,
        );

        if self.channel.mode() == ChannelMode::Undefined {
            let (mut status, response) = match &message {
                Message::NodeIdHandshake(payload) => {
                    self.handshake_stats
                        .handshakes_received
                        .fetch_add(1, Ordering::Relaxed);

                    let (status, response) = self.handshake_process.process_handshake(payload);
                    kernel_common::uart::putchar(b'h');
                    if response.is_some() {
                        kernel_common::uart::putchar(b'k');
                        self.handshake_stats
                            .response_ok
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    (status, response)
                }

                _ => (HandshakeStatus::Handshake, None),
            };

            if let Some(response) = response {
                let buffer = self
                    .serializer
                    .serialize(&Message::NodeIdHandshake(response));

                let enqueued = self.channel.send(buffer, TrafficType::Generic);
                if enqueued {
                    self.handshake_stats
                        .handshakes_sent
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    kernel_common::uart::putchar(b'X');
                    status = HandshakeStatus::Abort;
                }
            }

            match status {
                HandshakeStatus::Abort | HandshakeStatus::AbortOwnNodeId => {
                    kernel_common::uart::putchar(b'f');
                    tracing::warn!(
                        "Handshake aborted with {} (status: {:?})",
                        self.channel.peer_addr(),
                        status
                    );
                    if matches!(status, HandshakeStatus::AbortOwnNodeId) {
                        if let Some(peering_addr) = self.channel.peering_addr() {
                            if let Some(network) = self.network.upgrade() {
                                network.write().perma_ban(peering_addr);
                            }
                        }
                    }
                    return ReceiveResult::Abort;
                }
                HandshakeStatus::Handshake => {
                    return ReceiveResult::Continue;
                }
                HandshakeStatus::Realtime(node_id) => {
                    kernel_common::uart::putchar(b'h');
                    self.node_id = node_id;
                    return ReceiveResult::Pause;
                }
            }
        } else if self.channel.mode() == ChannelMode::Realtime {
            return self.process_realtime(message);
        }

        kernel_common::uart::putchar(b'!');
        ReceiveResult::Abort
    }
}

impl DataReceiver for NanoDataReceiver {
    fn receive(&mut self, data: &[u8]) -> ReceiveResult {
        kernel_common::uart::putchar(b'i');
        self.message_deserializer.push(data);
        while let Some(result) = self.message_deserializer.try_deserialize() {
            let result = match result {
                Ok(msg) => {
                    kernel_common::uart::putchar(b'm');
                    if self.first_message {
                        self.channel
                            .set_protocol_version(msg.protocol.version_using);
                        self.first_message = false;
                    }
                    self.process_message(msg.message)
                }
                Err(e) => {
                    if matches!(
                        e,
                        ParseMessageError::DuplicateConfirmAckMessage
                            | ParseMessageError::DuplicatePublishMessage
                    ) {
                        ReceiveResult::Continue
                    } else {
                        tracing::debug!(
                            "Message deserialization error: {:?} (addr: {})",
                            e,
                            self.channel.peer_addr()
                        );
                        ReceiveResult::Abort
                    }
                }
            };

            if !matches!(result, ReceiveResult::Continue) {
                return result;
            }
        }

        ReceiveResult::Continue
    }

    fn ensure_handshake(&mut self) {
        if self.channel.direction() == rsnano_network::ChannelDirection::Outbound {
            self.initiate_handshake();
        }
    }

    fn try_unpause(&self) -> ReceiveResult {
        let mode = self.channel.mode();
        match mode {
            ChannelMode::Undefined => {
                kernel_common::uart::putchar(b'P');
                let q_len = self.channel.queue_len();
                if q_len > 0 {
                    return ReceiveResult::Pause;
                }

                kernel_common::uart::putchar(b'C');
                if !self.to_realtime_connection(&self.node_id) {
                    return ReceiveResult::Abort;
                }

                ReceiveResult::Continue
            }
            ChannelMode::Realtime => {
                let message = self.retry_enqueue.clone();
                match message {
                    Some(message) => {
                        if self.try_enqueue(message) {
                            ReceiveResult::Continue
                        } else {
                            ReceiveResult::Pause
                        }
                    }
                    None => ReceiveResult::Continue,
                }
            }
        }
    }
}

impl Drop for NanoDataReceiver {
    fn drop(&mut self) {
        self.channel.close();
    }
}
