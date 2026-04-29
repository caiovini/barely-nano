//! Curve25519 field arithmetic optimized for RISC-V.
//! Prime p = 2^255 - 19.

// use core::arch::asm;

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
        FieldElement(limbs)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&self.0[i].to_le_bytes());
        }
        bytes
    }

    /// Addition modulo p
    pub fn add(&self, other: &Self) -> Self {
        let (r0, c) = self.0[0].overflowing_add(other.0[0]);
        let (r1, c) = self.0[1].overflowing_add(other.0[1] + c as u64);
        let (r2, c) = self.0[2].overflowing_add(other.0[2] + c as u64);
        let (r3, c) = self.0[3].overflowing_add(other.0[3] + c as u64);
        
        let mut res = FieldElement([r0, r1, r2, r3]);
        // If there's a carry or the value >= 2^255 - 19
        if c || (res.0[3] >> 63) != 0 {
            // res = res - p = res - (2^255 - 19) = res + 19 - 2^255
            let (r0, c) = res.0[0].overflowing_add(19);
            let (r1, c) = res.0[1].overflowing_add(c as u64);
            let (r2, c) = res.0[2].overflowing_add(c as u64);
            let r3 = res.0[3].wrapping_add(c as u64) & !(1 << 63);
            res = FieldElement([r0, r1, r2, r3]);
        }
        res
    }

    /// Subtraction modulo p
    pub fn sub(&self, other: &Self) -> Self {
        let (r0, b) = self.0[0].overflowing_sub(other.0[0]);
        let (r1, b) = self.0[1].overflowing_sub(other.0[1] + b as u64);
        let (r2, b) = self.0[2].overflowing_sub(other.0[2] + b as u64);
        let (r3, b) = self.0[3].overflowing_sub(other.0[3] + b as u64);
        
        let mut res = FieldElement([r0, r1, r2, r3]);
        if b {
            // res = res + p = res + 2^255 - 19
            let (r0, b) = res.0[0].overflowing_sub(19);
            let (r1, b) = res.0[1].overflowing_sub(b as u64);
            let (r2, b) = res.0[2].overflowing_sub(b as u64);
            let r3 = res.0[3].wrapping_sub(b as u64) | (1 << 63);
            res = FieldElement([r0, r1, r2, r3]);
        }
        res
    }

    /// Multiplication modulo p
    /// Uses 128-bit intermediate products and reduction 2^255 = 19
    pub fn mul(&self, other: &Self) -> Self {
        let mut r = [0u128; 7];
        for i in 0..4 {
            for j in 0..4 {
                r[i + j] += (self.0[i] as u128) * (other.0[j] as u128);
            }
        }
        
        // Reduction
        // r4, r5, r6 are coefficients of 2^256, 2^320, 2^384
        // 2^256 = 2 * 19 * 2^0 ? No, 2^255 = 19.
        // 2^256 = 38
        
        // let mut c: u128;
        
        // Limb reduction: 2^256 -> 38, 2^320 -> 38 * 2^64, etc.
        let r0 = r[0] + r[4] * 38;
        let r1 = r[1] + r[5] * 38;
        let r2 = r[2] + r[6] * 38;
        let r3 = r[3]; // coeff of 2^192
        
        // Propagate carries
        let (l0, c) = (r0 as u64, (r0 >> 64));
        let (l1, c) = ((r1 + c) as u64, ((r1 + c) >> 64));
        let (l2, c) = ((r2 + c) as u64, ((r2 + c) >> 64));
        let (l3, c) = ((r3 + c) as u64, ((r3 + c) >> 64));
        
        // Final carry reduction: c * 2^256 = c * 38
        let mut res = FieldElement([l0, l1, l2, l3]);
        if c > 0 {
            let overflow = FieldElement([ (c * 38) as u64, 0, 0, 0]);
            res = res.add(&overflow);
        }
        
        // Handle bit 255
        if (res.0[3] >> 63) != 0 {
            let high_bit = res.0[3] >> 63;
            res.0[3] &= !(1 << 63);
            res = res.add(&FieldElement([high_bit * 19, 0, 0, 0]));
        }
        
        res
    }

    pub fn square(&self) -> Self {
        self.mul(self)
    }

    pub fn invert(&self) -> Self {
        // a^(p-2) where p-2 = 2^255 - 21
        // This is a long sequence of squares and multiplies.
        // Simplified version for now:
        let mut res = FieldElement::ONE;
        let mut p_minus_2 = [0u64; 4];
        // 2^255 - 21 = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEB
        p_minus_2[0] = 0xFFFFFFFFFFFFFFEB;
        p_minus_2[1] = 0xFFFFFFFFFFFFFFFF;
        p_minus_2[2] = 0xFFFFFFFFFFFFFFFF;
        p_minus_2[3] = 0x7FFFFFFFFFFFFFFF;

        let mut base = *self;
        for i in 0..255 {
            if (p_minus_2[i / 64] >> (i % 64)) & 1 != 0 {
                res = res.mul(&base);
            }
            base = base.square();
        }
        res
    }

    pub fn pow2523(&self) -> Self {
        // a^((p-5)/8)
        let mut res = FieldElement::ONE;
        let mut exp = [0u64; 4];
        // (p-5)/8 = (2^255 - 24) / 8 = 2^252 - 3
        exp[0] = 0xFFFFFFFFFFFFFFFD;
        exp[1] = 0xFFFFFFFFFFFFFFFF;
        exp[2] = 0xFFFFFFFFFFFFFFFF;
        exp[3] = 0x0FFFFFFFFFFFFFFF;

        let mut base = *self;
        for i in 0..253 {
            if (exp[i / 64] >> (i % 64)) & 1 != 0 {
                res = res.mul(&base);
            }
            base = base.square();
        }
        res
    }
}
