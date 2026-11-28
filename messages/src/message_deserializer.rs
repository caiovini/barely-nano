use alloc::{sync::Arc, vec::Vec};
use rsnano_core::ProtocolInfo;

use crate::{
    validate_header, DeserializedMessage, Message, MessageHeader, MessageType, NetworkFilter,
    ParseMessageError,
};

pub struct MessageDeserializer {
    buffer: Vec<u8>,
    cursor: usize,
    current_header: Option<MessageHeader>,
    network_filter: Option<Arc<NetworkFilter>>,
    protocol: ProtocolInfo,
}

impl MessageDeserializer {
    pub fn new(protocol: ProtocolInfo) -> Self {
        Self {
            buffer: Vec::with_capacity(Message::MAX_MESSAGE_SIZE),
            cursor: 0,
            current_header: None,
            network_filter: None,
            protocol,
        }
    }

    pub fn with_filter(protocol: ProtocolInfo, network_filter: Arc<NetworkFilter>) -> Self {
        Self {
            buffer: Vec::with_capacity(Message::MAX_MESSAGE_SIZE),
            cursor: 0,
            current_header: None,
            network_filter: Some(network_filter),
            protocol,
        }
    }

    #[inline]
    pub fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if self.cursor == self.buffer.len() {
            self.cursor = 0;
            self.buffer.clear();
        } else if self.cursor >= 4096
            || self.buffer.len() + data.len() > self.buffer.capacity()
        {
            let rem = self.buffer.len() - self.cursor;
            self.buffer.copy_within(self.cursor.., 0);
            self.buffer.truncate(rem);
            self.cursor = 0;
        }
        self.buffer.extend_from_slice(data);
    }

    #[inline]
    pub fn try_deserialize(&mut self) -> Option<Result<DeserializedMessage, ParseMessageError>> {
        let mut empty: &[u8] = &[];
        self.next_from_slice(&mut empty)
    }

    /// Vectorized 16-message `Publish(StateBlock)` fast-path deserializer.
    /// When at least 16 contiguous 224-byte `Publish(State)` frames are present in `*input`
    /// and no partial frame is buffered, validates headers, applies the duplicate filter,
    /// and computes all 16 176-byte `StateBlock` Blake2b hashes simultaneously using
    /// `StateBlock::finish_batch_16` (`blake2b::hash_batch_16`).
    pub fn next_publish_state_batch_16(
        &mut self,
        input: &mut &[u8],
    ) -> Option<[Result<DeserializedMessage, ParseMessageError>; 16]> {
        const FRAME_LEN: usize = MessageHeader::SERIALIZED_SIZE + 216; // 8 + 216 = 224 bytes
        if self.current_header.is_some()
            || self.buffer.len() != self.cursor
            || input.len() < 16 * FRAME_LEN
        {
            return None;
        }
        self.cursor = 0;
        self.buffer.clear();

        let mut headers: [MessageHeader; 16] = core::array::from_fn(|_| MessageHeader::default());
        for i in 0..16 {
            let base = i * FRAME_LEN;
            let hb = &input[base..base + MessageHeader::SERIALIZED_SIZE];
            if hb[5] != MessageType::Publish as u8 {
                return None;
            }
            let Ok(hdr) = MessageHeader::deserialize_slice(hb) else {
                return None;
            };
            if validate_header(&hdr, &self.protocol).is_err() {
                return None;
            }
            if crate::Publish::block_type(hdr.extensions) != rsnano_core::BlockType::State {
                return None;
            }
            headers[i] = hdr;
        }

        let mut digests = [0u128; 16];
        let mut dup_err: [Option<ParseMessageError>; 16] = [const { None }; 16];
        let mut parts: [(
            rsnano_core::Account,
            rsnano_core::BlockHash,
            rsnano_core::PublicKey,
            rsnano_core::Amount,
            rsnano_core::Link,
            rsnano_core::Signature,
            rsnano_core::WorkNonce,
        ); 16] = core::array::from_fn(|_| {
            (
                rsnano_core::Account::zero(),
                rsnano_core::BlockHash::zero(),
                rsnano_core::PublicKey::default(),
                rsnano_core::Amount::zero(),
                rsnano_core::Link::zero(),
                rsnano_core::Signature::default(),
                rsnano_core::WorkNonce::default(),
            )
        });

        for i in 0..16 {
            let p_start = i * FRAME_LEN + MessageHeader::SERIALIZED_SIZE;
            let payload = &input[p_start..p_start + 216];
            match self.filter_duplicate_messages(MessageType::Publish, payload) {
                Ok(d) => digests[i] = d,
                Err(e) => {
                    dup_err[i] = Some(e);
                    continue;
                }
            }
            let acc = rsnano_core::Account::from_bytes(payload[0..32].try_into().unwrap());
            let prev = rsnano_core::BlockHash::from_bytes(payload[32..64].try_into().unwrap());
            let rep = rsnano_core::PublicKey::from_bytes(payload[64..96].try_into().unwrap());
            let bal = rsnano_core::Amount::raw(u128::from_be_bytes(
                payload[96..112].try_into().unwrap(),
            ));
            let link = rsnano_core::Link::from_bytes(payload[112..144].try_into().unwrap());
            let sig = rsnano_core::Signature::from_bytes(payload[144..208].try_into().unwrap());
            let work = u64::from_be_bytes(payload[208..216].try_into().unwrap()).into();
            parts[i] = (acc, prev, rep, bal, link, sig, work);
        }

        *input = &input[16 * FRAME_LEN..];
        let state_blocks = rsnano_core::StateBlock::finish_batch_16(parts);
        let mut block_iter = state_blocks.into_iter().enumerate();

        Some(core::array::from_fn(|_| {
            let (i, sb) = block_iter.next().unwrap();
            if let Some(err) = dup_err[i].clone() {
                Err(err)
            } else {
                let is_originator =
                    (headers[i].extensions.data & crate::Publish::ORIGINATOR_FLAG) > 0;
                let msg = Message::Publish(crate::Publish {
                    block: rsnano_core::Block::State(sb),
                    digest: digests[i],
                    is_originator,
                });
                Ok(DeserializedMessage::new(msg, headers[i].protocol))
            }
        }))
    }

    /// Zero-copy, zero-allocation in-place message deserializer.
    ///
    /// When no partial message is buffered in `self.buffer`, complete messages are parsed
    /// directly out of `*input` without copying bytes into `self.buffer` or allocating a
    /// temporary `Vec<u8>`. Any trailing partial message bytes at the end of `*input` are
    /// automatically saved into `self.buffer` for the next call.
    pub fn next_from_slice(
        &mut self,
        input: &mut &[u8],
    ) -> Option<Result<DeserializedMessage, ParseMessageError>> {
        // Step 1: Resolve the 8-byte MessageHeader if not already waiting for a payload.
        if self.current_header.is_none() {
            let buffered = self.buffer.len() - self.cursor;
            let header_bytes = if buffered == 0 {
                self.cursor = 0;
                self.buffer.clear();
                if input.len() < MessageHeader::SERIALIZED_SIZE {
                    if !input.is_empty() {
                        self.push(input);
                        *input = &[];
                    }
                    return None;
                }
                let mut hb = [0u8; MessageHeader::SERIALIZED_SIZE];
                hb.copy_from_slice(&input[..MessageHeader::SERIALIZED_SIZE]);
                *input = &input[MessageHeader::SERIALIZED_SIZE..];
                hb
            } else {
                if buffered < MessageHeader::SERIALIZED_SIZE {
                    let needed = MessageHeader::SERIALIZED_SIZE - buffered;
                    if input.len() < needed {
                        if !input.is_empty() {
                            self.push(input);
                            *input = &[];
                        }
                        return None;
                    }
                    self.push(&input[..needed]);
                    *input = &input[needed..];
                }
                let mut hb = [0u8; MessageHeader::SERIALIZED_SIZE];
                hb.copy_from_slice(
                    &self.buffer[self.cursor..self.cursor + MessageHeader::SERIALIZED_SIZE],
                );
                self.cursor += MessageHeader::SERIALIZED_SIZE;
                if self.cursor == self.buffer.len() {
                    self.cursor = 0;
                    self.buffer.clear();
                }
                hb
            };

            let Ok(header) = MessageHeader::deserialize_slice(&header_bytes) else {
                return Some(Err(ParseMessageError::InvalidHeader));
            };

            if let Err(e) = validate_header(&header, &self.protocol) {
                return Some(Err(e));
            }

            self.current_header = Some(header);
        }

        // Step 2: Resolve the message payload in-place.
        let header = self.current_header.as_ref().unwrap();
        let payload_len = header.payload_length();
        if payload_len == 0 {
            let header = self.current_header.take().unwrap();
            return Some(self.deserialize_payload_slice(header, &[]));
        }

        let buffered = self.buffer.len() - self.cursor;
        if buffered >= payload_len {
            let header = self.current_header.take().unwrap();
            let start = self.cursor;
            self.cursor += payload_len;
            let res = self.deserialize_payload_slice_from_buffer(header, start, payload_len);
            if self.cursor == self.buffer.len() {
                self.cursor = 0;
                self.buffer.clear();
            }
            return Some(res);
        }

        if buffered == 0 && input.len() >= payload_len {
            // Fast path: 0 bytes in `self.buffer` and the entire payload is contiguous in `*input`.
            let header = self.current_header.take().unwrap();
            let payload_slice = &input[..payload_len];
            *input = &input[payload_len..];
            return Some(self.deserialize_payload_slice(header, payload_slice));
        }

        let needed = payload_len - buffered;
        if input.len() >= needed {
            self.push(&input[..needed]);
            *input = &input[needed..];
            let header = self.current_header.take().unwrap();
            let start = self.cursor;
            self.cursor += payload_len;
            let res = self.deserialize_payload_slice_from_buffer(header, start, payload_len);
            if self.cursor == self.buffer.len() {
                self.cursor = 0;
                self.buffer.clear();
            }
            return Some(res);
        }

        // Incomplete payload: buffer the remaining input bytes and wait for the next TCP read.
        if !input.is_empty() {
            self.push(input);
            *input = &[];
        }
        None
    }

    #[inline]
    fn deserialize_payload_slice_from_buffer(
        &self,
        header: MessageHeader,
        start: usize,
        len: usize,
    ) -> Result<DeserializedMessage, ParseMessageError> {
        let payload_slice = &self.buffer[start..start + len];
        self.deserialize_payload_slice(header, payload_slice)
    }

    #[inline]
    fn deserialize_payload_slice(
        &self,
        header: MessageHeader,
        payload_slice: &[u8],
    ) -> Result<DeserializedMessage, ParseMessageError> {
        let digest = self.filter_duplicate_messages(header.message_type, payload_slice)?;

        let Some(message) = Message::deserialize(payload_slice, &header, digest) else {
            return Err(ParseMessageError::InvalidMessage(header.message_type));
        };

        Ok(DeserializedMessage::new(message, header.protocol))
    }

    /// Early filtering to not waste time deserializing duplicate blocks
    fn filter_duplicate_messages(
        &self,
        message_type: MessageType,
        payload_bytes: &[u8],
    ) -> Result<u128, ParseMessageError> {
        if matches!(message_type, MessageType::Publish | MessageType::ConfirmAck) {
            if let Some(filter) = self.network_filter.as_ref() {
                let (digest, existed) = filter.apply(payload_bytes);
                if existed {
                    if message_type == MessageType::ConfirmAck {
                        Err(ParseMessageError::DuplicateConfirmAckMessage)
                    } else {
                        Err(ParseMessageError::DuplicatePublishMessage)
                    }
                } else {
                    Ok(digest)
                }
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AscPullAck, Keepalive, MessageSerializer};

    mod happy_path {
        use super::*;

        #[test]
        fn empty() {
            let mut deserializer = create_deserializer();
            assert!(deserializer.try_deserialize().is_none());
        }

        #[test]
        fn deserialize_message_without_payload() {
            let mut deserializer = create_deserializer();
            deserializer.push(&message_bytes(&Message::TelemetryReq));

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(
                    Message::TelemetryReq,
                    Default::default()
                )))
            );
        }

        #[test]
        fn deserialize_message_with_payload() {
            let mut deserializer = create_deserializer();

            let keepalive = Message::Keepalive(Keepalive::new_test_instance());
            deserializer.push(&message_bytes(&keepalive));

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(keepalive, Default::default())))
            );
        }

        #[test]
        fn deserialize_multiple_messages() {
            let mut deserializer = create_deserializer();

            let keepalive = Message::Keepalive(Keepalive::new_test_instance());
            deserializer.push(&message_bytes(&Message::TelemetryReq));
            deserializer.push(&message_bytes(&keepalive));
            deserializer.push(&message_bytes(&Message::TelemetryReq));

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(
                    Message::TelemetryReq,
                    Default::default()
                )))
            );
            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(keepalive, Default::default())))
            );
            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(
                    Message::TelemetryReq,
                    Default::default()
                )))
            );
            assert_eq!(deserializer.try_deserialize(), None);
        }

        #[test]
        fn zero_copy_next_from_slice_and_segment_straddling() {
            let mut deserializer = create_deserializer();
            let keepalive = Message::Keepalive(Keepalive::new_test_instance());
            let mut stream = Vec::new();
            stream.extend_from_slice(&message_bytes(&Message::TelemetryReq));
            stream.extend_from_slice(&message_bytes(&keepalive));
            stream.extend_from_slice(&message_bytes(&Message::TelemetryReq));

            // 1. Contiguous fast-path: parses all 3 messages directly from slice with 0 buffer usage
            let mut slice = stream.as_slice();
            assert_eq!(
                deserializer.next_from_slice(&mut slice),
                Some(Ok(DeserializedMessage::new(
                    Message::TelemetryReq,
                    Default::default()
                )))
            );
            assert_eq!(
                deserializer.next_from_slice(&mut slice),
                Some(Ok(DeserializedMessage::new(keepalive.clone(), Default::default())))
            );
            assert_eq!(
                deserializer.next_from_slice(&mut slice),
                Some(Ok(DeserializedMessage::new(
                    Message::TelemetryReq,
                    Default::default()
                )))
            );
            assert!(slice.is_empty());
            assert_eq!(deserializer.next_from_slice(&mut slice), None);

            // 2. Straddling header (3 bytes) + straddling payload (header + 17 bytes)
            let ka_bytes = message_bytes(&keepalive);
            let mut chunk1 = &ka_bytes[..3];
            assert_eq!(deserializer.next_from_slice(&mut chunk1), None);
            assert!(chunk1.is_empty());

            let mut chunk2 = &ka_bytes[3..25];
            assert_eq!(deserializer.next_from_slice(&mut chunk2), None);
            assert!(chunk2.is_empty());

            let mut chunk3 = &ka_bytes[25..];
            assert_eq!(
                deserializer.next_from_slice(&mut chunk3),
                Some(Ok(DeserializedMessage::new(keepalive, Default::default())))
            );
            assert!(chunk3.is_empty());
        }
    }

    mod unhappy_path {
        use super::*;
        use crate::{ConfirmAck, Publish};
        use rsnano_core::Networks;

        #[test]
        fn push_incomplete_header() {
            let mut deserializer = create_deserializer();
            deserializer.push(&[1, 2]);
            assert!(deserializer.try_deserialize().is_none());
        }

        #[test]
        fn invalid_header() {
            let mut deserializer = create_deserializer();
            deserializer.push(&[1, 2, 3, 4, 5, 6, 7, 8]);

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Err(ParseMessageError::InvalidHeader))
            );
        }

        #[test]
        fn invalid_network() {
            let mut deserializer = create_deserializer();

            let mut serializer =
                MessageSerializer::new(ProtocolInfo::default_for(Networks::NanoBetaNetwork));
            let message = serializer.serialize(&Message::TelemetryReq);

            deserializer.push(&message);

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Err(ParseMessageError::InvalidNetwork))
            );
        }

        #[test]
        fn invalid_payload() {
            let mut deserializer = create_deserializer();
            deserializer.push(&invalid_asc_pull_ack_bytes());

            assert_eq!(
                deserializer.try_deserialize(),
                Some(Err(ParseMessageError::InvalidMessage(
                    MessageType::AscPullAck
                )))
            );
        }

        #[test]
        fn incomplete_playload() {
            let mut deserializer = create_deserializer();
            let keepalive = Message::Keepalive(Keepalive::new_test_instance());
            let data = message_bytes(&keepalive);

            // push incomplete message
            deserializer.push(&data[..data.len() - 1]);
            assert_eq!(deserializer.try_deserialize(), None);

            // push missing byte
            deserializer.push(&data[data.len() - 1..]);
            assert_eq!(
                deserializer.try_deserialize(),
                Some(Ok(DeserializedMessage::new(keepalive, Default::default())))
            );
        }

        // Send two publish messages and asserts that the duplication is detected.
        #[test]
        fn duplicate_publish_message() {
            let mut deserializer = create_deserializer();

            let message = Message::Publish(Publish::new_test_instance());
            let message_bytes = message_bytes(&message);

            deserializer.push(&message_bytes);
            deserializer.try_deserialize();

            deserializer.push(&message_bytes);
            let result = deserializer.try_deserialize();

            assert_eq!(
                result,
                Some(Err(ParseMessageError::DuplicatePublishMessage))
            );
        }
        //
        // Send two publish messages and asserts that the duplication is detected.
        #[test]
        fn duplicate_confirm_ack() {
            let mut deserializer = create_deserializer();
            let message = Message::ConfirmAck(ConfirmAck::new_test_instance());
            let message_bytes = message_bytes(&message);

            deserializer.push(&message_bytes);
            deserializer.try_deserialize();

            deserializer.push(&message_bytes);
            let result = deserializer.try_deserialize();

            assert_eq!(
                result,
                Some(Err(ParseMessageError::DuplicateConfirmAckMessage))
            );
        }
    }

    fn message_bytes(message: &Message) -> Vec<u8> {
        let mut serializer = MessageSerializer::default();
        serializer.serialize(message).to_vec()
    }

    fn invalid_asc_pull_ack_bytes() -> Vec<u8> {
        let mut data = message_bytes(&Message::AscPullAck(AscPullAck::new_test_instance_blocks()));
        // make message invalid:
        data[MessageHeader::SERIALIZED_SIZE..].fill(0xFF);
        data
    }

    fn create_deserializer() -> MessageDeserializer {
        let filter = Arc::new(NetworkFilter::default());
        MessageDeserializer::with_filter(ProtocolInfo::default(), filter)
    }
}
