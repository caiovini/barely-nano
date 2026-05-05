//! Curve25519 field arithmetic optimized for RISC-V.
//! Prime p = 2^255 - 19.

use core::convert::TryInto;

/// A field element in Curve25519.
/// Represented as 4 x u64 in little-endian.
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
        // Clear bit 255 as per Curve25519 spec
        limbs[3] &= 0x7FFFFFFFFFFFFFFF;
        FieldElement(limbs)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let reduced = self.reduce();
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&reduced.0[i].to_le_bytes());
        }
        bytes
    }

    /// Fully reduce the element to the range [0, p-1]
    pub fn reduce(&self) -> Self {
        let mut res = *self;
        
        // First pass: handle bit 255+
        // 2^255 \equiv 19 (mod p)
        // We might have values up to ~2^256 due to carries.
        let high_bits = (res.0[3] >> 63) as u128; // This could include carry if we were wider, but we're 256 bits.
        // Wait, if we had a carry from add/mul, it should have been handled by add_u64(38).
        // So here res.0[3] >> 63 is just the 255th bit.
        if high_bits > 0 {
            res.0[3] &= 0x7FFFFFFFFFFFFFFF;
            res = res.add_u64((high_bits * 19) as u64);
        }
        
        // Now res is in range [0, 2^255 + some_small_value]
        // We need to handle the case where it's still >= p (2^255 - 19)
        // A simple way is to check if res + 19 >= 2^255
        let (r0, c0) = res.0[0].overflowing_add(19);
        let (r1, c1) = res.0[1].overflowing_add(c0 as u64);
        let (r2, c2) = res.0[2].overflowing_add(c1 as u64);
        let (r3, c3) = res.0[3].overflowing_add(c2 as u64);
        
        // c3 is the 256th bit. If it's set, then res + 19 >= 2^256.
        // If bit 255 of r3 is set, it means res + 19 >= 2^255.
        if c3 || (r3 >> 63) != 0 {
            // It reached or exceeded 2^255, so original res was >= 2^255 - 19.
            // The reduced value is (res + 19) mod 2^255.
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
            // Carry from bit 256, add 38 (mod p)
            res = res.add_u64(38);
        }
        res
    }

    pub fn add(&self, other: &Self) -> Self {
        let (r0, c) = self.0[0].overflowing_add(other.0[0]);
        let (r1, c1) = self.0[1].overflowing_add(other.0[1]);
        let (r1, c2) = r1.overflowing_add(c as u64);
        let c = c1 | c2;
        let (r2, c1) = self.0[2].overflowing_add(other.0[2]);
        let (r2, c2) = r2.overflowing_add(c as u64);
        let c = c1 | c2;
        let (r3, c1) = self.0[3].overflowing_add(other.0[3]);
        let (r3, c2) = r3.overflowing_add(c as u64);
        let c = c1 | c2;
        
        let mut res = FieldElement([r0, r1, r2, r3]);
        if c {
            res = res.add_u64(38);
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
            // Borrow from 2^256, subtract 38 (mod p)
            // (2^256 - |x-y|) - 38 = (2^256 - 38) - |x-y| = 2p - |x-y|
            let (r0, b) = res.0[0].overflowing_sub(38);
            let (r1, b) = res.0[1].overflowing_sub(b as u64);
            let (r2, b) = res.0[2].overflowing_sub(b as u64);
            let (r3, _) = res.0[3].overflowing_sub(b as u64);
            res = FieldElement([r0, r1, r2, r3]);
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
            let (r0, b) = res.0[0].overflowing_sub(38);
            let (r1, b) = res.0[1].overflowing_sub(b as u64);
            let (r2, b) = res.0[2].overflowing_sub(b as u64);
            let (r3, _) = res.0[3].overflowing_sub(b as u64);
            res = FieldElement([r0, r1, r2, r3]);
        }
        res
    }

    pub fn mul(&self, other: &Self) -> Self {
        let mut r = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let prod = (self.0[i] as u128) * (other.0[j] as u128) + (r[i + j] as u128) + carry;
                r[i + j] = prod as u64;
                carry = prod >> 64;
            }
            r[i + 4] = carry as u64;
        }
        
        // Reduction: r = r_lo + r_hi * 2^256
        // 2^256 \equiv 38 \pmod p
        let mut res_limbs = [0u64; 4];
        res_limbs.copy_from_slice(&r[0..4]);
        
        let mut carry = 0u128;
        for i in 0..4 {
            let term = (r[i + 4] as u128) * 38 + (res_limbs[i] as u128) + carry;
            res_limbs[i] = term as u64;
            carry = term >> 64;
        }
        
        let mut res = FieldElement(res_limbs);
        if carry > 0 {
            res = res.add_u64((carry * 38) as u64);
        }
        
        // Handle bit 255
        let high_bit = res.0[3] >> 63;
        if high_bit > 0 {
            res.0[3] &= 0x7FFFFFFFFFFFFFFF;
            res = res.add_u64(high_bit * 19);
        }
        
        res
    }

    pub fn square(&self) -> Self {
        self.mul(self)
    }

    pub fn invert(&self) -> Self {
        // a^(p-2) where p-2 = 2^255 - 21
        let mut res = FieldElement::ONE;
        let mut base = *self;
        
        // 2^255 - 21 in little-endian binary:
        // 21 = 16 + 4 + 1 = 10101 binary.
        // 2^255 - 21 = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEB
        let p_minus_2: [u64; 4] = [
            0xFFFFFFFFFFFFFFEB,
            0xFFFFFFFFFFFFFFFF,
            0xFFFFFFFFFFFFFFFF,
            0x7FFFFFFFFFFFFFFF,
        ];
        
        for i in 0..255 {
            if (p_minus_2[i / 64] >> (i % 64)) & 1 != 0 {
                res = res.mul(&base);
            }
            base = base.square();
        }
        res
    }

    pub fn pow_p38(&self) -> Self {
        // a^((p+3)/8) where (p+3)/8 = 2^252 - 2
        let mut res = FieldElement::ONE;
        let mut base = *self;
        
        let exp: [u64; 4] = [
            0xFFFFFFFFFFFFFFFE,
            0xFFFFFFFFFFFFFFFF,
            0xFFFFFFFFFFFFFFFF,
            0x0FFFFFFFFFFFFFFF,
        ];
        
        for i in 0..252 {
            if (exp[i / 64] >> (i % 64)) & 1 != 0 {
                res = res.mul(&base);
            }
            base = base.square();
        }
        res
    }
}
