use spin::Lazy;

use rsnano_nullable_random::NullableRng;
use rand::Rng;

pub struct HardenedConstants {
    pub random_128: u128,
}

impl HardenedConstants {
    pub fn get() -> &'static HardenedConstants {
        &INSTANCE
    }
}

static INSTANCE: Lazy<HardenedConstants> = Lazy::new(|| {
    let mut rng = NullableRng::rng();
    HardenedConstants {
        random_128: u128::from_ne_bytes(rng.random::<[u8; 16]>()),
    }
});
