//! Curve25519 field arithmetic optimized for RISC-V.
//! Prime p = 2^255 - 19.

use core::convert::TryInto;

/// A field element in Curve25519.
/// Represented as 4 x u64 in little-endian.
#[derive(Clone, Copy, Debug, Eq)]
pub struct FieldElement(pub [u64; 4]);

impl PartialEq for FieldElement {
    fn eq(&self, other: &Self) -> bool {
        self.reduce().0 == other.reduce().0
    }
}

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
        let h = res.0[3] >> 63;
        res.0[3] &= 0x7FFFFFFFFFFFFFFF;
        res = res.add_u64(h * 19);
        
        // Second pass: handle any carry into bit 255 from the first pass
        let h = res.0[3] >> 63;
        res.0[3] &= 0x7FFFFFFFFFFFFFFF;
        res = res.add_u64(h * 19);
        
        // Trial subtraction of p = 2^255 - 19
        // res >= p  <=>  res + 19 >= 2^255
        let (r0, c0) = res.0[0].overflowing_add(19);
        let (r1, c1) = res.0[1].overflowing_add(c0 as u64);
        let (r2, c2) = res.0[2].overflowing_add(c1 as u64);
        let (r3, c3) = res.0[3].overflowing_add(c2 as u64);
        
        if c3 || (r3 >> 63) != 0 {
            FieldElement([r0, r1, r2, r3 & 0x7FFFFFFFFFFFFFFF])
        } else {
            res
        }
    }

    #[inline(always)]
    pub fn add_u64(&self, val: u64) -> Self {
        let c0 = (self.0[0] as u128) + (val as u128);
        let c1 = (self.0[1] as u128) + (c0 >> 64);
        let c2 = (self.0[2] as u128) + (c1 >> 64);
        let c3 = (self.0[3] as u128) + (c2 >> 64);

        let carry = (c3 >> 64) as u64;
        let r0 = c0 as u64;
        let r1 = c1 as u64;
        let r2 = c2 as u64;
        let r3 = c3 as u64;

        let c0_red = (r0 as u128) + ((carry * 38) as u128);
        let c1_red = (r1 as u128) + (c0_red >> 64);
        let c2_red = (r2 as u128) + (c1_red >> 64);
        let c3_red = (r3 as u128) + (c2_red >> 64);

        FieldElement([c0_red as u64, c1_red as u64, c2_red as u64, c3_red as u64])
    }


    #[inline(always)]
    pub fn add(&self, other: &Self) -> Self {
        let c0 = (self.0[0] as u128) + (other.0[0] as u128);
        let c1 = (self.0[1] as u128) + (other.0[1] as u128) + (c0 >> 64);
        let c2 = (self.0[2] as u128) + (other.0[2] as u128) + (c1 >> 64);
        let c3 = (self.0[3] as u128) + (other.0[3] as u128) + (c2 >> 64);

        let carry = (c3 >> 64) as u64;
        let r0 = c0 as u64;
        let r1 = c1 as u64;
        let r2 = c2 as u64;
        let r3 = c3 as u64;

        let c0_red = (r0 as u128) + ((carry * 38) as u128);
        let c1_red = (r1 as u128) + (c0_red >> 64);
        let c2_red = (r2 as u128) + (c1_red >> 64);
        let c3_red = (r3 as u128) + (c2_red >> 64);

        FieldElement([c0_red as u64, c1_red as u64, c2_red as u64, c3_red as u64])
    }

    #[inline(always)]
    pub fn sub(&self, other: &Self) -> Self {
        let b0 = ((self.0[0] as u128) | (1 << 64)) - (other.0[0] as u128);
        let b1 = ((self.0[1] as u128) | (1 << 64)) - (other.0[1] as u128) + (b0 >> 64) - 1;
        let b2 = ((self.0[2] as u128) | (1 << 64)) - (other.0[2] as u128) + (b1 >> 64) - 1;
        let b3 = ((self.0[3] as u128) | (1 << 64)) - (other.0[3] as u128) + (b2 >> 64) - 1;

        let borrow = 1 - (b3 >> 64) as u64;
        let r0 = b0 as u64;
        let r1 = b1 as u64;
        let r2 = b2 as u64;
        let r3 = b3 as u64;

        let b0_red = ((r0 as u128) | (1 << 64)) - ((borrow * 38) as u128);
        let b1_red = ((r1 as u128) | (1 << 64)) + (b0_red >> 64) - 1;
        let b2_red = ((r2 as u128) | (1 << 64)) + (b1_red >> 64) - 1;
        let b3_red = ((r3 as u128) | (1 << 64)) + (b2_red >> 64) - 1;

        FieldElement([b0_red as u64, b1_red as u64, b2_red as u64, b3_red as u64])
    }
    
    #[inline(always)]
    pub fn sub_u64(&self, val: u64) -> Self {
        let b0 = ((self.0[0] as u128) | (1 << 64)) - (val as u128);
        let b1 = ((self.0[1] as u128) | (1 << 64)) + (b0 >> 64) - 1;
        let b2 = ((self.0[2] as u128) | (1 << 64)) + (b1 >> 64) - 1;
        let b3 = ((self.0[3] as u128) | (1 << 64)) + (b2 >> 64) - 1;

        let borrow = 1 - (b3 >> 64) as u64;
        let r0 = b0 as u64;
        let r1 = b1 as u64;
        let r2 = b2 as u64;
        let r3 = b3 as u64;

        let b0_red = ((r0 as u128) | (1 << 64)) - ((borrow * 38) as u128);
        let b1_red = ((r1 as u128) | (1 << 64)) + (b0_red >> 64) - 1;
        let b2_red = ((r2 as u128) | (1 << 64)) + (b1_red >> 64) - 1;
        let b3_red = ((r3 as u128) | (1 << 64)) + (b2_red >> 64) - 1;

        FieldElement([b0_red as u64, b1_red as u64, b2_red as u64, b3_red as u64])
    }


    #[inline(always)]
    fn reduce_product(r: &[u64; 8]) -> Self {
        let t0 = (r[4] as u128) * 38 + (r[0] as u128);
        let t1 = (r[5] as u128) * 38 + (r[1] as u128) + (t0 >> 64);
        let t2 = (r[6] as u128) * 38 + (r[2] as u128) + (t1 >> 64);
        let t3 = (r[7] as u128) * 38 + (r[3] as u128) + (t2 >> 64);
        let carry = (t3 >> 64) as u64;

        let r0 = t0 as u64;
        let r1 = t1 as u64;
        let r2 = t2 as u64;
        let r3 = t3 as u64;

        let c0 = (r0 as u128) + ((carry * 38) as u128);
        let c1 = (r1 as u128) + (c0 >> 64);
        let c2 = (r2 as u128) + (c1 >> 64);
        let c3 = (r3 as u128) + (c2 >> 64);

        let h = ((c3 as u64) >> 63) as u64;
        let r3_final = (c3 as u64) & 0x7FFFFFFFFFFFFFFF;

        let d0 = (c0 as u64 as u128) + ((h * 19) as u128);
        let d1 = (c1 as u64 as u128) + (d0 >> 64);
        let d2 = (c2 as u64 as u128) + (d1 >> 64);
        let d3 = (r3_final as u128) + (d2 >> 64);

        FieldElement([d0 as u64, d1 as u64, d2 as u64, d3 as u64])
    }

    #[inline(always)]
    pub fn mul(&self, other: &Self) -> Self {
        let a = &self.0;
        let b = &other.0;

        // Row 0
        let p0 = (a[0] as u128) * (b[0] as u128);
        let r0 = p0 as u64;
        let mut carry = p0 >> 64;

        let p1 = (a[0] as u128) * (b[1] as u128) + carry;
        let mut r1 = p1 as u64;
        carry = p1 >> 64;

        let p2 = (a[0] as u128) * (b[2] as u128) + carry;
        let mut r2 = p2 as u64;
        carry = p2 >> 64;

        let p3 = (a[0] as u128) * (b[3] as u128) + carry;
        let mut r3 = p3 as u64;
        let mut r4 = (p3 >> 64) as u64;

        // Row 1
        let p0 = (a[1] as u128) * (b[0] as u128) + (r1 as u128);
        r1 = p0 as u64;
        carry = p0 >> 64;

        let p1 = (a[1] as u128) * (b[1] as u128) + (r2 as u128) + carry;
        r2 = p1 as u64;
        carry = p1 >> 64;

        let p2 = (a[1] as u128) * (b[2] as u128) + (r3 as u128) + carry;
        r3 = p2 as u64;
        carry = p2 >> 64;

        let p3 = (a[1] as u128) * (b[3] as u128) + (r4 as u128) + carry;
        r4 = p3 as u64;
        let mut r5 = (p3 >> 64) as u64;

        // Row 2
        let p0 = (a[2] as u128) * (b[0] as u128) + (r2 as u128);
        r2 = p0 as u64;
        carry = p0 >> 64;

        let p1 = (a[2] as u128) * (b[1] as u128) + (r3 as u128) + carry;
        r3 = p1 as u64;
        carry = p1 >> 64;

        let p2 = (a[2] as u128) * (b[2] as u128) + (r4 as u128) + carry;
        r4 = p2 as u64;
        carry = p2 >> 64;

        let p3 = (a[2] as u128) * (b[3] as u128) + (r5 as u128) + carry;
        r5 = p3 as u64;
        let mut r6 = (p3 >> 64) as u64;

        // Row 3
        let p0 = (a[3] as u128) * (b[0] as u128) + (r3 as u128);
        r3 = p0 as u64;
        carry = p0 >> 64;

        let p1 = (a[3] as u128) * (b[1] as u128) + (r4 as u128) + carry;
        r4 = p1 as u64;
        carry = p1 >> 64;

        let p2 = (a[3] as u128) * (b[2] as u128) + (r5 as u128) + carry;
        r5 = p2 as u64;
        carry = p2 >> 64;

        let p3 = (a[3] as u128) * (b[3] as u128) + (r6 as u128) + carry;
        r6 = p3 as u64;
        let r7 = (p3 >> 64) as u64;

        Self::reduce_product(&[r0, r1, r2, r3, r4, r5, r6, r7])
    }

    /// Dedicated 256-bit squaring using upper-triangular cross products (10M vs 16M).
    #[inline(always)]
    pub fn square(&self) -> Self {
        let a = &self.0;

        // Step 1: Strictly upper-triangular products (6 multiplications)
        let p01 = (a[0] as u128) * (a[1] as u128);
        let p02 = (a[0] as u128) * (a[2] as u128);
        let p03 = (a[0] as u128) * (a[3] as u128);

        let w1 = p01 as u64;
        let mut c = p01 >> 64;

        let t = p02 + c;
        let w2 = t as u64;
        c = t >> 64;

        let t = p03 + c;
        let mut w3 = t as u64;
        let mut w4 = (t >> 64) as u64;

        // Row 1
        let p12 = (a[1] as u128) * (a[2] as u128);
        let p13 = (a[1] as u128) * (a[3] as u128);

        let t = (p12 as u128) + (w3 as u128);
        w3 = t as u64;
        c = t >> 64;

        let t = (p13 as u128) + (w4 as u128) + c;
        w4 = t as u64;
        let mut w5 = (t >> 64) as u64;

        // Row 2
        let p23 = (a[2] as u128) * (a[3] as u128);

        let t = (p23 as u128) + (w5 as u128);
        w5 = t as u64;
        let w6 = (t >> 64) as u64;

        // Step 2: Unrolled shift left by 1 bit (2 * cross products)
        let r0 = 0u64;
        let r1 = w1 << 1;
        let r2 = (w2 << 1) | (w1 >> 63);
        let r3 = (w3 << 1) | (w2 >> 63);
        let r4 = (w4 << 1) | (w3 >> 63);
        let r5 = (w5 << 1) | (w4 >> 63);
        let r6 = (w6 << 1) | (w5 >> 63);
        let r7 = w6 >> 63;

        // Step 3: Add diagonal squares a[i]^2 (4 multiplications)
        let sq0 = (a[0] as u128) * (a[0] as u128);
        let t = (r0 as u128) + (sq0 as u64 as u128);
        let out0 = t as u64;
        let mut c = (sq0 >> 64) + (t >> 64);

        let t = (r1 as u128) + c;
        let out1 = t as u64;
        c = t >> 64;

        let sq1 = (a[1] as u128) * (a[1] as u128);
        let t = (r2 as u128) + (sq1 as u64 as u128) + c;
        let out2 = t as u64;
        c = (sq1 >> 64) + (t >> 64);

        let t = (r3 as u128) + c;
        let out3 = t as u64;
        c = t >> 64;

        let sq2 = (a[2] as u128) * (a[2] as u128);
        let t = (r4 as u128) + (sq2 as u64 as u128) + c;
        let out4 = t as u64;
        c = (sq2 >> 64) + (t >> 64);

        let t = (r5 as u128) + c;
        let out5 = t as u64;
        c = t >> 64;

        let sq3 = (a[3] as u128) * (a[3] as u128);
        let t = (r6 as u128) + (sq3 as u64 as u128) + c;
        let out6 = t as u64;
        c = (sq3 >> 64) + (t >> 64);

        let t = (r7 as u128) + c;
        let out7 = t as u64;

        Self::reduce_product(&[out0, out1, out2, out3, out4, out5, out6, out7])
    }

    #[inline(always)]
    fn sqn(&self, n: usize) -> Self {
        let mut x = *self;
        for _ in 0..n {
            x = x.square();
        }
        x
    }

    /// Computes self^(p-2) mod p = self^(2^255 - 21) using an optimal addition chain
    /// (254 squarings and 11 multiplications) instead of 253 full multiplications.
    pub fn invert(&self) -> Self {
        let z3 = self.sqn(1).mul(self);
        let z_11 = self.sqn(3).mul(&z3);
        let z7 = z3.sqn(1).mul(self);
        let z15 = z7.sqn(1).mul(self);
        let z31 = z15.sqn(1).mul(self);

        let t0 = z31.sqn(5).mul(&z31);     // 2^10 - 1
        let t1 = t0.sqn(10).mul(&t0);       // 2^20 - 1
        let t2 = t1.sqn(20).mul(&t1);       // 2^40 - 1
        let t3 = t2.sqn(10).mul(&t0);       // 2^50 - 1
        let t4 = t3.sqn(50).mul(&t3);       // 2^100 - 1
        let t5 = t4.sqn(100).mul(&t4);      // 2^200 - 1
        let t6 = t5.sqn(50).mul(&t3);       // 2^250 - 1

        t6.sqn(5).mul(&z_11)
    }

    /// Computes self^((p+3)/8) = self^(2^252 - 2) using an optimal addition chain
    /// (252 squarings and 10 multiplications) instead of 250 full multiplications.
    pub fn pow_p38(&self) -> Self {
        let z3 = self.sqn(1).mul(self);
        let z7 = z3.sqn(1).mul(self);
        let z15 = z7.sqn(1).mul(self);
        let z31 = z15.sqn(1).mul(self);

        let t0 = z31.sqn(5).mul(&z31);     // 2^10 - 1
        let t1 = t0.sqn(10).mul(&t0);       // 2^20 - 1
        let t2 = t1.sqn(20).mul(&t1);       // 2^40 - 1
        let t3 = t2.sqn(10).mul(&t0);       // 2^50 - 1
        let t4 = t3.sqn(50).mul(&t3);       // 2^100 - 1
        let t5 = t4.sqn(100).mul(&t4);      // 2^200 - 1
        let t6 = t5.sqn(50).mul(&t3);       // 2^250 - 1

        t6.sqn(1).mul(self).sqn(1)
    }

    /// Invert 4 field elements simultaneously using Montgomery's trick.
    /// Uses 1 inversion and 9 multiplications instead of 4 full inversions.
    pub fn batch_invert_4(elements: [&FieldElement; 4]) -> [FieldElement; 4] {
        if elements[0] == &FieldElement::ZERO
            || elements[1] == &FieldElement::ZERO
            || elements[2] == &FieldElement::ZERO
            || elements[3] == &FieldElement::ZERO
        {
            return [
                elements[0].invert(),
                elements[1].invert(),
                elements[2].invert(),
                elements[3].invert(),
            ];
        }

        let c0 = *elements[0];
        let c1 = c0.mul(elements[1]);
        let c2 = c1.mul(elements[2]);
        let c3 = c2.mul(elements[3]);

        let mut inv = c3.invert();

        let inv3 = inv.mul(&c2);
        inv = inv.mul(elements[3]);

        let inv2 = inv.mul(&c1);
        inv = inv.mul(elements[2]);

        let inv1 = inv.mul(&c0);
        let inv0 = inv.mul(elements[1]);

        [inv0, inv1, inv2, inv3]
    }

    /// Invert 8 field elements simultaneously using Montgomery's trick.
    /// Uses 1 inversion and 21 multiplications instead of 8 full inversions.
    pub fn batch_invert_8(elements: [&FieldElement; 8]) -> [FieldElement; 8] {
        let mut c = [FieldElement::ZERO; 8];
        c[0] = *elements[0];
        for i in 1..8 {
            c[i] = c[i - 1].mul(elements[i]);
        }

        let mut inv = c[7].invert();
        let mut invs = [FieldElement::ZERO; 8];
        for i in (1..8).rev() {
            invs[i] = inv.mul(&c[i - 1]);
            inv = inv.mul(elements[i]);
        }
        invs[0] = inv;
        invs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_eq;

    #[test]
    fn test_sub_reduce() {
        let zero = FieldElement::ZERO;
        let one = FieldElement::ONE;
        let res = zero.sub(&one).reduce();
        
        // Expected: 2^255 - 20
        // Limb 0: 0xFFFFFFFFFFFFFFEC (18446744073709551596)
        // Limb 1: 0xFFFFFFFFFFFFFFFF
        // Limb 2: 0xFFFFFFFFFFFFFFFF
        // Limb 3: 0x7FFFFFFFFFFFFFFF (9223372036854775807)
        assert_eq!(res.0[0], 0xFFFFFFFFFFFFFFEC);
        assert_eq!(res.0[1], 0xFFFFFFFFFFFFFFFF);
        assert_eq!(res.0[2], 0xFFFFFFFFFFFFFFFF);
        assert_eq!(res.0[3], 0x7FFFFFFFFFFFFFFF);
    }

    #[test]
    fn test_batch_invert_4() {
        let a = FieldElement([3, 0, 0, 0]);
        let b = FieldElement([7, 0, 0, 0]);
        let c = FieldElement([11, 0, 0, 0]);
        let d = FieldElement([13, 0, 0, 0]);

        let invs = FieldElement::batch_invert_4([&a, &b, &c, &d]);

        assert_eq!(a.mul(&invs[0]).reduce(), FieldElement::ONE);
        assert_eq!(b.mul(&invs[1]).reduce(), FieldElement::ONE);
        assert_eq!(c.mul(&invs[2]).reduce(), FieldElement::ONE);
        assert_eq!(d.mul(&invs[3]).reduce(), FieldElement::ONE);
    }
}
