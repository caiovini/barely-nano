mod block_details;
mod block_priority;

pub use block_details::BlockDetails;
pub use block_priority::block_priority;

mod block_sideband;
pub use block_sideband::BlockSideband;

mod change_block;
use change_block::JsonChangeBlock;
pub use change_block::{valid_change_block_predecessor, ChangeBlock, ChangeBlockArgs};

mod open_block;
pub use open_block::{JsonOpenBlock, OpenBlock, OpenBlockArgs};

mod receive_block;
pub use receive_block::{
    valid_receive_block_predecessor, JsonReceiveBlock, ReceiveBlock, ReceiveBlockArgs,
};

mod send_block;
use send_block::JsonSendBlock;
pub use send_block::{valid_send_block_predecessor, SendBlock, SendBlockArgs};

mod state_block;
pub use state_block::{EpochBlockArgs, JsonStateBlock, StateBlock, StateBlockArgs};

mod builders;
pub use builders::*;

use crate::{
    utils::{BufferWriter, Deserialize, MemoryStream, Serialize, Stream, UnixMillisTimestamp},
    Account, Amount, BlockHash, Epoch, Epochs, Link, PrivateKey, PublicKey, QualifiedRoot, Root,
    Signature, WorkNonce,
};
use num::FromPrimitive;
use core::ops::{Deref, DerefMut};
use spin::Lazy;
use anyhow::bail;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::borrow::ToOwned;

#[repr(u8)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, FromPrimitive)]
pub enum BlockType {
    Invalid = 0,
    NotABlock = 1,
    LegacySend = 2,
    LegacyReceive = 3,
    LegacyOpen = 4,
    LegacyChange = 5,
    State = 6,
}

impl TryFrom<BlockType> for BlockSubType {
    type Error = anyhow::Error;

    fn try_from(value: BlockType) -> Result<Self, Self::Error> {
        match value {
            BlockType::LegacySend => Ok(BlockSubType::Send),
            BlockType::LegacyReceive => Ok(BlockSubType::Receive),
            BlockType::LegacyOpen => Ok(BlockSubType::Open),
            BlockType::LegacyChange => Ok(BlockSubType::Change),
            BlockType::State => Ok(BlockSubType::Send),
            BlockType::Invalid | BlockType::NotABlock => {
                Err(anyhow!("Invalid block type for conversion to subtype"))
            }
        }
    }
}

impl TryFrom<u8> for BlockType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        FromPrimitive::from_u8(value).ok_or_else(|| anyhow!("invalid block type value"))
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum BlockSubType {
    Send,
    Receive,
    Open,
    Change,
    Epoch,
}

impl BlockSubType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockSubType::Send => "send",
            BlockSubType::Receive => "receive",
            BlockSubType::Open => "open",
            BlockSubType::Change => "change",
            BlockSubType::Epoch => "epoch",
        }
    }
}

pub trait BlockBase {
    fn block_type(&self) -> BlockType;
    fn account_field(&self) -> Option<Account>;
    fn hash(&self) -> BlockHash;
    fn link_field(&self) -> Option<Link>;
    fn signature(&self) -> &Signature;
    fn set_signature(&mut self, signature: Signature);
    fn work(&self) -> WorkNonce;
    fn set_work(&mut self, work: WorkNonce);
    fn previous(&self) -> BlockHash;
    fn serialize_without_block_type(&self, writer: &mut dyn BufferWriter);
    fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&self.json_representation())?)
    }
    fn json_representation(&self) -> JsonBlock;
    fn root(&self) -> Root;
    fn balance_field(&self) -> Option<Amount>;
    /// Source block for open/receive blocks, zero otherwise.
    fn source_field(&self) -> Option<BlockHash>;
    fn representative_field(&self) -> Option<PublicKey>;
    fn destination_field(&self) -> Option<Account>;
    fn qualified_root(&self) -> QualifiedRoot {
        QualifiedRoot::new(self.root(), self.previous())
    }
    fn valid_predecessor(&self, block_type: BlockType) -> bool;
    fn verify_signature(&self, pub_key: &PublicKey) -> anyhow::Result<()> {
        pub_key.verify(self.hash().as_bytes(), self.signature())
    }
}

pub fn serialized_block_size(block_type: BlockType) -> usize {
    match block_type {
        BlockType::Invalid | BlockType::NotABlock => 0,
        BlockType::LegacySend => SendBlock::serialized_size(),
        BlockType::LegacyReceive => ReceiveBlock::serialized_size(),
        BlockType::LegacyOpen => OpenBlock::serialized_size(),
        BlockType::LegacyChange => ChangeBlock::serialized_size(),
        BlockType::State => StateBlock::serialized_size(),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Block {
    LegacySend(SendBlock),
    LegacyReceive(ReceiveBlock),
    LegacyOpen(OpenBlock),
    LegacyChange(ChangeBlock),
    State(StateBlock),
}

impl Block {
    pub fn new_test_instance() -> Self {
        let key = PrivateKey::from(42);
        Self::new_test_instance_with_key(key)
    }

    pub fn new_test_open() -> Self {
        let key = PrivateKey::from(42);
        StateBlockArgs {
            key: &key,
            previous: BlockHash::zero(),
            representative: 789.into(),
            balance: 420.into(),
            link: 111.into(),
            work: 69420.into(),
        }
        .into()
    }

    pub fn new_test_instance_with_key(key: impl Into<PrivateKey>) -> Self {
        let key = key.into();
        StateBlockArgs {
            key: &key,
            previous: BlockHash::from_bytes(*key.account().as_bytes()),
            representative: 789.into(),
            balance: 420.into(),
            link: 111.into(),
            work: 69420.into(),
        }
        .into()
    }

    pub fn block_type(&self) -> BlockType {
        self.as_block().block_type()
    }

    pub fn as_block_mut(&mut self) -> &mut dyn BlockBase {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
        }
    }

    pub fn as_block(&self) -> &dyn BlockBase {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
        }
    }

    pub fn is_open(&self) -> bool {
        match &self {
            Block::LegacyOpen(_) => true,
            Block::State(state) => state.previous().is_zero(),
            _ => false,
        }
    }

    pub fn is_legacy(&self) -> bool {
        !matches!(self, Block::State(_))
    }

    pub fn is_change(&self) -> bool {
        match self {
            Block::LegacyChange(_) => true,
            Block::State(state) => state.link().is_zero(),
            _ => false,
        }
    }

    pub fn source_or_link(&self) -> BlockHash {
        self.source_field()
            .unwrap_or_else(|| self.link_field().unwrap_or_default().into())
    }

    pub fn destination_or_link(&self) -> Account {
        self.destination_field()
            .unwrap_or_else(|| self.link_field().unwrap_or_default().into())
    }

    pub fn serialize(&self, stream: &mut dyn BufferWriter) {
        let block_type = self.block_type() as u8;
        stream.write_u8_safe(block_type);
        self.serialize_without_block_type(stream);
    }

    pub fn deserialize_block_type(
        block_type: BlockType,
        stream: &mut dyn Stream,
    ) -> anyhow::Result<Self> {
        Self::deserialize_block_type_with_hash(block_type, stream, None)
    }

    pub fn deserialize_block_type_with_hash(
        block_type: BlockType,
        stream: &mut dyn Stream,
        known_hash: Option<BlockHash>,
    ) -> anyhow::Result<Self> {
        let block = match block_type {
            BlockType::LegacyReceive => {
                Self::LegacyReceive(ReceiveBlock::deserialize_with_hash(stream, known_hash)?)
            }
            BlockType::LegacyOpen => {
                Self::LegacyOpen(OpenBlock::deserialize_with_hash(stream, known_hash)?)
            }
            BlockType::LegacyChange => {
                Self::LegacyChange(ChangeBlock::deserialize_with_hash(stream, known_hash)?)
            }
            BlockType::State => Self::State(StateBlock::deserialize_with_hash(stream, known_hash)?),
            BlockType::LegacySend => {
                Self::LegacySend(SendBlock::deserialize_with_hash(stream, known_hash)?)
            }
            BlockType::Invalid | BlockType::NotABlock => bail!("invalid block type"),
        };
        Ok(block)
    }

    pub fn verify_signature(&self) -> anyhow::Result<()> {
        match self {
            Block::State(b) => b.verify_signature(),
            _ => anyhow::bail!("Legacy blocks require a public key to verify signature"),
        }
    }

    pub fn verify_with_key(&self, pub_key: &PublicKey) -> anyhow::Result<()> {
        if self.is_signature_verified() {
            return Ok(());
        }
        let res = pub_key.verify(self.hash().as_bytes(), self.signature());
        if res.is_ok() {
            self.set_signature_verified(true);
        }
        res
    }

    #[inline(always)]
    pub fn is_signature_verified(&self) -> bool {
        match self {
            Block::State(b) => b.is_signature_verified(),
            Block::LegacySend(b) => b.is_signature_verified(),
            Block::LegacyReceive(b) => b.is_signature_verified(),
            Block::LegacyOpen(b) => b.is_signature_verified(),
            Block::LegacyChange(b) => b.is_signature_verified(),
        }
    }

    #[inline(always)]
    pub fn set_signature_verified(&self, verified: bool) {
        match self {
            Block::State(b) => b.set_signature_verified(verified),
            Block::LegacySend(b) => b.set_signature_verified(verified),
            Block::LegacyReceive(b) => b.set_signature_verified(verified),
            Block::LegacyOpen(b) => b.set_signature_verified(verified),
            Block::LegacyChange(b) => b.set_signature_verified(verified),
        }
    }

    #[inline(always)]
    pub fn is_pow_base_verified(&self) -> bool {
        match self {
            Block::State(b) => b.is_pow_base_verified(),
            _ => false,
        }
    }

    #[inline(always)]
    pub fn set_pow_base_verified(&self, verified: bool) {
        if let Block::State(b) = self {
            b.set_pow_base_verified(verified);
        }
    }

    pub fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Block> {
        Self::deserialize_with_hash(stream, None)
    }

    pub fn deserialize_with_hash(
        stream: &mut dyn Stream,
        known_hash: Option<BlockHash>,
    ) -> anyhow::Result<Block> {
        let block_type =
            BlockType::from_u8(stream.read_u8()?).ok_or_else(|| anyhow!("invalid block type"))?;
        Self::deserialize_block_type_with_hash(block_type, stream, known_hash)
    }
}

impl Serialize for Block {
    fn serialize(&self, stream: &mut dyn BufferWriter) {
        let block_type = self.block_type() as u8;
        stream.write_u8_safe(block_type);
        self.serialize_without_block_type(stream);
    }
}

impl From<Block> for serde_json::Value {
    fn from(value: Block) -> Self {
        serde_json::to_value(value.json_representation()).unwrap()
    }
}

impl From<SavedBlock> for serde_json::Value {
    fn from(value: SavedBlock) -> Self {
        let mut result = serde_json::to_value(value.block.json_representation()).unwrap();
        if let serde_json::Value::Object(obj) = &mut result {
            obj.insert(
                "subtype".to_string(),
                serde_json::Value::String(value.subtype().as_str().to_owned()),
            );
        }
        result
    }
}

impl Deref for Block {
    type Target = dyn BlockBase;

    fn deref(&self) -> &Self::Target {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
        }
    }
}

impl DerefMut for Block {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
        }
    }
}

impl serde::Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let json = self.as_block().json_representation();
        json.serialize(serializer)
    }
}

#[derive(PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonBlock {
    Open(JsonOpenBlock),
    Change(JsonChangeBlock),
    Receive(JsonReceiveBlock),
    Send(JsonSendBlock),
    State(JsonStateBlock),
}

impl<'de> serde::Deserialize<'de> for Block {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let json_block = JsonBlock::deserialize(deserializer)?;
        Ok(json_block.into())
    }
}

impl From<JsonBlock> for Block {
    fn from(value: JsonBlock) -> Self {
        match value {
            JsonBlock::Open(open) => Block::LegacyOpen(open.into()),
            JsonBlock::Change(change) => Block::LegacyChange(change.into()),
            JsonBlock::Receive(receive) => Block::LegacyReceive(receive.into()),
            JsonBlock::Send(send) => Block::LegacySend(send.into()),
            JsonBlock::State(state) => Block::State(state.into()),
        }
    }
}

impl From<Block> for JsonBlock {
    fn from(value: Block) -> Self {
        value.as_block().json_representation()
    }
}

impl From<&Block> for JsonBlock {
    fn from(value: &Block) -> Self {
        value.as_block().json_representation()
    }
}

/// A block with additional data about that block (the "sideband")
/// which is only known when the block is saved.
/// The sideband contains additional data like block height, block time, etc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedBlock {
    block: Block,
    sideband: BlockSideband,
}

impl SavedBlock {
    pub fn new(block: Block, sideband: BlockSideband) -> Self {
        Self { block, sideband }
    }

    pub fn new_test_open_block() -> Self {
        let block = Block::new_test_open();
        let sideband = BlockSideband {
            height: 1,
            details: BlockDetails::new(Epoch::Epoch2, false, true, false),
            ..BlockSideband::new_test_instance_for(&block)
        };
        Self::new(block, sideband)
    }

    pub fn new_test_change_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, false, false, false))
    }

    pub fn new_test_send_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, true, false, false))
    }

    pub fn new_test_receive_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, false, true, false))
    }

    fn new_test_block_with_details(details: BlockDetails) -> Self {
        let block = Block::new_test_instance();
        let sideband = BlockSideband {
            details,
            ..BlockSideband::new_test_instance_for(&block)
        };
        Self::new(block, sideband)
    }

    pub fn new_test_instance() -> Self {
        let block = Block::new_test_instance();
        Self::new_test_instance_with(block)
    }

    pub fn new_test_instance_with(block: Block) -> Self {
        let sideband = Self::test_sideband(&block);
        Self::new(block, sideband)
    }

    pub fn new_test_instance_with_key(key: impl Into<PrivateKey>) -> Self {
        let block = Block::new_test_instance_with_key(key);
        let sideband = Self::test_sideband(&block);
        Self::new(block, sideband)
    }

    fn test_sideband(block: &Block) -> BlockSideband {
        BlockSideband {
            height: 2,
            timestamp: 222222.into(),
            account: block.account_field().unwrap(),
            balance: block.balance_field().unwrap(),
            details: BlockDetails::new(Epoch::Epoch2, true, false, false),
            source_epoch: Epoch::Epoch0,
        }
    }

    pub fn account(&self) -> Account {
        match self.account_field() {
            Some(account) => account,
            None => self.sideband.account,
        }
    }

    pub fn height(&self) -> u64 {
        self.sideband.height
    }

    pub fn timestamp(&self) -> UnixMillisTimestamp {
        self.sideband.timestamp
    }

    pub fn subtype(&self) -> BlockSubType {
        self.sideband.details.subtype()
    }

    pub fn epoch(&self) -> Epoch {
        self.sideband.details.epoch
    }

    pub fn is_epoch(&self) -> bool {
        self.sideband.details.is_epoch
    }

    pub fn is_receive(&self) -> bool {
        self.sideband.details.is_receive
    }

    pub fn is_send(&self) -> bool {
        match &self.block {
            Block::LegacySend(_) => true,
            Block::State(_) => self.sideband.details.is_send,
            _ => false,
        }
    }

    pub fn source(&self) -> Option<BlockHash> {
        match &self.block {
            Block::LegacyOpen(i) => Some(i.source()),
            Block::LegacyReceive(i) => Some(i.source()),
            Block::State(i) => {
                if self.sideband.details.is_receive {
                    Some(i.link().into())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn source_epoch(&self) -> Epoch {
        self.sideband.source_epoch
    }

    pub fn destination(&self) -> Option<Account> {
        match &self.block {
            Block::LegacySend(i) => Some(i.destination()),
            Block::State(i) => {
                if self.sideband.details.is_send {
                    Some(i.link().into())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn serialize_with_sideband(&self) -> Vec<u8> {
        let mut stream = MemoryStream::new();
        self.block.serialize(&mut stream);
        self.sideband
            .serialize(&mut stream, self.block.block_type());
        stream.to_vec()
    }

    pub fn balance(&self) -> Amount {
        match &self.block {
            Block::LegacySend(b) => b.balance(),
            Block::State(b) => b.balance(),
            _ => self.sideband.balance,
        }
    }

    pub fn details(&self) -> &BlockDetails {
        &self.sideband.details
    }

    pub fn sideband(&self) -> &BlockSideband {
        &self.sideband
    }

    /// There can be at most two dependencies per block, namely "previous" and "link/source".
    pub fn dependent_blocks(&self, epochs: &Epochs, genesis_account: &Account) -> DependentBlocks {
        match &self.block {
            Block::LegacySend(b) => b.dependent_blocks(),
            Block::LegacyChange(b) => b.dependent_blocks(),
            Block::LegacyReceive(b) => b.dependent_blocks(),
            Block::LegacyOpen(b) => b.dependent_blocks(genesis_account),
            Block::State(state) => {
                let link_refers_to_block = !self.is_send() && !epochs.is_epoch_link(&state.link());
                let linked_block = if link_refers_to_block {
                    state.link().into()
                } else {
                    BlockHash::zero()
                };
                DependentBlocks::new(self.previous(), linked_block)
            }
        }
    }
}

impl Deref for SavedBlock {
    type Target = Block;

    fn deref(&self) -> &Self::Target {
        &self.block
    }
}

impl DerefMut for SavedBlock {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.block
    }
}

impl Serialize for SavedBlock {
    fn serialize(&self, stream: &mut dyn BufferWriter) {
        self.block.serialize(stream);
        self.sideband.serialize(stream, self.block.block_type());
    }
}

impl From<SavedBlock> for Block {
    fn from(value: SavedBlock) -> Self {
        value.block
    }
}

/// Bit 7 (`0x80`): Indicates the self-contained bit-packed compact `SavedBlock` format in `TABLE_BLOCKS`.
pub const COMPACT_BLOCK_TAG: u8 = 0x80;
/// Bit 6 (`0x40`): Indicates a non-zero 32-byte `successor` is appended at `val[val.len() - 32..]`.
/// When 0, `successor` is `BlockHash::zero()` and the 32 zero bytes are omitted.
pub const COMPACT_HAS_SUCCESSOR: u8 = 0x40;
/// Bit 5 (`0x20`): When 0, the block is a bit-packed `StateBlock`; when 1, it is a legacy block (`val[0] & 0x1F` = `BlockType`).
pub const COMPACT_IS_LEGACY: u8 = 0x20;

const COMPACT_STATE_HAS_PREV: u8 = 0x10;
const COMPACT_STATE_BAL_SCALED: u8 = 0x01;
const RAW_BALANCE_SCALE: u128 = 1_000_000_000_000_000_000_000_000; // 10^24 raw (10^-6 NANO)

const EPOCH_V1_LINK_BYTES: [u8; 32] = *b"epoch v1 block\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
const EPOCH_V2_LINK_BYTES: [u8; 32] = *b"epoch v2 block\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

#[repr(C)]
struct CompactRepTable {
    count: usize,
    by_id: [[u8; 32]; 256],
    sorted_len: usize,
    sorted_keys: [[u8; 32]; 256],
    sorted_ids: [u8; 256],
}

impl CompactRepTable {
    const fn empty() -> Self {
        Self {
            count: 0,
            by_id: [[0u8; 32]; 256],
            sorted_len: 0,
            sorted_keys: [[0u8; 32]; 256],
            sorted_ids: [0u8; 256],
        }
    }
}

struct SyncRepSlots(core::cell::UnsafeCell<[CompactRepTable; 2]>);
// SAFETY: Writes to the inactive slot in `REP_TABLE_SLOTS` are serialized by `REP_TABLE_UPDATE_LOCK`
// and published via `ACTIVE_REP_SLOT` with `Release`/`Acquire` ordering.
unsafe impl Sync for SyncRepSlots {}

static REP_TABLE_SLOTS: SyncRepSlots = SyncRepSlots(core::cell::UnsafeCell::new([
    CompactRepTable::empty(),
    CompactRepTable::empty(),
]));
static ACTIVE_REP_SLOT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static REP_TABLE_UPDATE_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// Sets the active Top-256 Representative lookup table (up to 256 32-byte public keys).
/// Index `0..count` in `reps` maps to the 1-byte compact representative ID `0..count as u8`.
pub fn set_compact_representatives(reps: &[[u8; 32]]) {
    let _guard = REP_TABLE_UPDATE_LOCK.lock();
    let cur = ACTIVE_REP_SLOT.load(core::sync::atomic::Ordering::Relaxed) & 1;
    let next = 1 - cur;
    let count = reps.len().min(256);

    let mut pairs: [([u8; 32], u8); 256] = [([0u8; 32], 0); 256];
    let mut unique_len = 0usize;
    let mut by_id = [[0u8; 32]; 256];

    for (i, &key) in reps.iter().take(count).enumerate() {
        by_id[i] = key;
        // Insert into sorted `pairs` (insertion sort on <= 256 items, ignoring duplicate keys)
        let mut pos = 0usize;
        let mut dup = false;
        while pos < unique_len {
            match pairs[pos].0.cmp(&key) {
                core::cmp::Ordering::Less => pos += 1,
                core::cmp::Ordering::Equal => {
                    dup = true;
                    break;
                }
                core::cmp::Ordering::Greater => break,
            }
        }
        if !dup {
            let mut j = unique_len;
            while j > pos {
                pairs[j] = pairs[j - 1];
                j -= 1;
            }
            pairs[pos] = (key, i as u8);
            unique_len += 1;
        }
    }

    // SAFETY: `REP_TABLE_UPDATE_LOCK` is held and `next` is the inactive slot.
    unsafe {
        let slot = &mut (*REP_TABLE_SLOTS.0.get())[next];
        slot.count = count;
        slot.by_id = by_id;
        slot.sorted_len = unique_len;
        for idx in 0..unique_len {
            slot.sorted_keys[idx] = pairs[idx].0;
            slot.sorted_ids[idx] = pairs[idx].1;
        }
    }
    ACTIVE_REP_SLOT.store(next, core::sync::atomic::Ordering::Release);
}

/// Returns a copy of the currently active compact representative table (`0..count`).
pub fn compact_representatives() -> Vec<[u8; 32]> {
    let slot_idx = ACTIVE_REP_SLOT.load(core::sync::atomic::Ordering::Acquire) & 1;
    // SAFETY: `slot_idx` was published with `Release` ordering.
    unsafe {
        let slot = &(*REP_TABLE_SLOTS.0.get())[slot_idx];
        slot.by_id[..slot.count].to_vec()
    }
}

/// Looks up `rep` in the active Top-256 Representative table in `O(log2(256))` (`<= 8` comparisons).
#[inline]
pub fn lookup_compact_rep_id(rep: &[u8; 32]) -> Option<u8> {
    let slot_idx = ACTIVE_REP_SLOT.load(core::sync::atomic::Ordering::Acquire) & 1;
    // SAFETY: `slot_idx` was published with `Release` ordering.
    let slot = unsafe { &(*REP_TABLE_SLOTS.0.get())[slot_idx] };
    let keys = &slot.sorted_keys[..slot.sorted_len];
    let idx = keys.binary_search(rep).ok()?;
    Some(slot.sorted_ids[idx])
}

/// Decodes a 1-byte compact representative table index in `O(1)` (`~1 ns`).
#[inline]
pub fn get_compact_rep_bytes(id: u8) -> Option<[u8; 32]> {
    let slot_idx = ACTIVE_REP_SLOT.load(core::sync::atomic::Ordering::Acquire) & 1;
    // SAFETY: `slot_idx` was published with `Release` ordering.
    let slot = unsafe { &(*REP_TABLE_SLOTS.0.get())[slot_idx] };
    let i = id as usize;
    if i < slot.count {
        Some(slot.by_id[i])
    } else {
        None
    }
}

impl SavedBlock {
    pub fn deserialize_with_hash(
        stream: &mut dyn Stream,
        known_hash: Option<BlockHash>,
    ) -> anyhow::Result<Self> {
        let block = Block::deserialize_with_hash(stream, known_hash)?;
        let mut sideband = BlockSideband::from_stream(stream, block.block_type())?;
        match &block {
            Block::LegacySend(i) => {
                sideband.balance = i.balance();
                sideband.details = BlockDetails::new(Epoch::Epoch0, true, false, false)
            }
            Block::LegacyOpen(open) => {
                sideband.account = open.account();
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, true, false)
            }
            Block::LegacyReceive(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, true, false)
            }
            Block::LegacyChange(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, false, false)
            }
            Block::State(state) => {
                sideband.account = state.account();
                sideband.balance = state.balance();
            }
        }
        Ok(SavedBlock { block, sideband })
    }

    /// Extracts only the 32-byte `successor` from a `TABLE_BLOCKS` value slice (`compact` or `legacy`).
    #[inline]
    pub fn extract_compact_successor(val: &[u8]) -> Option<BlockHash> {
        if val.is_empty() {
            return None;
        }
        if (val[0] & COMPACT_BLOCK_TAG) != 0 {
            if (val[0] & COMPACT_HAS_SUCCESSOR) != 0 {
                if val.len() < 33 {
                    return None;
                }
                let mut succ = [0u8; 32];
                succ.copy_from_slice(&val[val.len() - 32..]);
                Some(BlockHash::from_bytes(succ))
            } else {
                Some(BlockHash::zero())
            }
        } else {
            if val.len() < 32 {
                return None;
            }
            let mut succ = [0u8; 32];
            succ.copy_from_slice(&val[val.len() - 32..]);
            Some(BlockHash::from_bytes(succ))
        }
    }

    /// Serializes `self` + `successor` into the self-contained bit-packed `TABLE_BLOCKS` format.
    /// Returns the number of bytes written into `out` (`<= 272`).
    #[inline]
    pub fn serialize_compact(&self, successor: &BlockHash, out: &mut [u8; 272]) -> usize {
        let has_succ = !successor.is_zero();
        match &self.block {
            Block::State(state) => {
                let acc = state.account();
                let prev = state.previous();
                let rep = state.representative();
                let link = state.link();
                let acc_bytes = acc.as_bytes();
                let prev_bytes = prev.as_bytes();
                let rep_bytes = rep.as_bytes();
                let link_bytes = link.as_bytes();

                let has_prev = !prev.is_zero();
                let link_is_zero = *link_bytes == [0u8; 32];
                // 3-bit `rl_code` (`(hdr0 >> 1) & 0x07`):
                //   0: rep = 32B inline, link = 32B inline
                //   1: rep = 32B inline, link = 0 (zero)
                //   2: rep = 32B inline, link = EPOCH_V1_LINK_BYTES
                //   3: rep = 32B inline, link = EPOCH_V2_LINK_BYTES
                //   4: rep = account (0B), link = 32B inline
                //   5: rep = account (0B), link = 0 (zero)
                //   6: rep = 1B table index, link = 32B inline
                //   7: rep = 1B table index, link = 0 (zero)
                let (rl_code, rep_id_opt): (u8, Option<u8>) = if rep_bytes == acc_bytes {
                    if link_is_zero {
                        (5, None)
                    } else {
                        (4, None)
                    }
                } else if *link_bytes == EPOCH_V1_LINK_BYTES {
                    (2, None)
                } else if *link_bytes == EPOCH_V2_LINK_BYTES {
                    (3, None)
                } else if let Some(rep_id) = lookup_compact_rep_id(rep_bytes) {
                    if link_is_zero {
                        (7, Some(rep_id))
                    } else {
                        (6, Some(rep_id))
                    }
                } else if link_is_zero {
                    (1, None)
                } else {
                    (0, None)
                };

                let bal = state.balance().number();
                let bal_buf: [u8; 16];
                let (bal_scaled, bal_nibble, bal_slice): (bool, u8, &[u8]) = if bal == 0 {
                    (false, 0, &[])
                } else if bal % RAW_BALANCE_SCALE == 0 {
                    let q = bal / RAW_BALANCE_SCALE;
                    let len = 16 - (q.leading_zeros() as usize / 8);
                    bal_buf = q.to_be_bytes();
                    (true, len as u8, &bal_buf[16 - len..16])
                } else {
                    let len = 16 - (bal.leading_zeros() as usize / 8);
                    bal_buf = bal.to_be_bytes();
                    if len == 16 {
                        (true, 0, &bal_buf[0..16])
                    } else {
                        (false, len as u8, &bal_buf[16 - len..16])
                    }
                };

                let height = self.sideband.height;
                let height_be = height.to_be_bytes();
                let (h_idx, height_slice): (u8, &[u8]) = if height == 1 {
                    (0, &[])
                } else if height <= 0xFF {
                    (1, &height_be[7..8])
                } else if height <= 0xFFFF {
                    (2, &height_be[6..8])
                } else if height <= 0xFFFF_FFFF {
                    (3, &height_be[4..8])
                } else {
                    (4, &height_be[0..8])
                };

                let ts = self.sideband.timestamp.as_u64();
                let ts_be = ts.to_be_bytes();
                let (ts_idx, ts_slice): (u8, &[u8]) = if ts == 0 {
                    (0, &[])
                } else if ts <= 0xFFFF_FFFF_FFFF {
                    (1, &ts_be[2..8])
                } else {
                    (2, &ts_be[0..8])
                };

                let details = &self.sideband.details;
                let epoch_combo = (details.epoch as u8) * 5 + (self.sideband.source_epoch as u8);

                out[0] = COMPACT_BLOCK_TAG
                    | (if has_succ { COMPACT_HAS_SUCCESSOR } else { 0 })
                    | (if has_prev { COMPACT_STATE_HAS_PREV } else { 0 })
                    | (rl_code << 1)
                    | (if bal_scaled { COMPACT_STATE_BAL_SCALED } else { 0 });
                out[1] = ((details.is_send as u8) << 7)
                    | ((details.is_receive as u8) << 6)
                    | ((details.is_epoch as u8) << 5)
                    | (epoch_combo & 0x1F);
                out[2] = (bal_nibble << 4) | (h_idx * 3 + ts_idx);

                let mut pos = 3usize;
                out[pos..pos + 32].copy_from_slice(acc_bytes);
                pos += 32;
                if has_prev {
                    out[pos..pos + 32].copy_from_slice(prev_bytes);
                    pos += 32;
                }
                if let Some(rep_id) = rep_id_opt {
                    out[pos] = rep_id;
                    pos += 1;
                } else if rl_code <= 3 {
                    out[pos..pos + 32].copy_from_slice(rep_bytes);
                    pos += 32;
                }
                if matches!(rl_code, 0 | 4 | 6) {
                    out[pos..pos + 32].copy_from_slice(link_bytes);
                    pos += 32;
                }
                if !bal_slice.is_empty() {
                    out[pos..pos + bal_slice.len()].copy_from_slice(bal_slice);
                    pos += bal_slice.len();
                }
                if !height_slice.is_empty() {
                    out[pos..pos + height_slice.len()].copy_from_slice(height_slice);
                    pos += height_slice.len();
                }
                if !ts_slice.is_empty() {
                    out[pos..pos + ts_slice.len()].copy_from_slice(ts_slice);
                    pos += ts_slice.len();
                }
                out[pos..pos + 64].copy_from_slice(state.signature().as_bytes());
                pos += 64;
                out[pos..pos + 8].copy_from_slice(&state.work().0.to_be_bytes());
                pos += 8;
                if has_succ {
                    out[pos..pos + 32].copy_from_slice(successor.as_bytes());
                    pos += 32;
                }
                pos
            }
            _ => {
                let mut stream = MemoryStream::new();
                self.serialize(&mut stream);
                let raw = stream.as_bytes();
                let btype = raw[0] & 0x1F;
                out[0] = COMPACT_BLOCK_TAG
                    | COMPACT_IS_LEGACY
                    | (if has_succ { COMPACT_HAS_SUCCESSOR } else { 0 })
                    | btype;
                let body_len = raw.len() - 1;
                out[1..1 + body_len].copy_from_slice(&raw[1..]);
                let mut pos = 1 + body_len;
                if has_succ {
                    out[pos..pos + 32].copy_from_slice(successor.as_bytes());
                    pos += 32;
                }
                pos
            }
        }
    }

    /// Deserializes a `SavedBlock` and its 32-byte `successor` from either the self-contained
    /// bit-packed format (`val[0] & 0x80 != 0`) or the legacy uncompressed format (`val[0] in 2..=6`).
    /// When `known_hash` is `Some(hash)`, skips redundant Blake2b hashing.
    pub fn deserialize_compact(
        val: &[u8],
        known_hash: Option<BlockHash>,
    ) -> anyhow::Result<(Self, BlockHash)> {
        if val.is_empty() {
            bail!("empty block value");
        }
        let hdr0 = val[0];
        if (hdr0 & COMPACT_BLOCK_TAG) == 0 {
            if val.len() < 32 {
                bail!("legacy block value too short");
            }
            let block_len = val.len() - 32;
            let mut stream = crate::utils::BufferReader::new(&val[..block_len]);
            let block = Self::deserialize_with_hash(&mut stream, known_hash)?;
            let mut succ = [0u8; 32];
            succ.copy_from_slice(&val[block_len..]);
            return Ok((block, BlockHash::from_bytes(succ)));
        }

        let has_succ = (hdr0 & COMPACT_HAS_SUCCESSOR) != 0;
        if has_succ && val.len() < 33 {
            bail!("compact block with successor too short");
        }
        let (body, successor) = if has_succ {
            let split = val.len() - 32;
            let mut succ = [0u8; 32];
            succ.copy_from_slice(&val[split..]);
            (&val[..split], BlockHash::from_bytes(succ))
        } else {
            (val, BlockHash::zero())
        };

        if (hdr0 & COMPACT_IS_LEGACY) != 0 {
            let mut raw = [0u8; 240];
            if body.len() > raw.len() {
                bail!("compact legacy block too large");
            }
            raw[0] = hdr0 & 0x1F;
            raw[1..body.len()].copy_from_slice(&body[1..]);
            let mut stream = crate::utils::BufferReader::new(&raw[..body.len()]);
            let block = Self::deserialize_with_hash(&mut stream, known_hash)?;
            return Ok((block, successor));
        }

        // Compact StateBlock decode
        if body.len() < 3 + 32 + 64 + 8 {
            bail!("compact state block too short");
        }
        let hdr1 = body[1];
        let hdr2 = body[2];
        let epoch_combo = hdr1 & 0x1F;
        if epoch_combo >= 25 {
            bail!("invalid compact epoch_combo");
        }
        let ht_nibble = hdr2 & 0x0F;
        if ht_nibble >= 15 {
            bail!("invalid compact ht_nibble");
        }
        let epoch = Epoch::from_u8(epoch_combo / 5).ok_or_else(|| anyhow!("invalid details epoch"))?;
        let source_epoch =
            Epoch::from_u8(epoch_combo % 5).ok_or_else(|| anyhow!("invalid source_epoch"))?;
        let details = BlockDetails::new(
            epoch,
            (hdr1 & 0x80) != 0,
            (hdr1 & 0x40) != 0,
            (hdr1 & 0x20) != 0,
        );
        let mut pos = 3usize;

        if pos + 32 > body.len() {
            bail!("truncated account");
        }
        let mut acc_bytes = [0u8; 32];
        acc_bytes.copy_from_slice(&body[pos..pos + 32]);
        let account = Account::from_bytes(acc_bytes);
        pos += 32;

        let previous = if (hdr0 & COMPACT_STATE_HAS_PREV) != 0 {
            if pos + 32 > body.len() {
                bail!("truncated previous");
            }
            let mut prev_bytes = [0u8; 32];
            prev_bytes.copy_from_slice(&body[pos..pos + 32]);
            pos += 32;
            BlockHash::from_bytes(prev_bytes)
        } else {
            BlockHash::zero()
        };

        let rl_code = (hdr0 >> 1) & 0x07;
        let representative = match rl_code {
            0..=3 => {
                if pos + 32 > body.len() {
                    bail!("truncated representative");
                }
                let mut rep_bytes = [0u8; 32];
                rep_bytes.copy_from_slice(&body[pos..pos + 32]);
                pos += 32;
                PublicKey::from_bytes(rep_bytes)
            }
            4..=5 => PublicKey::from_bytes(acc_bytes),
            6..=7 => {
                if pos + 1 > body.len() {
                    bail!("truncated representative table id");
                }
                let rep_id = body[pos];
                pos += 1;
                let rep_bytes = get_compact_rep_bytes(rep_id).ok_or_else(|| {
                    anyhow!("unknown compact representative table id {}", rep_id)
                })?;
                PublicKey::from_bytes(rep_bytes)
            }
            _ => unreachable!(),
        };

        let link = match rl_code {
            0 | 4 | 6 => {
                if pos + 32 > body.len() {
                    bail!("truncated link");
                }
                let mut l_bytes = [0u8; 32];
                l_bytes.copy_from_slice(&body[pos..pos + 32]);
                pos += 32;
                Link::from_bytes(l_bytes)
            }
            1 | 5 | 7 => Link::zero(),
            2 => Link::from_bytes(EPOCH_V1_LINK_BYTES),
            _ => Link::from_bytes(EPOCH_V2_LINK_BYTES),
        };

        let bal_scaled = (hdr0 & COMPACT_STATE_BAL_SCALED) != 0;
        let bal_nibble = (hdr2 >> 4) as usize;
        let balance = match (bal_scaled, bal_nibble) {
            (false, 0) => Amount::zero(),
            (false, len) => {
                if pos + len > body.len() {
                    bail!("truncated balance");
                }
                let mut be = [0u8; 16];
                be[16 - len..16].copy_from_slice(&body[pos..pos + len]);
                pos += len;
                Amount::raw(u128::from_be_bytes(be))
            }
            (true, 0) => {
                if pos + 16 > body.len() {
                    bail!("truncated 16B balance");
                }
                let mut be = [0u8; 16];
                be.copy_from_slice(&body[pos..pos + 16]);
                pos += 16;
                Amount::raw(u128::from_be_bytes(be))
            }
            (true, len) => {
                if pos + len > body.len() {
                    bail!("truncated scaled balance");
                }
                let mut be = [0u8; 16];
                be[16 - len..16].copy_from_slice(&body[pos..pos + len]);
                pos += len;
                Amount::raw(u128::from_be_bytes(be).wrapping_mul(RAW_BALANCE_SCALE))
            }
        };

        let h_idx = ht_nibble / 3;
        let ts_idx = ht_nibble % 3;
        let height = match h_idx {
            0 => 1u64,
            1 => {
                if pos + 1 > body.len() {
                    bail!("truncated 1B height");
                }
                let h = body[pos] as u64;
                pos += 1;
                h
            }
            2 => {
                if pos + 2 > body.len() {
                    bail!("truncated 2B height");
                }
                let h = u16::from_be_bytes([body[pos], body[pos + 1]]) as u64;
                pos += 2;
                h
            }
            3 => {
                if pos + 4 > body.len() {
                    bail!("truncated 4B height");
                }
                let h = u32::from_be_bytes([
                    body[pos],
                    body[pos + 1],
                    body[pos + 2],
                    body[pos + 3],
                ]) as u64;
                pos += 4;
                h
            }
            _ => {
                if pos + 8 > body.len() {
                    bail!("truncated 8B height");
                }
                let mut be = [0u8; 8];
                be.copy_from_slice(&body[pos..pos + 8]);
                pos += 8;
                u64::from_be_bytes(be)
            }
        };

        let timestamp = match ts_idx {
            0 => UnixMillisTimestamp::from(0),
            1 => {
                if pos + 6 > body.len() {
                    bail!("truncated 6B timestamp");
                }
                let mut be = [0u8; 8];
                be[2..8].copy_from_slice(&body[pos..pos + 6]);
                pos += 6;
                UnixMillisTimestamp::from(u64::from_be_bytes(be))
            }
            _ => {
                if pos + 8 > body.len() {
                    bail!("truncated 8B timestamp");
                }
                let mut be = [0u8; 8];
                be.copy_from_slice(&body[pos..pos + 8]);
                pos += 8;
                UnixMillisTimestamp::from(u64::from_be_bytes(be))
            }
        };

        if pos + 72 != body.len() {
            bail!("invalid compact state block trailing length");
        }
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(&body[pos..pos + 64]);
        let signature = Signature::from_bytes(sig_bytes);
        pos += 64;

        let mut work_bytes = [0u8; 8];
        work_bytes.copy_from_slice(&body[pos..pos + 8]);
        let work = WorkNonce::from(u64::from_be_bytes(work_bytes));

        let state_block = StateBlock::from_parts_with_hash(
            account,
            previous,
            representative,
            balance,
            link,
            signature,
            work,
            known_hash,
        );
        let sideband = BlockSideband {
            height,
            timestamp,
            account,
            balance,
            details,
            source_epoch,
        };
        Ok((
            SavedBlock::new(Block::State(state_block), sideband),
            successor,
        ))
    }
}

impl Deserialize for SavedBlock {
    type Target = Self;
    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self> {
        Self::deserialize_with_hash(stream, None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaybeSavedBlock {
    Saved(SavedBlock),
    Unsaved(Block),
}

impl From<MaybeSavedBlock> for Block {
    fn from(value: MaybeSavedBlock) -> Self {
        match value {
            MaybeSavedBlock::Saved(b) => b.into(),
            MaybeSavedBlock::Unsaved(b) => b,
        }
    }
}

impl Deref for MaybeSavedBlock {
    type Target = Block;

    fn deref(&self) -> &Self::Target {
        match self {
            MaybeSavedBlock::Saved(b) => b,
            MaybeSavedBlock::Unsaved(b) => b,
        }
    }
}

static DEV_PRIVATE_KEY_DATA: &str =
    "34F0A37AAD20F4A260F0A5B3CB3D7FB50673212263E58A380BC10474BB039CE4";
pub static DEV_PUBLIC_KEY_DATA: &str =
    "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0"; // xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo
pub static DEV_GENESIS_KEY: Lazy<PrivateKey> =
    Lazy::new(|| PrivateKey::from_hex_str(DEV_PRIVATE_KEY_DATA).unwrap());

static DEV_GENESIS_DATA: &str = r###"{
	"type": "open",
	"source": "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0",
	"representative": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"account": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"work": "7b42a00ee91d5810",
	"signature": "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02"
    }"###;

pub static DEV_GENESIS_BLOCK: Lazy<Block> =
    Lazy::new(|| serde_json::from_str(DEV_GENESIS_DATA).unwrap());

#[derive(Default, PartialEq, Eq, Debug, Clone)]
pub struct DependentBlocks {
    dependents: [BlockHash; 2],
}

impl DependentBlocks {
    pub fn new(previous: BlockHash, link: BlockHash) -> Self {
        Self {
            dependents: [previous, link],
        }
    }

    pub fn none() -> Self {
        Self::new(BlockHash::zero(), BlockHash::zero())
    }

    pub fn previous(&self) -> Option<BlockHash> {
        self.get_index(0)
    }

    pub fn link(&self) -> Option<BlockHash> {
        self.get_index(1)
    }

    fn get_index(&self, index: usize) -> Option<BlockHash> {
        if self.dependents[index].is_zero() {
            None
        } else {
            Some(self.dependents[index])
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &BlockHash> {
        self.dependents
            .iter()
            .flat_map(|i| if i.is_zero() { None } else { Some(i) })
    }
}

pub struct DetailedBlock {
    pub block: SavedBlock,
    pub amount: Option<Amount>,
    pub confirmed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_legacy_open() {
        let block = TestBlockBuilder::legacy_open().build_saved();
        assert_serializable(block.into());
    }

    #[test]
    fn serialize_legacy_receive() {
        let block = TestBlockBuilder::legacy_receive().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_legacy_send() {
        let block = TestBlockBuilder::legacy_send().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_legacy_change() {
        let block = TestBlockBuilder::legacy_change().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_state() {
        let block = TestBlockBuilder::state().build();
        assert_serializable(block);
    }

    fn assert_serializable(block: Block) {
        let mut buffer = MemoryStream::new();
        block.serialize(&mut buffer);
        let deserialized = Block::deserialize(&mut buffer).unwrap();
        assert_eq!(deserialized, block);
    }

    #[test]
    fn compact_saved_block_roundtrip_all_variants() {
        let key = PrivateKey::from(77);
        let acc = key.account();
        let rep_same: PublicKey = acc.into();
        let rep_diff = PrivateKey::from(88).public_key();
        let rep_table_0 = PrivateKey::from(1001).public_key();
        let rep_table_255 = PrivateKey::from(1256).public_key();
        let mut table_reps = [[0u8; 32]; 256];
        for i in 0..256 {
            table_reps[i] = *PrivateKey::from((1001 + i) as u64).public_key().as_bytes();
        }
        set_compact_representatives(&table_reps);
        assert_eq!(lookup_compact_rep_id(rep_table_0.as_bytes()), Some(0));
        assert_eq!(lookup_compact_rep_id(rep_table_255.as_bytes()), Some(255));
        assert_eq!(lookup_compact_rep_id(rep_diff.as_bytes()), None);

        let balances = [
            Amount::zero(),
            Amount::raw(1),
            Amount::raw(42),
            Amount::nano(1),
            Amount::nano(133_248_297),
            Amount::raw(RAW_BALANCE_SCALE * 123_456),
            Amount::raw(u128::MAX),
        ];
        let heights = [0u64, 1, 2, 255, 256, 65535, 65536, 0xFFFF_FFFF, 0x1_0000_0000];
        let timestamps = [0u64, 100, 222_222, 1_700_000_000_000, u64::MAX];
        let links = [
            Link::zero(),
            Link::from_bytes(EPOCH_V1_LINK_BYTES),
            Link::from_bytes(EPOCH_V2_LINK_BYTES),
            Link::from(999),
        ];
        let successors = [BlockHash::zero(), BlockHash::from(5555)];

        for &bal in &balances {
            for &h in &heights {
                for &ts in &timestamps {
                    for &link in &links {
                        for &rep in &[rep_same, rep_diff, rep_table_0, rep_table_255] {
                            for &prev in &[BlockHash::zero(), BlockHash::from(1234)] {
                                for &succ in &successors {
                                    let block: Block = StateBlockArgs {
                                        key: &key,
                                        previous: prev,
                                        representative: rep,
                                        balance: bal,
                                        link,
                                        work: 0xDEAD_BEEF_CAFE_BABEu64.into(),
                                    }
                                    .into();
                                    let sideband = BlockSideband {
                                        height: h,
                                        timestamp: ts.into(),
                                        account: acc,
                                        balance: bal,
                                        details: BlockDetails::new(
                                            Epoch::Epoch2,
                                            true,
                                            false,
                                            false,
                                        ),
                                        source_epoch: Epoch::Epoch1,
                                    };
                                    let saved = SavedBlock::new(block, sideband);
                                    let mut buf = [0u8; 272];
                                    let n = saved.serialize_compact(&succ, &mut buf);
                                    assert!(n <= 267, "compact len {} > 267", n);
                                    assert_eq!(
                                        SavedBlock::extract_compact_successor(&buf[..n]),
                                        Some(succ)
                                    );
                                    let (dec_none, dec_succ1) =
                                        SavedBlock::deserialize_compact(&buf[..n], None).unwrap();
                                    let (dec_some, dec_succ2) = SavedBlock::deserialize_compact(
                                        &buf[..n],
                                        Some(saved.hash()),
                                    )
                                    .unwrap();
                                    assert_eq!(dec_succ1, succ);
                                    assert_eq!(dec_succ2, succ);
                                    assert_eq!(dec_none, saved);
                                    assert_eq!(dec_some, saved);
                                    assert_eq!(dec_some.hash(), saved.hash());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also test legacy blocks (Open, Send, Receive, Change)
        let recv_blk = TestBlockBuilder::legacy_receive().build();
        let chg_blk = TestBlockBuilder::legacy_change().build();
        let legacies = [
            TestBlockBuilder::legacy_open().build_saved(),
            TestBlockBuilder::legacy_send().build_saved(),
            SavedBlock::new(
                recv_blk,
                BlockSideband {
                    details: BlockDetails::new(Epoch::Epoch0, false, true, false),
                    source_epoch: Epoch::Epoch0,
                    ..BlockSideband::new_test_instance()
                },
            ),
            SavedBlock::new(
                chg_blk,
                BlockSideband {
                    details: BlockDetails::new(Epoch::Epoch0, false, false, false),
                    source_epoch: Epoch::Epoch0,
                    ..BlockSideband::new_test_instance()
                },
            ),
        ];
        for saved in legacies {
            for &succ in &successors {
                let mut buf = [0u8; 272];
                let n = saved.serialize_compact(&succ, &mut buf);
                assert_eq!(
                    SavedBlock::extract_compact_successor(&buf[..n]),
                    Some(succ)
                );
                let (dec, dec_succ) =
                    SavedBlock::deserialize_compact(&buf[..n], Some(saved.hash())).unwrap();
                assert_eq!(dec_succ, succ);
                assert_eq!(dec, saved);
                assert_eq!(dec.hash(), saved.hash());
            }
        }
    }
}
