use rsnano_core::{PublicKey, Amount, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use core::sync::atomic::Ordering;

const HOT_REP_SLOTS: usize = 16; // 64 shards * 16 slots = 1,024 hot representatives (in .bss)

#[derive(Clone, Copy)]
struct HotRepEntry {
    gen: u32,
    present: u32,
    rep: PublicKey,
    amount: Amount,
}

impl HotRepEntry {
    const EMPTY: Self = Self {
        gen: 0,
        present: 0,
        rep: PublicKey::zero(),
        amount: Amount::zero(),
    };
}

static HOT_REP_WEIGHTS: [spin::Mutex<[HotRepEntry; HOT_REP_SLOTS]>; SHARD_COUNT] =
    [const { spin::Mutex::new([HotRepEntry::EMPTY; HOT_REP_SLOTS]) }; SHARD_COUNT];

#[inline]
fn hot_rep_slot(pub_key: &PublicKey) -> (usize, usize) {
    let b = pub_key.as_bytes();
    let shard = (b[0] >> 2) as usize;
    let w = u32::from_ne_bytes([b[1], b[2], b[3], b[4]]);
    let slot = (w.wrapping_mul(0x9E37_79B9) >> 28) as usize & (HOT_REP_SLOTS - 1);
    (shard, slot)
}

#[inline]
fn hot_rep_get(pub_key: &PublicKey) -> Option<Option<Amount>> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_rep_slot(pub_key);
    let guard = HOT_REP_WEIGHTS[shard].lock();
    let entry = &guard[slot];
    if entry.gen == gen && entry.rep == *pub_key {
        return Some(if entry.present != 0 {
            Some(entry.amount)
        } else {
            None
        });
    }
    None
}

#[inline]
fn hot_rep_put(pub_key: PublicKey, amount: Option<Amount>) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_rep_slot(&pub_key);
    let (present, val) = match amount {
        Some(a) => (1u32, a),
        None => (0u32, Amount::zero()),
    };
    HOT_REP_WEIGHTS[shard].lock()[slot] = HotRepEntry {
        gen,
        present,
        rep: pub_key,
        amount: val,
    };
}

pub struct BtreeRepWeightStore {}

impl BtreeRepWeightStore {
    pub const TABLE_ID: u8 = 12;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, pub_key: &PublicKey) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(pub_key.as_bytes());
        key
    }

    pub fn put(&self, pub_key: PublicKey, amount: Amount) {
        let key = self.make_key(&pub_key);
        let mut buffer = [0u8; 32];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        amount.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buffer[0..len]);
            drop(lock);
            if !cfg!(test) {
                hot_rep_put(pub_key, Some(amount));
            }
        }
    }

    pub fn get(&self, _tx: &dyn core::any::Any, pub_key: &PublicKey) -> Option<Amount> {
        if !cfg!(test) {
            if let Some(cached) = hot_rep_get(pub_key) {
                return cached;
            }
        }
        let key = self.make_key(pub_key);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            let found = db
                .get_with(&key, |value| {
                    let mut reader = BufferReader::new(value);
                    Amount::deserialize(&mut reader).ok()
                })
                .flatten();
            drop(lock);
            if !cfg!(test) {
                hot_rep_put(*pub_key, found);
            }
            return found;
        }
        None
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(PublicKey, Amount),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        let mut count = 0usize;
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let rep = PublicKey::from_bytes(key[1..33].try_into().unwrap());
                    let mut reader = BufferReader::new(value);
                    Ok(Amount::deserialize(&mut reader)
                        .ok()
                        .map(|amount| (rep, amount)))
                }) {
                    match step {
                        Ok(Some((rep, amount))) => {
                            f(rep, amount);
                            count += 1;
                            if count % 1000 == 0 {
                                kernel_common::watchdog::kick();
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }
}
