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

        // Split t3 at bit 63: 2^255 == 19 (mod p), so ((t3 >> 63) * 19) folds both the high carry
        // and bit 255 in a single carry chain instead of two separate 256-bit carry chains.
        let carry255 = ((t3 >> 63) as u64) * 19;
        let r0 = t0 as u64;
        let r1 = t1 as u64;
        let r2 = t2 as u64;
        let r3 = (t3 as u64) & 0x7FFFFFFFFFFFFFFF;

        let c0 = (r0 as u128) + (carry255 as u128);
        let c1 = (r1 as u128) + (c0 >> 64);
        let c2 = (r2 as u128) + (c1 >> 64);
        let c3 = (r3 as u128) + (c2 >> 64);

        FieldElement([c0 as u64, c1 as u64, c2 as u64, c3 as u64])
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
        if elements.iter().any(|e| *e == &FieldElement::ZERO) {
            return core::array::from_fn(|i| elements[i].invert());
        }
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

    /// Invert 16 field elements simultaneously using Montgomery's trick.
    /// Uses 1 inversion and 45 multiplications instead of 16 full inversions.
    pub fn batch_invert_16(elements: [&FieldElement; 16]) -> [FieldElement; 16] {
        if elements.iter().any(|e| *e == &FieldElement::ZERO) {
            return core::array::from_fn(|i| elements[i].invert());
        }
        let mut c = [FieldElement::ZERO; 16];
        c[0] = *elements[0];
        for i in 1..16 {
            c[i] = c[i - 1].mul(elements[i]);
        }

        let mut inv = c[15].invert();
        let mut invs = [FieldElement::ZERO; 16];
        for i in (1..16).rev() {
            invs[i] = inv.mul(&c[i - 1]);
            inv = inv.mul(elements[i]);
        }
        invs[0] = inv;
        invs
    }
}

const MASK51: u64 = (1u64 << 51) - 1;
const TWO_P0: u64 = (1u64 << 52) - 38;
const TWO_P14: u64 = (1u64 << 52) - 2;

/// 16-lane transposed Curve25519 field element in Radix-`2^51` (`5 x 16 x u64` = `5` `VLEN=1024`
/// `LMUL=1` vector registers). All arithmetic maintains the strict limb invariant `limb < 2^51 + 4096`.
#[repr(C, align(128))]
#[derive(Clone, Copy, Debug)]
pub struct Fe51x16(pub [[u64; 16]; 5]);

impl Fe51x16 {
    pub const ZERO: Self = Self([[0u64; 16]; 5]);
    pub const ONE: Self = Self([[1u64; 16], [0u64; 16], [0u64; 16], [0u64; 16], [0u64; 16]]);

    #[inline]
    pub fn from_fe_array(elems: &[FieldElement; 16]) -> Self {
        let mut out = Self::ZERO;
        for lane in 0..16 {
            let r = elems[lane].reduce();
            let w0 = r.0[0];
            let w1 = r.0[1];
            let w2 = r.0[2];
            let w3 = r.0[3];
            out.0[0][lane] = w0 & MASK51;
            out.0[1][lane] = ((w0 >> 51) | (w1 << 13)) & MASK51;
            out.0[2][lane] = ((w1 >> 38) | (w2 << 26)) & MASK51;
            out.0[3][lane] = ((w2 >> 25) | (w3 << 39)) & MASK51;
            out.0[4][lane] = (w3 >> 12) & MASK51;
        }
        out
    }

    #[inline]
    pub fn to_fe_array(&self) -> [FieldElement; 16] {
        let mut out = [FieldElement::ZERO; 16];
        for lane in 0..16 {
            let mut l0 = self.0[0][lane];
            let mut l1 = self.0[1][lane];
            let mut l2 = self.0[2][lane];
            let mut l3 = self.0[3][lane];
            let mut l4 = self.0[4][lane];

            let c0 = l0 >> 51; l0 &= MASK51; l1 += c0;
            let c1 = l1 >> 51; l1 &= MASK51; l2 += c1;
            let c2 = l2 >> 51; l2 &= MASK51; l3 += c2;
            let c3 = l3 >> 51; l3 &= MASK51; l4 += c3;
            let c4 = l4 >> 51; l4 &= MASK51; l0 += c4 * 19;
            let c0b = l0 >> 51; l0 &= MASK51; l1 += c0b;

            let w0 = l0 | (l1 << 51);
            let w1 = (l1 >> 13) | (l2 << 38);
            let w2 = (l2 >> 26) | (l3 << 25);
            let w3 = (l3 >> 39) | (l4 << 12);
            out[lane] = FieldElement([w0, w1, w2, w3]).reduce();
        }
        out
    }

    #[inline]
    pub fn add(&self, rhs: &Self) -> Self {
        #[cfg(all(target_arch = "riscv64", feature = "simd"))]
        if crate::blake2b::has_vlen_1024() {
            let mut out = Self::ZERO;
            // SAFETY: `has_vlen_1024()` verified `vlenb >= 128`, so `vsetivli zero, 16, e64, m1`
            // sets `VL = 16` (`128` bytes per `LMUL=1` register). `self`, `rhs`, and `out` are
            // valid `5 * 128`-byte `Fe51x16` buffers.
            unsafe {
                core::arch::asm!(
                    "vsetivli zero, 16, e64, m1",
                    "vle64.v v1, ({a})",
                    "vle64.v v6, ({b})",
                    "addi {ta}, {a}, 128",
                    "addi {tb}, {b}, 128",
                    "vle64.v v2, ({ta})",
                    "vle64.v v7, ({tb})",
                    "addi {ta}, {a}, 256",
                    "addi {tb}, {b}, 256",
                    "vle64.v v3, ({ta})",
                    "vle64.v v8, ({tb})",
                    "addi {ta}, {a}, 384",
                    "addi {tb}, {b}, 384",
                    "vle64.v v4, ({ta})",
                    "vle64.v v9, ({tb})",
                    "addi {ta}, {a}, 512",
                    "addi {tb}, {b}, 512",
                    "vle64.v v5, ({ta})",
                    "vle64.v v10, ({tb})",

                    "vadd.vv v1, v1, v6",
                    "vadd.vv v2, v2, v7",
                    "vadd.vv v3, v3, v8",
                    "vadd.vv v4, v4, v9",
                    "vadd.vv v5, v5, v10",

                    "vsrl.vx v11, v1, {sh51}",
                    "vsrl.vx v12, v2, {sh51}",
                    "vsrl.vx v13, v3, {sh51}",
                    "vsrl.vx v14, v4, {sh51}",
                    "vsrl.vx v15, v5, {sh51}",

                    "vand.vx v1, v1, {mask51}",
                    "vand.vx v2, v2, {mask51}",
                    "vand.vx v3, v3, {mask51}",
                    "vand.vx v4, v4, {mask51}",
                    "vand.vx v5, v5, {mask51}",

                    "vmul.vx v15, v15, {c19}",
                    "vadd.vv v1, v1, v15",
                    "vadd.vv v2, v2, v11",
                    "vadd.vv v3, v3, v12",
                    "vadd.vv v4, v4, v13",
                    "vadd.vv v5, v5, v14",

                    "vse64.v v1, ({o})",
                    "addi {ta}, {o}, 128",
                    "vse64.v v2, ({ta})",
                    "addi {ta}, {o}, 256",
                    "vse64.v v3, ({ta})",
                    "addi {ta}, {o}, 384",
                    "vse64.v v4, ({ta})",
                    "addi {ta}, {o}, 512",
                    "vse64.v v5, ({ta})",

                    a = in(reg) self.0.as_ptr(),
                    b = in(reg) rhs.0.as_ptr(),
                    o = in(reg) out.0.as_mut_ptr(),
                    sh51 = in(reg) 51usize,
                    mask51 = in(reg) MASK51,
                    c19 = in(reg) 19u64,
                    ta = out(reg) _,
                    tb = out(reg) _,
                    out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                    out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _,
                    out("v11") _, out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                    options(nostack),
                );
            }
            return out;
        }

        let mut out = Self::ZERO;
        for lane in 0..16 {
            let s0 = self.0[0][lane] + rhs.0[0][lane];
            let s1 = self.0[1][lane] + rhs.0[1][lane];
            let s2 = self.0[2][lane] + rhs.0[2][lane];
            let s3 = self.0[3][lane] + rhs.0[3][lane];
            let s4 = self.0[4][lane] + rhs.0[4][lane];

            out.0[0][lane] = (s0 & MASK51) + (s4 >> 51) * 19;
            out.0[1][lane] = (s1 & MASK51) + (s0 >> 51);
            out.0[2][lane] = (s2 & MASK51) + (s1 >> 51);
            out.0[3][lane] = (s3 & MASK51) + (s2 >> 51);
            out.0[4][lane] = (s4 & MASK51) + (s3 >> 51);
        }
        out
    }

    #[inline]
    pub fn sub(&self, rhs: &Self) -> Self {
        #[cfg(all(target_arch = "riscv64", feature = "simd"))]
        if crate::blake2b::has_vlen_1024() {
            let mut out = Self::ZERO;
            // SAFETY: `has_vlen_1024()` verified `vlenb >= 128`, so `vsetivli zero, 16, e64, m1`
            // sets `VL = 16`. `rhs` limbs satisfy `< 2^51 + 4096 < TWO_P0`, preventing underflow.
            unsafe {
                core::arch::asm!(
                    "vsetivli zero, 16, e64, m1",
                    "vle64.v v1, ({a})",
                    "vle64.v v6, ({b})",
                    "addi {ta}, {a}, 128",
                    "addi {tb}, {b}, 128",
                    "vle64.v v2, ({ta})",
                    "vle64.v v7, ({tb})",
                    "addi {ta}, {a}, 256",
                    "addi {tb}, {b}, 256",
                    "vle64.v v3, ({ta})",
                    "vle64.v v8, ({tb})",
                    "addi {ta}, {a}, 384",
                    "addi {tb}, {b}, 384",
                    "vle64.v v4, ({ta})",
                    "vle64.v v9, ({tb})",
                    "addi {ta}, {a}, 512",
                    "addi {tb}, {b}, 512",
                    "vle64.v v5, ({ta})",
                    "vle64.v v10, ({tb})",

                    "vadd.vx v1, v1, {tp0}",
                    "vadd.vx v2, v2, {tp14}",
                    "vadd.vx v3, v3, {tp14}",
                    "vadd.vx v4, v4, {tp14}",
                    "vadd.vx v5, v5, {tp14}",

                    "vsub.vv v1, v1, v6",
                    "vsub.vv v2, v2, v7",
                    "vsub.vv v3, v3, v8",
                    "vsub.vv v4, v4, v9",
                    "vsub.vv v5, v5, v10",

                    "vsrl.vx v11, v1, {sh51}",
                    "vsrl.vx v12, v2, {sh51}",
                    "vsrl.vx v13, v3, {sh51}",
                    "vsrl.vx v14, v4, {sh51}",
                    "vsrl.vx v15, v5, {sh51}",

                    "vand.vx v1, v1, {mask51}",
                    "vand.vx v2, v2, {mask51}",
                    "vand.vx v3, v3, {mask51}",
                    "vand.vx v4, v4, {mask51}",
                    "vand.vx v5, v5, {mask51}",

                    "vmul.vx v15, v15, {c19}",
                    "vadd.vv v1, v1, v15",
                    "vadd.vv v2, v2, v11",
                    "vadd.vv v3, v3, v12",
                    "vadd.vv v4, v4, v13",
                    "vadd.vv v5, v5, v14",

                    "vse64.v v1, ({o})",
                    "addi {ta}, {o}, 128",
                    "vse64.v v2, ({ta})",
                    "addi {ta}, {o}, 256",
                    "vse64.v v3, ({ta})",
                    "addi {ta}, {o}, 384",
                    "vse64.v v4, ({ta})",
                    "addi {ta}, {o}, 512",
                    "vse64.v v5, ({ta})",

                    a = in(reg) self.0.as_ptr(),
                    b = in(reg) rhs.0.as_ptr(),
                    o = in(reg) out.0.as_mut_ptr(),
                    tp0 = in(reg) TWO_P0,
                    tp14 = in(reg) TWO_P14,
                    sh51 = in(reg) 51usize,
                    mask51 = in(reg) MASK51,
                    c19 = in(reg) 19u64,
                    ta = out(reg) _,
                    tb = out(reg) _,
                    out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                    out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _,
                    out("v11") _, out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                    options(nostack),
                );
            }
            return out;
        }

        let mut out = Self::ZERO;
        for lane in 0..16 {
            let d0 = (self.0[0][lane] + TWO_P0) - rhs.0[0][lane];
            let d1 = (self.0[1][lane] + TWO_P14) - rhs.0[1][lane];
            let d2 = (self.0[2][lane] + TWO_P14) - rhs.0[2][lane];
            let d3 = (self.0[3][lane] + TWO_P14) - rhs.0[3][lane];
            let d4 = (self.0[4][lane] + TWO_P14) - rhs.0[4][lane];

            out.0[0][lane] = (d0 & MASK51) + (d4 >> 51) * 19;
            out.0[1][lane] = (d1 & MASK51) + (d0 >> 51);
            out.0[2][lane] = (d2 & MASK51) + (d1 >> 51);
            out.0[3][lane] = (d3 & MASK51) + (d2 >> 51);
            out.0[4][lane] = (d4 & MASK51) + (d3 >> 51);
        }
        out
    }

    #[inline]
    pub fn mul(&self, rhs: &Self) -> Self {
        #[cfg(all(target_arch = "riscv64", feature = "simd"))]
        if crate::blake2b::has_vlen_1024() {
            let mut out = Self::ZERO;
            // SAFETY: `has_vlen_1024()` verified `vlenb >= 128` (`VL = 16` for `e64, m1`).
            // All 16 5-limb multiplications and 128-bit carries stay strictly in `v0..v28`
            // without touching memory until the final 5 stores to `out`.
            unsafe {
                core::arch::asm!(
                    "vsetivli zero, 16, e64, m1",
                    "vle64.v v1, ({a})",
                    "vle64.v v6, ({b})",
                    "addi {ta}, {a}, 128",
                    "addi {tb}, {b}, 128",
                    "vle64.v v2, ({ta})",
                    "vle64.v v7, ({tb})",
                    "addi {ta}, {a}, 256",
                    "addi {tb}, {b}, 256",
                    "vle64.v v3, ({ta})",
                    "vle64.v v8, ({tb})",
                    "addi {ta}, {a}, 384",
                    "addi {tb}, {b}, 384",
                    "vle64.v v4, ({ta})",
                    "vle64.v v9, ({tb})",
                    "addi {ta}, {a}, 512",
                    "addi {tb}, {b}, 512",
                    "vle64.v v5, ({ta})",
                    "vle64.v v10, ({tb})",

                    "vmul.vx v11, v7, {c19}",
                    "vmul.vx v12, v8, {c19}",
                    "vmul.vx v13, v9, {c19}",
                    "vmul.vx v14, v10, {c19}",

                    // t0 = a0*b0 + a1*(19*b4) + a2*(19*b3) + a3*(19*b2) + a4*(19*b1)
                    "vmul.vv   v15, v1, v6",
                    "vmulhu.vv v16, v1, v6",
                    "vmul.vv   v25, v2, v14", "vmulhu.vv v26, v2, v14",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",
                    "vmul.vv   v25, v3, v13", "vmulhu.vv v26, v3, v13",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",
                    "vmul.vv   v25, v4, v12", "vmulhu.vv v26, v4, v12",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",
                    "vmul.vv   v25, v5, v11", "vmulhu.vv v26, v5, v11",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",

                    // t1 = a0*b1 + a1*b0 + a2*(19*b4) + a3*(19*b3) + a4*(19*b2)
                    "vmul.vv   v17, v1, v7",
                    "vmulhu.vv v18, v1, v7",
                    "vmul.vv   v25, v2, v6",  "vmulhu.vv v26, v2, v6",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",
                    "vmul.vv   v25, v3, v14", "vmulhu.vv v26, v3, v14",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",
                    "vmul.vv   v25, v4, v13", "vmulhu.vv v26, v4, v13",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",
                    "vmul.vv   v25, v5, v12", "vmulhu.vv v26, v5, v12",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",

                    // t2 = a0*b2 + a1*b1 + a2*b0 + a3*(19*b4) + a4*(19*b3)
                    "vmul.vv   v19, v1, v8",
                    "vmulhu.vv v20, v1, v8",
                    "vmul.vv   v25, v2, v7",  "vmulhu.vv v26, v2, v7",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",
                    "vmul.vv   v25, v3, v6",  "vmulhu.vv v26, v3, v6",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",
                    "vmul.vv   v25, v4, v14", "vmulhu.vv v26, v4, v14",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",
                    "vmul.vv   v25, v5, v13", "vmulhu.vv v26, v5, v13",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",

                    // t3 = a0*b3 + a1*b2 + a2*b1 + a3*b0 + a4*(19*b4)
                    "vmul.vv   v21, v1, v9",
                    "vmulhu.vv v22, v1, v9",
                    "vmul.vv   v25, v2, v8",  "vmulhu.vv v26, v2, v8",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",
                    "vmul.vv   v25, v3, v7",  "vmulhu.vv v26, v3, v7",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",
                    "vmul.vv   v25, v4, v6",  "vmulhu.vv v26, v4, v6",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",
                    "vmul.vv   v25, v5, v14", "vmulhu.vv v26, v5, v14",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",

                    // t4 = a0*b4 + a1*b3 + a2*b2 + a3*b1 + a4*b0
                    "vmul.vv   v23, v1, v10",
                    "vmulhu.vv v24, v1, v10",
                    "vmul.vv   v25, v2, v9",  "vmulhu.vv v26, v2, v9",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",
                    "vmul.vv   v25, v3, v8",  "vmulhu.vv v26, v3, v8",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",
                    "vmul.vv   v25, v4, v7",  "vmulhu.vv v26, v4, v7",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",
                    "vmul.vv   v25, v5, v6",  "vmulhu.vv v26, v5, v6",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",

                    // Sequential 51-bit carry reduction
                    "vmv.v.i   v28, 0",

                    "vsrl.vx   v25, v15, {sh51}", "vsll.vi v26, v16, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v1,  v15, {mask51}",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v28, v0",

                    "vsrl.vx   v25, v17, {sh51}", "vsll.vi v26, v18, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v2,  v17, {mask51}",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v28, v0",

                    "vsrl.vx   v25, v19, {sh51}", "vsll.vi v26, v20, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v3,  v19, {mask51}",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v28, v0",

                    "vsrl.vx   v25, v21, {sh51}", "vsll.vi v26, v22, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v4,  v21, {mask51}",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v28, v0",

                    "vsrl.vx   v25, v23, {sh51}", "vsll.vi v26, v24, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v5,  v23, {mask51}",
                    "vmul.vx   v25, v25, {c19}",
                    "vadd.vv   v1,  v1,  v25",
                    "vsrl.vx   v25, v1,  {sh51}",
                    "vand.vx   v1,  v1,  {mask51}",
                    "vadd.vv   v2,  v2,  v25",

                    "vse64.v   v1, ({o})",
                    "addi      {ta}, {o}, 128",
                    "vse64.v   v2, ({ta})",
                    "addi      {ta}, {o}, 256",
                    "vse64.v   v3, ({ta})",
                    "addi      {ta}, {o}, 384",
                    "vse64.v   v4, ({ta})",
                    "addi      {ta}, {o}, 512",
                    "vse64.v   v5, ({ta})",

                    a = in(reg) self.0.as_ptr(),
                    b = in(reg) rhs.0.as_ptr(),
                    o = in(reg) out.0.as_mut_ptr(),
                    c19 = in(reg) 19u64,
                    sh51 = in(reg) 51usize,
                    mask51 = in(reg) MASK51,
                    ta = out(reg) _,
                    tb = out(reg) _,
                    out("v0") _,
                    out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                    out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _,
                    out("v11") _, out("v12") _, out("v13") _, out("v14") _,
                    out("v15") _, out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                    out("v20") _, out("v21") _, out("v22") _, out("v23") _, out("v24") _,
                    out("v25") _, out("v26") _, out("v27") _, out("v28") _,
                    options(nostack),
                );
            }
            return out;
        }

        let mut out = Self::ZERO;
        for lane in 0..16 {
            let a0 = self.0[0][lane] as u128;
            let a1 = self.0[1][lane] as u128;
            let a2 = self.0[2][lane] as u128;
            let a3 = self.0[3][lane] as u128;
            let a4 = self.0[4][lane] as u128;

            let b0 = rhs.0[0][lane] as u128;
            let b1 = rhs.0[1][lane] as u128;
            let b2 = rhs.0[2][lane] as u128;
            let b3 = rhs.0[3][lane] as u128;
            let b4 = rhs.0[4][lane] as u128;

            let b1_19 = b1 * 19;
            let b2_19 = b2 * 19;
            let b3_19 = b3 * 19;
            let b4_19 = b4 * 19;

            let t0 = a0 * b0 + a1 * b4_19 + a2 * b3_19 + a3 * b2_19 + a4 * b1_19;
            let mut t1 = a0 * b1 + a1 * b0 + a2 * b4_19 + a3 * b3_19 + a4 * b2_19;
            let mut t2 = a0 * b2 + a1 * b1 + a2 * b0 + a3 * b4_19 + a4 * b3_19;
            let mut t3 = a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + a4 * b4_19;
            let mut t4 = a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0;

            let mut r0 = (t0 as u64) & MASK51;
            t1 += t0 >> 51;
            let mut r1 = (t1 as u64) & MASK51;
            t2 += t1 >> 51;
            let r2 = (t2 as u64) & MASK51;
            t3 += t2 >> 51;
            let r3 = (t3 as u64) & MASK51;
            t4 += t3 >> 51;
            let r4 = (t4 as u64) & MASK51;
            let c4 = (t4 >> 51) as u64;

            r0 += c4 * 19;
            r1 += r0 >> 51;
            r0 &= MASK51;

            out.0[0][lane] = r0;
            out.0[1][lane] = r1;
            out.0[2][lane] = r2;
            out.0[3][lane] = r3;
            out.0[4][lane] = r4;
        }
        out
    }

    #[inline]
    pub fn square(&self) -> Self {
        #[cfg(all(target_arch = "riscv64", feature = "simd"))]
        if crate::blake2b::has_vlen_1024() {
            let mut out = Self::ZERO;
            // SAFETY: `has_vlen_1024()` verified `vlenb >= 128` (`VL = 16` for `e64, m1`).
            // Symmetric 5-limb squaring uses 15 `vmul`/`vmulhu` pairs completely in registers.
            unsafe {
                core::arch::asm!(
                    "vsetivli zero, 16, e64, m1",
                    "vle64.v v1, ({a})",
                    "addi {ta}, {a}, 128",
                    "vle64.v v2, ({ta})",
                    "addi {ta}, {a}, 256",
                    "vle64.v v3, ({ta})",
                    "addi {ta}, {a}, 384",
                    "vle64.v v4, ({ta})",
                    "addi {ta}, {a}, 512",
                    "vle64.v v5, ({ta})",

                    "vadd.vv v6,  v1, v1",        // 2 * a0
                    "vadd.vv v7,  v2, v2",        // 2 * a1
                    "vmul.vx v8,  v5, {c19}",     // 19 * a4
                    "vmul.vx v9,  v4, {c19}",     // 19 * a3
                    "vmul.vx v11, v3, {c38}",     // 38 * a2
                    "vmul.vx v12, v4, {c38}",     // 38 * a3
                    "vmul.vx v13, v5, {c38}",     // 38 * a4

                    // t0 = a0*a0 + a1*(38*a4) + a2*(38*a3)
                    "vmul.vv   v15, v1, v1",
                    "vmulhu.vv v16, v1, v1",
                    "vmul.vv   v25, v2, v13", "vmulhu.vv v26, v2, v13",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",
                    "vmul.vv   v25, v3, v12", "vmulhu.vv v26, v3, v12",
                    "vadd.vv   v27, v15, v25", "vmsltu.vv v0, v27, v15", "vmv.v.v v15, v27", "vadc.vvm v16, v16, v26, v0",

                    // t1 = (2*a0)*a1 + a2*(38*a4) + a3*(19*a3)
                    "vmul.vv   v17, v6, v2",
                    "vmulhu.vv v18, v6, v2",
                    "vmul.vv   v25, v3, v13", "vmulhu.vv v26, v3, v13",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",
                    "vmul.vv   v25, v4, v9",  "vmulhu.vv v26, v4, v9",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v26, v0",

                    // t2 = (2*a0)*a2 + a1*a1 + a3*(38*a4)
                    "vmul.vv   v19, v6, v3",
                    "vmulhu.vv v20, v6, v3",
                    "vmul.vv   v25, v2, v2",  "vmulhu.vv v26, v2, v2",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",
                    "vmul.vv   v25, v4, v13", "vmulhu.vv v26, v4, v13",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v26, v0",

                    // t3 = (2*a0)*a3 + (2*a1)*a2 + a4*(19*a4)
                    "vmul.vv   v21, v6, v4",
                    "vmulhu.vv v22, v6, v4",
                    "vmul.vv   v25, v7, v3",  "vmulhu.vv v26, v7, v3",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",
                    "vmul.vv   v25, v5, v8",  "vmulhu.vv v26, v5, v8",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v26, v0",

                    // t4 = (2*a0)*a4 + (2*a1)*a3 + a2*a2
                    "vmul.vv   v23, v6, v5",
                    "vmulhu.vv v24, v6, v5",
                    "vmul.vv   v25, v7, v4",  "vmulhu.vv v26, v7, v4",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",
                    "vmul.vv   v25, v3, v3",  "vmulhu.vv v26, v3, v3",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v26, v0",

                    "vmv.v.i   v28, 0",
                    "vsrl.vx   v25, v15, {sh51}", "vsll.vi v26, v16, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v1,  v15, {mask51}",
                    "vadd.vv   v27, v17, v25", "vmsltu.vv v0, v27, v17", "vmv.v.v v17, v27", "vadc.vvm v18, v18, v28, v0",

                    "vsrl.vx   v25, v17, {sh51}", "vsll.vi v26, v18, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v2,  v17, {mask51}",
                    "vadd.vv   v27, v19, v25", "vmsltu.vv v0, v27, v19", "vmv.v.v v19, v27", "vadc.vvm v20, v20, v28, v0",

                    "vsrl.vx   v25, v19, {sh51}", "vsll.vi v26, v20, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v3,  v19, {mask51}",
                    "vadd.vv   v27, v21, v25", "vmsltu.vv v0, v27, v21", "vmv.v.v v21, v27", "vadc.vvm v22, v22, v28, v0",

                    "vsrl.vx   v25, v21, {sh51}", "vsll.vi v26, v22, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v4,  v21, {mask51}",
                    "vadd.vv   v27, v23, v25", "vmsltu.vv v0, v27, v23", "vmv.v.v v23, v27", "vadc.vvm v24, v24, v28, v0",

                    "vsrl.vx   v25, v23, {sh51}", "vsll.vi v26, v24, 13", "vor.vv v25, v25, v26",
                    "vand.vx   v5,  v23, {mask51}",
                    "vmul.vx   v25, v25, {c19}",
                    "vadd.vv   v1,  v1,  v25",
                    "vsrl.vx   v25, v1,  {sh51}",
                    "vand.vx   v1,  v1,  {mask51}",
                    "vadd.vv   v2,  v2,  v25",

                    "vse64.v   v1, ({o})",
                    "addi      {ta}, {o}, 128",
                    "vse64.v   v2, ({ta})",
                    "addi      {ta}, {o}, 256",
                    "vse64.v   v3, ({ta})",
                    "addi      {ta}, {o}, 384",
                    "vse64.v   v4, ({ta})",
                    "addi      {ta}, {o}, 512",
                    "vse64.v   v5, ({ta})",

                    a = in(reg) self.0.as_ptr(),
                    o = in(reg) out.0.as_mut_ptr(),
                    c19 = in(reg) 19u64,
                    c38 = in(reg) 38u64,
                    sh51 = in(reg) 51usize,
                    mask51 = in(reg) MASK51,
                    ta = out(reg) _,
                    out("v0") _,
                    out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
                    out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v11") _,
                    out("v12") _, out("v13") _,
                    out("v15") _, out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                    out("v20") _, out("v21") _, out("v22") _, out("v23") _, out("v24") _,
                    out("v25") _, out("v26") _, out("v27") _, out("v28") _,
                    options(nostack),
                );
            }
            return out;
        }

        let mut out = Self::ZERO;
        for lane in 0..16 {
            let a0 = self.0[0][lane] as u128;
            let a1 = self.0[1][lane] as u128;
            let a2 = self.0[2][lane] as u128;
            let a3 = self.0[3][lane] as u128;
            let a4 = self.0[4][lane] as u128;

            let a0_2 = a0 * 2;
            let a1_2 = a1 * 2;
            let a3_19 = a3 * 19;
            let a4_19 = a4 * 19;
            let a3_38 = a3 * 38;
            let a4_38 = a4 * 38;

            let t0 = a0 * a0 + a1 * a4_38 + a2 * a3_38;
            let mut t1 = a0_2 * a1 + a2 * a4_38 + a3 * a3_19;
            let mut t2 = a0_2 * a2 + a1 * a1 + a3 * a4_38;
            let mut t3 = a0_2 * a3 + a1_2 * a2 + a4 * a4_19;
            let mut t4 = a0_2 * a4 + a1_2 * a3 + a2 * a2;

            let mut r0 = (t0 as u64) & MASK51;
            t1 += t0 >> 51;
            let mut r1 = (t1 as u64) & MASK51;
            t2 += t1 >> 51;
            let r2 = (t2 as u64) & MASK51;
            t3 += t2 >> 51;
            let r3 = (t3 as u64) & MASK51;
            t4 += t3 >> 51;
            let r4 = (t4 as u64) & MASK51;
            let c4 = (t4 >> 51) as u64;

            r0 += c4 * 19;
            r1 += r0 >> 51;
            r0 &= MASK51;

            out.0[0][lane] = r0;
            out.0[1][lane] = r1;
            out.0[2][lane] = r2;
            out.0[3][lane] = r3;
            out.0[4][lane] = r4;
        }
        out
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

    #[test]
    fn test_fe51x16_and_batch_invert_16() {
        let a: [FieldElement; 16] = core::array::from_fn(|i| {
            FieldElement([
                0x75eb4dca135978a3u64.wrapping_add((i as u64) * 97),
                0x00700a4d4141d8abu64.wrapping_add((i as u64) * 31),
                0x8cc740797779e898u64,
                0x52036cee2b6ffe73u64 ^ (i as u64),
            ])
        });
        let b: [FieldElement; 16] = core::array::from_fn(|i| {
            FieldElement([
                0xc9562d608f25d51au64.wrapping_add((i as u64) * 53),
                0x692cc7609525a7b2u64,
                0xc0a4e231fdd6dc5cu64.wrapping_add((i as u64) * 19),
                0x216936d3cd6e53feu64,
            ])
        });

        let a16 = Fe51x16::from_fe_array(&a);
        let b16 = Fe51x16::from_fe_array(&b);

        let sum = a16.add(&b16).to_fe_array();
        let diff = a16.sub(&b16).to_fe_array();
        let prod = a16.mul(&b16).to_fe_array();
        let sq = a16.square().to_fe_array();

        let a_refs: [&FieldElement; 16] = core::array::from_fn(|i| &a[i]);
        let invs = FieldElement::batch_invert_16(a_refs);

        for i in 0..16 {
            assert_eq!(sum[i], a[i].add(&b[i]).reduce());
            assert_eq!(diff[i], a[i].sub(&b[i]).reduce());
            assert_eq!(prod[i], a[i].mul(&b[i]).reduce());
            assert_eq!(sq[i], a[i].square().reduce());
            assert_eq!(a[i].mul(&invs[i]).reduce(), FieldElement::ONE);
        }
    }
}
