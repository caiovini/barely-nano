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

    pub fn add_u64(&self, val: u64) -> Self {
        let (r0, c) = self.0[0].overflowing_add(val);
        let (r1, c) = self.0[1].overflowing_add(c as u64);
        let (r2, c) = self.0[2].overflowing_add(c as u64);
        let (r3, c) = self.0[3].overflowing_add(c as u64);
        
        let mut res = FieldElement([r0, r1, r2, r3]);
        if c {
            res = res.add_u64_raw(38);
        }
        res
    }

    fn add_u64_raw(&self, val: u64) -> Self {
        let (r0, c) = self.0[0].overflowing_add(val);
        let (r1, c) = self.0[1].overflowing_add(c as u64);
        let (r2, c) = self.0[2].overflowing_add(c as u64);
        let (r3, _) = self.0[3].overflowing_add(c as u64);
        FieldElement([r0, r1, r2, r3])
    }

    pub fn add(&self, other: &Self) -> Self {
        let mut res = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            let sum = (self.0[i] as u128) + (other.0[i] as u128) + carry;
            res[i] = sum as u64;
            carry = sum >> 64;
        }
        let mut fe = FieldElement(res);
        if carry > 0 {
            fe = fe.add_u64((carry * 38) as u64);
        }
        fe
    }

    pub fn sub(&self, other: &Self) -> Self {
        let mut res = [0u64; 4];
        let mut borrow = 0i128;
        for i in 0..4 {
            let diff = (self.0[i] as i128) - (other.0[i] as i128) - borrow;
            if diff < 0 {
                res[i] = (diff + (1 << 64)) as u64;
                borrow = 1;
            } else {
                res[i] = diff as u64;
                borrow = 0;
            }
        }
        let mut fe = FieldElement(res);
        if borrow > 0 {
            fe = fe.sub_u64((borrow * 38) as u64);
        }
        fe
    }
    
    pub fn sub_u64(&self, val: u64) -> Self {
        let mut res = [0u64; 4];
        let mut borrow;
        
        let diff = (self.0[0] as i128) - (val as i128);
        if diff < 0 {
            res[0] = (diff + (1 << 64)) as u64;
            borrow = 1;
        } else {
            res[0] = diff as u64;
            borrow = 0;
        }
        
        for i in 1..4 {
            let diff = (self.0[i] as i128) - borrow;
            if diff < 0 {
                res[i] = (diff + (1 << 64)) as u64;
                borrow = 1;
            } else {
                res[i] = diff as u64;
                borrow = 0;
            }
        }
        
        let mut fe = FieldElement(res);
        if borrow > 0 {
            fe = fe.sub_u64_raw(38);
        }
        fe
    }

    fn sub_u64_raw(&self, val: u64) -> Self {
        let mut res = [0u64; 4];
        let mut borrow;
        
        let diff = (self.0[0] as i128) - (val as i128);
        if diff < 0 {
            res[0] = (diff + (1 << 64)) as u64;
            borrow = 1;
        } else {
            res[0] = diff as u64;
            borrow = 0;
        }
        
        for i in 1..4 {
            let diff = (self.0[i] as i128) - borrow;
            if diff < 0 {
                res[i] = (diff + (1 << 64)) as u64;
                borrow = 1;
            } else {
                res[i] = diff as u64;
                borrow = 0;
            }
        }
        FieldElement(res)
    }

    #[inline(always)]
    fn reduce_product(r: &[u64; 8]) -> Self {
        let mut carry = 0u128;
        let mut res_limbs = [0u64; 4];
        for i in 0..4 {
            let term = (r[i + 4] as u128) * 38 + (r[i] as u128) + carry;
            res_limbs[i] = term as u64;
            carry = term >> 64;
        }

        let mut res = FieldElement(res_limbs);
        if carry > 0 {
            res = res.add_u64_raw((carry * 38) as u64);
        }

        let h = res.0[3] >> 63;
        res.0[3] &= 0x7FFFFFFFFFFFFFFF;
        if h > 0 {
            res = res.add_u64_raw(h * 19);
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
        Self::reduce_product(&r)
    }

    /// Dedicated 256-bit squaring using upper-triangular cross products (10M vs 16M).
    pub fn square(&self) -> Self {
        let mut r = [0u64; 8];
        for i in 0..3 {
            let mut carry = 0u128;
            for j in (i + 1)..4 {
                let prod = (self.0[i] as u128) * (self.0[j] as u128) + (r[i + j] as u128) + carry;
                r[i + j] = prod as u64;
                carry = prod >> 64;
            }
            r[i + 4] = carry as u64;
        }

        // Shift r left by 1 bit (doubling strictly upper-triangular cross products)
        let mut carry_bit = 0u64;
        for i in 0..8 {
            let new_carry = r[i] >> 63;
            r[i] = (r[i] << 1) | carry_bit;
            carry_bit = new_carry;
        }

        // Add diagonal squares a[i]^2
        for i in 0..4 {
            let sq = (self.0[i] as u128) * (self.0[i] as u128);
            let mut idx = 2 * i;
            let mut c = sq;
            while c > 0 && idx < 8 {
                let s = (r[idx] as u128) + (c as u64 as u128);
                r[idx] = s as u64;
                c = (c >> 64) + (s >> 64);
                idx += 1;
            }
        }

        Self::reduce_product(&r)
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
