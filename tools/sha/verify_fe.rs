
use std::convert::TryInto;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldElement(pub [u64; 4]);

impl FieldElement {
    pub const ZERO: Self = FieldElement([0, 0, 0, 0]);
    pub const ONE: Self = FieldElement([1, 0, 0, 0]);

    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            limbs[i] = u64::from_le_bytes(bytes[i * 8..(i + 1) * 8].try_into().unwrap());
        }
        limbs[3] &= 0x7FFFFFFFFFFFFFFF;
        FieldElement(limbs)
    }

    pub fn reduce(&self) -> Self {
        let mut res = *self;
        let high_bits = (res.0[3] >> 63) as u128;
        if high_bits > 0 {
            res.0[3] &= 0x7FFFFFFFFFFFFFFF;
            res = res.add_u64((high_bits * 19) as u64);
        }
        let (r0, c0) = res.0[0].overflowing_add(19);
        let (r1, c1) = res.0[1].overflowing_add(c0 as u64);
        let (r2, c2) = res.0[2].overflowing_add(c1 as u64);
        let (r3, _c3) = res.0[3].overflowing_add(c2 as u64);
        if (r3 >> 63) != 0 {
            FieldElement([r0, r1, r2, r3 & 0x7FFFFFFFFFFFFFFF])
        } else {
            res
        }
    }

    fn add_u64(&self, val: u64) -> Self {
        let (r0, c) = self.0[0].overflowing_add(val);
        let (r1, c) = self.0[1].overflowing_add(c as u64);
        let (r2, c) = self.0[2].overflowing_add(c as u64);
        let (r3, c) = self.0[3].overflowing_add(c as u64);
        let mut res = FieldElement([r0, r1, r2, r3]);
        if c {
            res = res.add_u64(38);
        }
        res
    }
    
    fn sub_u64(&self, val: u64) -> Self {
        let (r0, b) = self.0[0].overflowing_sub(val);
        let (r1, b) = self.0[1].overflowing_sub(b as u64);
        let (r2, b) = self.0[2].overflowing_sub(b as u64);
        let (r3, b) = self.0[3].overflowing_sub(b as u64);
        let mut res = FieldElement([r0, r1, r2, r3]);
        if b {
            res = res.sub_u64(38);
        }
        res
    }

    pub fn sub(&self, other: &Self) -> Self {
        let (r0, b) = self.0[0].overflowing_sub(other.0[0]);
        let (r1, b1) = self.0[1].overflowing_sub(other.0[1]);
        let (r1, b2) = r1.overflowing_sub(b as u64);
        let b = b1 | b2;
        let (r2, b1) = self.0[2].overflowing_sub(other.0[2]);
        let (r2, b2) = r2.overflowing_sub(b as u64);
        let b = b1 | b2;
        let (r3, b1) = self.0[3].overflowing_sub(other.0[3]);
        let (r3, b2) = r3.overflowing_sub(b as u64);
        let b = b1 | b2;
        
        let mut res = FieldElement([r0, r1, r2, r3]);
        if b {
            // BUGGY CODE HAD: res = res.add_u64(38);
            res = res.sub_u64(38);
        }
        res
    }
}

fn main() {
    let zero = FieldElement::ZERO;
    let one = FieldElement::ONE;
    let res = zero.sub(&one).reduce();
    println!("0 - 1 reduced: {:?}", res);
    
    // Expected: 2^255 - 20
    // Limb 0: 0xFFFFFFFFFFFFFFEC (18446744073709551596)
    // Limb 1: 0xFFFFFFFFFFFFFFFF
    // Limb 2: 0xFFFFFFFFFFFFFFFF
    // Limb 3: 0x7FFFFFFFFFFFFFFF (9223372036854775807)
}
