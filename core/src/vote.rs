use super::{
    utils::{BufferWriter, Deserialize, FixedSizeSerialize, Stream},
    Account, Blake2HashBuilder, BlockHash, PrivateKey, Signature,
};
use crate::{
    utils::{Serialize, UnixMillisTimestamp},
    PublicKey, VoteTimestamp,
};
use anyhow::Result;
use core::time::Duration;
use alloc::vec::Vec;
use alloc::vec;

#[derive(Copy, Clone, PartialEq, Eq, Debug, EnumCount, EnumIter)]
pub enum VoteSource {
    Live,
    Rebroadcast,
    Cache,
}

impl VoteSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteSource::Live => "live",
            VoteSource::Rebroadcast => "rebroadcast",
            VoteSource::Cache => "cache",
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoteError {
    /// Vote is not signed correctly
    Invalid,

    /// Vote does not have the highest timestamp, it's a replay
    Replay,

    /// Vote has the highest timestamp
    Vote,

    /// Vote is late, the election is already confirmed and present in the recently confirmed set
    Late,

    /// Unknown if replay or vote
    Indeterminate,

    /// Vote is valid, but got ingored (e.g. due to cooldown)
    Ignored,
}

impl VoteError {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteError::Vote => "vote",
            VoteError::Late => "late",
            VoteError::Replay => "replay",
            VoteError::Indeterminate => "indeterminate",
            VoteError::Ignored => "ignored",
            VoteError::Invalid => "invalid",
        }
    }
}

const VOTE_SIG_UNCHECKED: u8 = 0;
const VOTE_SIG_VALID: u8 = 1;
const VOTE_SIG_INVALID: u8 = 2;

#[derive(Debug)]
pub struct Vote {
    timestamp: VoteTimestamp,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of timestamp + block hashes
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,

    sig_state: core::sync::atomic::AtomicU8,
}

impl Clone for Vote {
    fn clone(&self) -> Self {
        Self {
            timestamp: self.timestamp,
            voter: self.voter,
            signature: self.signature.clone(),
            hashes: self.hashes.clone(),
            sig_state: core::sync::atomic::AtomicU8::new(
                self.sig_state.load(core::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

static HASH_PREFIX: &str = "vote ";

impl Vote {
    pub const MAX_HASHES: usize = 255;
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            voter: PublicKey::zero(),
            signature: Signature::new(),
            hashes: Vec::new(),
            sig_state: core::sync::atomic::AtomicU8::new(VOTE_SIG_UNCHECKED),
        }
    }

    pub fn new_final(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
    }

    pub fn new(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
            sig_state: core::sync::atomic::AtomicU8::new(VOTE_SIG_VALID),
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    pub fn new_test_instance() -> Self {
        Self::build_test_instance().finish()
    }

    pub fn build_test_instance() -> TestVoteBuilder {
        TestVoteBuilder::new()
    }

    pub const DURATION_MAX: u8 = 0x0F;
    pub const TIMESTAMP_MAX: UnixMillisTimestamp = UnixMillisTimestamp::new(0xFFFF_FFFF_FFFF_FFF0);
    pub const TIMESTAMP_MIN: UnixMillisTimestamp = UnixMillisTimestamp::new(0x0000_0000_0000_0010);

    pub fn timestamp(&self) -> UnixMillisTimestamp {
        self.timestamp.unix_timestamp()
    }

    pub fn is_final(&self) -> bool {
        self.timestamp.is_final()
    }

    pub fn duration_bits(&self) -> u8 {
        self.timestamp.duration_bits()
    }

    pub fn duration(&self) -> Duration {
        self.timestamp.duration()
    }

    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);

        for hash in &self.hashes {
            builder = builder.update(hash.as_bytes())
        }

        builder.update(self.timestamp.to_ne_bytes()).build()
    }

    pub fn deserialize(&mut self, stream: &mut impl Stream) -> Result<()> {
        self.voter = PublicKey::deserialize(stream)?;
        self.signature = Signature::deserialize(stream)?;
        let mut buffer = [0; 8];
        stream.read_bytes(&mut buffer, 8)?;
        self.timestamp = VoteTimestamp::from_le_bytes(buffer);
        self.hashes = Vec::new();
        while stream.in_avail()? > 0 && self.hashes.len() < Self::MAX_HASHES {
            self.hashes.push(BlockHash::deserialize(stream)?);
        }
        self.sig_state
            .store(VOTE_SIG_UNCHECKED, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    #[inline]
    pub fn is_signature_checked(&self) -> bool {
        self.sig_state.load(core::sync::atomic::Ordering::Acquire) != VOTE_SIG_UNCHECKED
    }

    #[inline]
    pub fn set_signature_result(&self, valid: bool) {
        self.sig_state.store(
            if valid {
                VOTE_SIG_VALID
            } else {
                VOTE_SIG_INVALID
            },
            core::sync::atomic::Ordering::Release,
        );
    }

    pub fn validate(&self) -> Result<()> {
        match self.sig_state.load(core::sync::atomic::Ordering::Acquire) {
            VOTE_SIG_VALID => return Ok(()),
            VOTE_SIG_INVALID => return Err(anyhow!("invalid vote signature")),
            _ => {}
        }
        match self.voter.verify(self.hash().as_bytes(), &self.signature) {
            Ok(()) => {
                self.sig_state
                    .store(VOTE_SIG_VALID, core::sync::atomic::Ordering::Release);
                Ok(())
            }
            Err(e) => {
                self.sig_state
                    .store(VOTE_SIG_INVALID, core::sync::atomic::Ordering::Release);
                Err(e)
            }
        }
    }

    /// Batch-hashes and Ed25519-verifies a slice of `Vote` references using 16-way `VLEN=1024`
    /// (`verify_batch_16` / `hash_batch_16`) on SpacemiT K3 A100 harts and 4-way (`verify_batch_4`)
    /// on X100 / host targets, caching the result atomically in `sig_state`.
    pub fn verify_batch(votes: &[&Self]) {
        let mut unchecked: Vec<&Self> = votes
            .iter()
            .copied()
            .filter(|v| !v.is_signature_checked())
            .collect();
        if unchecked.is_empty() {
            return;
        }

        let mut offset = 0usize;
        let vlen1024 = tools::blake2b::has_vlen_1024();

        while offset < unchecked.len() {
            let rem = unchecked.len() - offset;
            if rem >= 16 || (vlen1024 && rem > 4) {
                let take = rem.min(16);
                let chunk = &mut unchecked[offset..offset + take];

                // Compute 32-byte vote digests (using 16-way lockstep Blake2b when all 16 votes
                // carry the same small number of block hashes <= 3, i.e., <= 109 bytes).
                let hlen0 = chunk[0].hashes.len();
                let digests: [BlockHash; 16] =
                    if take == 16 && hlen0 <= 3 && chunk.iter().all(|v| v.hashes.len() == hlen0) {
                        let msg_len = 5 + hlen0 * 32 + 8;
                        let mut bufs = [[0u8; 5 + 3 * 32 + 8]; 16];
                        for i in 0..16 {
                            bufs[i][0..5].copy_from_slice(HASH_PREFIX.as_bytes());
                            for (hi, bh) in chunk[i].hashes.iter().enumerate() {
                                let off = 5 + hi * 32;
                                bufs[i][off..off + 32].copy_from_slice(bh.as_bytes());
                            }
                            let ts_off = 5 + hlen0 * 32;
                            bufs[i][ts_off..ts_off + 8]
                                .copy_from_slice(&chunk[i].timestamp.to_ne_bytes());
                        }
                        let refs: [&[u8]; 16] = core::array::from_fn(|i| &bufs[i][..msg_len]);
                        let raw = tools::blake2b::hash_batch_16(refs);
                        core::array::from_fn(|i| BlockHash::from_bytes(raw[i]))
                    } else {
                        let mut d = [BlockHash::zero(); 16];
                        for i in 0..take {
                            d[i] = chunk[i].hash();
                        }
                        for i in take..16 {
                            d[i] = d[0];
                        }
                        d
                    };

                let keys: [&[u8; 32]; 16] = core::array::from_fn(|i| {
                    let src = if i < take { i } else { 0 };
                    chunk[src].voter.as_bytes()
                });
                let msgs: [&[u8]; 16] = core::array::from_fn(|i| digests[i].as_bytes() as &[u8]);
                let sigs: [&[u8; 64]; 16] = core::array::from_fn(|i| {
                    let src = if i < take { i } else { 0 };
                    chunk[src].signature.as_bytes()
                });

                let results = tools::ed25519::verify_batch_16(keys, msgs, sigs);
                for i in 0..take {
                    chunk[i].set_signature_result(results[i]);
                }
                offset += take;
            } else if rem >= 4 {
                let chunk = &unchecked[offset..offset + 4];
                let d0 = chunk[0].hash();
                let d1 = chunk[1].hash();
                let d2 = chunk[2].hash();
                let d3 = chunk[3].hash();
                let keys = [
                    chunk[0].voter.as_bytes(),
                    chunk[1].voter.as_bytes(),
                    chunk[2].voter.as_bytes(),
                    chunk[3].voter.as_bytes(),
                ];
                let msgs = [
                    d0.as_bytes() as &[u8],
                    d1.as_bytes() as &[u8],
                    d2.as_bytes() as &[u8],
                    d3.as_bytes() as &[u8],
                ];
                let sigs = [
                    chunk[0].signature.as_bytes(),
                    chunk[1].signature.as_bytes(),
                    chunk[2].signature.as_bytes(),
                    chunk[3].signature.as_bytes(),
                ];
                let results = tools::ed25519::verify_batch_4(keys, msgs, sigs);
                for i in 0..4 {
                    chunk[i].set_signature_result(results[i]);
                }
                offset += 4;
            } else {
                let _ = unchecked[offset].validate();
                offset += 1;
            }
        }
    }

    pub fn serialized_size(count: usize) -> usize {
        Account::serialized_size()
        + Signature::serialized_size()
        + core::mem::size_of::<u64>() // timestamp
        + (BlockHash::serialized_size() * count)
    }
}

impl Serialize for Vote {
    fn serialize(&self, writer: &mut dyn BufferWriter) {
        self.voter.serialize(writer);
        self.signature.serialize(writer);
        writer.write_bytes_safe(&self.timestamp.to_le_bytes());
        for hash in &self.hashes {
            hash.serialize(writer);
        }
    }
}

impl PartialEq for Vote {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp
            && self.voter == other.voter
            && self.signature == other.signature
            && self.hashes == other.hashes
    }
}

impl Eq for Vote {}

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    hashes: Vec<BlockHash>,
}

impl TestVoteBuilder {
    fn new() -> Self {
        Self {
            key: PrivateKey::from(42),
            timestamp: UnixMillisTimestamp::new(1),
            duration: 2,
            is_final: false,
            hashes: vec![BlockHash::from(5)],
        }
    }

    pub fn voter_key(mut self, key: impl Into<PrivateKey>) -> Self {
        self.key = key.into();
        self
    }

    pub fn timestamp(mut self, ts: UnixMillisTimestamp) -> Self {
        self.timestamp = ts;
        self
    }

    pub fn final_vote(mut self) -> Self {
        self.is_final = true;
        self
    }

    pub fn blocks(mut self, hashes: impl IntoIterator<Item = BlockHash>) -> Self {
        self.hashes = hashes.into_iter().collect();
        self
    }

    pub fn finish(self) -> Vote {
        if self.is_final {
            Vote::new_final(&self.key, self.hashes)
        } else {
            Vote::new(&self.key, self.timestamp, self.duration, self.hashes)
        }
    }
}
