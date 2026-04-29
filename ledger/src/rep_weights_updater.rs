use alloc::sync::Arc;
use hashbrown::HashMap;
use spin::RwLock;

use rsnano_core::{Amount, PublicKey};
use rsnano_ledger_store::BtreeRepWeightStore;

use crate::{RepWeightCache, RepWeights};

/// Updates the representative weights in the ledger and in the in-memory cache
pub struct RepWeightsUpdater {
    weight_cache: Arc<RwLock<RepWeights>>,
    store: Arc<BtreeRepWeightStore>,
    min_weight: Amount,
}

impl RepWeightsUpdater {
    pub fn new(store: Arc<BtreeRepWeightStore>, min_weight: Amount, cache: &RepWeightCache) -> Self {
        RepWeightsUpdater {
            weight_cache: cache.inner(),
            store,
            min_weight,
        }
    }

    /// Only use this method when loading rep weights from the database table
    pub fn copy_from(&self, other: &HashMap<PublicKey, Amount>) {
        let mut guard_this = self.weight_cache.write();
        for (account, amount) in other {
            let prev_amount = self.get(&guard_this, account);
            self.put_cache(&mut guard_this, *account, prev_amount.wrapping_add(*amount));
        }
    }

    fn get(&self, weights: &HashMap<PublicKey, Amount>, account: &PublicKey) -> Amount {
        weights.get(account).cloned().unwrap_or_default()
    }

    pub fn representation_add(
        &self,
        _tx: &mut (),
        representative: PublicKey,
        amount: Amount,
    ) {
        let previous_weight = self.store.get(&(), &representative).unwrap_or_default();
        let new_weight = previous_weight.wrapping_add(amount);
        self.put_store(&(), representative, previous_weight, new_weight);
        let mut guard = self.weight_cache.write();
        self.put_cache(&mut guard, representative, new_weight);
    }

    fn put_cache(
        &self,
        weights: &mut HashMap<PublicKey, Amount>,
        representative: PublicKey,
        new_weight: Amount,
    ) {
        if new_weight < self.min_weight || new_weight.is_zero() {
            weights.remove(&representative);
        } else {
            weights.insert(representative, new_weight);
        }
    }

    fn put_store(
        &self,
        _tx: &(),
        representative: PublicKey,
        previous_weight: Amount,
        new_weight: Amount,
    ) {
        if new_weight.is_zero() {
            if !previous_weight.is_zero() {
                // Btree doesn't support delete yet, but we can put zero
                self.store.put(representative, Amount::zero());
            }
        } else {
            self.store.put(representative, new_weight);
        }
    }

    /// Only use this method when loading rep weights from the database table!
    pub fn representation_put(&self, representative: PublicKey, weight: Amount) {
        let mut guard = self.weight_cache.write();
        self.put_cache(&mut guard, representative, weight);
    }

    pub fn representation_add_dual(
        &self,
        _tx: &mut (),
        rep_1: PublicKey,
        amount_1: Amount,
        rep_2: PublicKey,
        amount_2: Amount,
    ) {
        if rep_1 != rep_2 {
            let previous_weight_1 = self.store.get(&(), &rep_1).unwrap_or_default();
            let previous_weight_2 = self.store.get(&(), &rep_2).unwrap_or_default();
            let new_weight_1 = previous_weight_1.wrapping_add(amount_1);
            let new_weight_2 = previous_weight_2.wrapping_add(amount_2);
            self.put_store(&(), rep_1, previous_weight_1, new_weight_1);
            self.put_store(&(), rep_2, previous_weight_2, new_weight_2);
            let mut guard = self.weight_cache.write();
            self.put_cache(&mut guard, rep_1, new_weight_1);
            self.put_cache(&mut guard, rep_2, new_weight_2);
        } else {
            self.representation_add(&mut (), rep_1, amount_1.wrapping_add(amount_2));
        }
    }
}
