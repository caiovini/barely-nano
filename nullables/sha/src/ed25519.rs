use crate::blake2b;
use crate::curve25519::FieldElement;
use core::convert::TryInto;
use core::iter::Iterator;
use core::option::Option::{self, None, Some};
use crate::base_table::{RADIX16_B, ODD_MULTIPLES_B};
use core::sync::atomic::{AtomicBool, Ordering};

struct TinyLock<T> {
    locked: AtomicBool,
    data: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for TinyLock<T> {}

impl<T> TinyLock<T> {
    const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: core::cell::UnsafeCell::new(data),
        }
    }

    fn try_with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut T) -> R,
    {
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            let res = f(unsafe { &mut *self.data.get() });
            self.locked.store(false, Ordering::Release);
            Some(res)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
struct SigningKeyCache {
    priv_key: [u8; 32],
    pub_key: [u8; 32],
    s_bytes: [u8; 32],
    h_prefix: [u8; 32],
    valid: bool,
}

static SIGN_KEY_CACHE: TinyLock<[SigningKeyCache; 2]> = TinyLock::new([
    SigningKeyCache { priv_key: [0; 32], pub_key: [0; 32], s_bytes: [0; 32], h_prefix: [0; 32], valid: false },
    SigningKeyCache { priv_key: [0; 32], pub_key: [0; 32], s_bytes: [0; 32], h_prefix: [0; 32], valid: false },
]);

/// Point in Extended Coordinates (X, Y, Z, T)
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: FieldElement,
    pub y: FieldElement,
    pub z: FieldElement,
    pub t: FieldElement,
}

/// Precomputed affine point in (Y+X, Y-X, 2dXY) coordinates for ultra-fast mixed additions
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrecomputedPoint {
    pub yplusx: FieldElement,
    pub yminusx: FieldElement,
    pub xy2d: FieldElement,
}

impl PrecomputedPoint {
    pub const IDENTITY: Self = PrecomputedPoint {
        yplusx: FieldElement::ONE,
        yminusx: FieldElement::ONE,
        xy2d: FieldElement::ZERO,
    };
}

/// Point in standard Projective Coordinates (X : Y : Z)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectivePoint {
    pub x: FieldElement,
    pub y: FieldElement,
    pub z: FieldElement,
}

impl ProjectivePoint {
    pub const IDENTITY: Self = ProjectivePoint {
        x: FieldElement::ZERO,
        y: FieldElement::ONE,
        z: FieldElement::ONE,
    };

    /// Dedicated point doubling: ProjectivePoint -> CompletedPoint.
    /// Exactly 4 squarings, 0 multiplications (Bernstein ref10 ge_p2_dbl).
    #[inline(always)]
    pub fn double(&self) -> CompletedPoint {
        let tr_x = self.x.square();
        let tr_z = self.y.square();
        let z2 = self.z.square();
        let tr_t = z2.add(&z2);
        let r_y_in = self.x.add(&self.y);
        let t0 = r_y_in.square();
        let r_y = tr_z.add(&tr_x);
        let r_z = tr_z.sub(&tr_x);
        let r_x = t0.sub(&r_y);
        let r_t = tr_t.sub(&r_z);
        CompletedPoint {
            x: r_x,
            y: r_y,
            z: r_z,
            t: r_t,
        }
    }

    #[inline(always)]
    pub fn to_bytes_with_z_inv(&self, z_inv: &FieldElement) -> [u8; 32] {
        let x = self.x.mul(z_inv).reduce();
        let y = self.y.mul(z_inv).reduce();
        let mut bytes = y.to_bytes();
        if (x.0[0] & 1) != 0 {
            bytes[31] |= 0x80;
        }
        bytes
    }

    #[inline(always)]
    pub fn to_bytes(&self) -> [u8; 32] {
        let z_inv = self.z.invert();
        self.to_bytes_with_z_inv(&z_inv)
    }

    #[inline(always)]
    pub fn equals_signature_r(&self, signature_r: &[u8; 32]) -> bool {
        let z_inv = self.z.invert();
        let y = self.y.mul(&z_inv).reduce();
        let y_bytes = y.to_bytes();
        if y_bytes[..31] != signature_r[..31] {
            return false;
        }
        if (y_bytes[31] & 0x7f) != (signature_r[31] & 0x7f) {
            return false;
        }
        let x = self.x.mul(&z_inv).reduce();
        let x_bit = (x.0[0] & 1) as u8;
        let expected_x_bit = signature_r[31] >> 7;
        x_bit == expected_x_bit
    }
}

/// Point in Completed Coordinates ((X : Z), (Y : T))
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletedPoint {
    pub x: FieldElement,
    pub y: FieldElement,
    pub z: FieldElement,
    pub t: FieldElement,
}

impl CompletedPoint {
    /// Convert CompletedPoint to ProjectivePoint (P2). Exactly 3 multiplications, 0 squarings.
    #[inline(always)]
    pub fn to_p2(&self) -> ProjectivePoint {
        ProjectivePoint {
            x: self.x.mul(&self.t),
            y: self.y.mul(&self.z),
            z: self.z.mul(&self.t),
        }
    }

    /// Convert CompletedPoint to Extended Point (P3). Exactly 4 multiplications, 0 squarings.
    #[inline(always)]
    pub fn to_p3(&self) -> Point {
        Point {
            x: self.x.mul(&self.t),
            y: self.y.mul(&self.z),
            z: self.z.mul(&self.t),
            t: self.x.mul(&self.y),
        }
    }
}

#[derive(Clone, Copy)]
pub struct PubkeyPointCache {
    pub key: [u8; 32],
    pub point: Point,
    pub neg_table_a: [PrecomputedPoint; 8],
    pub valid: bool,
}

const EMPTY_PUBKEY_CACHE: PubkeyPointCache = PubkeyPointCache {
    key: [0; 32],
    point: Point { x: FieldElement::ZERO, y: FieldElement::ONE, z: FieldElement::ONE, t: FieldElement::ZERO },
    neg_table_a: [PrecomputedPoint::IDENTITY; 8],
    valid: false,
};

static PUBKEY_POINT_CACHE: TinyLock<[PubkeyPointCache; 4]> = TinyLock::new([EMPTY_PUBKEY_CACHE; 4]);

impl PartialEq for Point {
    fn eq(&self, other: &Self) -> bool {
        // Compare normalized coordinates: x1/z1 == x2/z2 and y1/z1 == y2/z2
        // Equiv: x1*z2 == x2*z1 and y1*z2 == y2*z1
        let x1z2 = self.x.mul(&other.z);
        let x2z1 = other.x.mul(&self.z);
        let y1z2 = self.y.mul(&other.z);
        let y2z1 = other.y.mul(&self.z);
        x1z2 == x2z1 && y1z2 == y2z1
    }
}

impl Eq for Point {}

const D: FieldElement = FieldElement([
    0x75eb4dca135978a3,
    0x00700a4d4141d8ab,
    0x8cc740797779e898,
    0x52036cee2b6ffe73,
]); // d = -121665/121666

const D_TIMES_TWO: FieldElement = FieldElement([
    0xebd69b9426b2f159,
    0x00e0149a8283b156,
    0x198e80f2eef3d130,
    0x2406d9dc56dffce7,
]); // 2 * d mod p

pub const B: Point = Point {
    x: FieldElement([
        0xc9562d608f25d51a,
        0x692cc7609525a7b2,
        0xc0a4e231fdd6dc5c,
        0x216936d3cd6e53fe,
    ]),
    y: FieldElement([
        0x6666666666666658,
        0x6666666666666666,
        0x6666666666666666,
        0x6666666666666666,
    ]),
    z: FieldElement::ONE,
    t: FieldElement([
        0x6dde8ab3a5b7dda3,
        0x20f09f80775152f5,
        0x66ea4e8e64abe37d,
        0x67875f0fd78b7665,
    ]), // t = x*y
};

impl Point {
    pub fn identity() -> Self {
        Point {
            x: FieldElement::ZERO,
            y: FieldElement::ONE,
            z: FieldElement::ONE,
            t: FieldElement::ZERO,
        }
    }

    pub fn add(&self, other: &Self) -> Self {
        // Hisil et al. 2008: Extended Coordinates addition in 9M (vs 11M)
        let y1_minus_x1 = self.y.sub(&self.x);
        let y1_plus_x1 = self.y.add(&self.x);
        let y2_minus_x2 = other.y.sub(&other.x);
        let y2_plus_x2 = other.y.add(&other.x);

        let a = y1_minus_x1.mul(&y2_minus_x2);
        let b = y1_plus_x1.mul(&y2_plus_x2);
        let c = self.t.mul(&other.t).mul(&D_TIMES_TWO);
        let z1z2 = self.z.mul(&other.z);
        let d = z1z2.add(&z1z2);

        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);

        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    /// Mixed addition for when other.z == 1 (8M vs 10M).
    pub fn add_affine(&self, other: &Self) -> Self {
        let y1_minus_x1 = self.y.sub(&self.x);
        let y1_plus_x1 = self.y.add(&self.x);
        let y2_minus_x2 = other.y.sub(&other.x);
        let y2_plus_x2 = other.y.add(&other.x);

        let a = y1_minus_x1.mul(&y2_minus_x2);
        let b = y1_plus_x1.mul(&y2_plus_x2);
        let c = self.t.mul(&other.t).mul(&D_TIMES_TWO);
        let d = self.z.add(&self.z);

        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);

        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    #[inline(always)]
    pub fn neg(&self) -> Self {
        Point {
            x: FieldElement::ZERO.sub(&self.x),
            y: self.y,
            z: self.z,
            t: FieldElement::ZERO.sub(&self.t),
        }
    }

    #[inline(always)]
    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.neg())
    }

    #[inline(always)]
    pub fn sub_affine(&self, other: &Self) -> Self {
        self.add_affine(&other.neg())
    }

    /// Mixed addition of Extended Point (P3) and PrecomputedPoint: P3 + Precomp -> CompletedPoint.
    /// Exactly 3 multiplications, 0 squarings (Bernstein ref10 ge_madd).
    #[inline(always)]
    pub fn madd(&self, q: &PrecomputedPoint) -> CompletedPoint {
        let y_plus_x = self.y.add(&self.x);
        let y_minus_x = self.y.sub(&self.x);
        let a = y_plus_x.mul(&q.yplusx);
        let b = y_minus_x.mul(&q.yminusx);
        let c = q.xy2d.mul(&self.t);
        let d = self.z.add(&self.z);
        CompletedPoint {
            x: a.sub(&b),
            y: a.add(&b),
            z: d.add(&c),
            t: d.sub(&c),
        }
    }

    /// Mixed subtraction of Extended Point (P3) and PrecomputedPoint: P3 - Precomp -> CompletedPoint.
    /// Exactly 3 multiplications, 0 squarings (Bernstein ref10 ge_msub).
    #[inline(always)]
    pub fn msub(&self, q: &PrecomputedPoint) -> CompletedPoint {
        let y_plus_x = self.y.add(&self.x);
        let y_minus_x = self.y.sub(&self.x);
        let a = y_plus_x.mul(&q.yminusx);
        let b = y_minus_x.mul(&q.yplusx);
        let c = q.xy2d.mul(&self.t);
        let d = self.z.add(&self.z);
        CompletedPoint {
            x: a.sub(&b),
            y: a.add(&b),
            z: d.sub(&c),
            t: d.add(&c),
        }
    }

    pub fn double(&self) -> Self {
        // Dedicated point doubling for a = -1 (Ed25519) in Extended Coordinates (Hisil et al. 2008)
        let a = self.x.square();
        let b = self.y.square();
        let z2 = self.z.square();
        let c = z2.add(&z2); // 2 * Z1^2
        let xy = self.x.mul(&self.y);
        let e = xy.add(&xy); // 2 * X1 * Y1 (clean, avoids subtractions)
        let g = b.sub(&a);
        let f = g.sub(&c);
        let a_plus_b = a.add(&b);
        let h = FieldElement::ZERO.sub(&a_plus_b); // -(A + B)

        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    pub fn mul(&self, scalar: &[u8; 32]) -> Self {
        let mut res = Point::identity();
        let mut base = *self;
        let mut has_added = false;
        for i in 0..256 {
            if (scalar[i / 8] >> (i % 8)) & 1 != 0 {
                if !has_added {
                    res = base;
                    has_added = true;
                } else {
                    res = res.add(&base);
                }
            }
            if i < 255 {
                base = base.double();
            }
        }
        res
    }

    /// Fast scalar multiplication with the fixed basepoint B using precomputed 4-bit window table.
    /// Eliminates all point doublings and uses mixed affine additions (~60 additions average).
    pub fn mul_base(scalar: &[u8; 32]) -> Self {
        let mut res = Point::identity();
        let mut has_added = false;
        for j in 0..64 {
            let nibble = ((scalar[j / 2] >> ((j % 2) * 4)) & 0x0F) as usize;
            if nibble != 0 {
                let pt = &RADIX16_B[j][nibble - 1];
                if !has_added {
                    res = *pt;
                    has_added = true;
                } else {
                    res = res.add_affine(pt);
                }
            }
        }
        res
    }

    pub fn to_bytes_with_z_inv(&self, z_inv: &FieldElement) -> [u8; 32] {
        let x = self.x.mul(z_inv).reduce();
        let y = self.y.mul(z_inv).reduce();
        let mut bytes = y.to_bytes();
        if (x.0[0] & 1) != 0 {
            bytes[31] |= 0x80;
        }
        bytes
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let z_inv = self.z.invert();
        self.to_bytes_with_z_inv(&z_inv)
    }

    #[inline(always)]
    pub fn get_or_create_neg_table(bytes: &[u8; 32]) -> Option<[PrecomputedPoint; 8]> {
        let cached = PUBKEY_POINT_CACHE.try_with(|cache| {
            for entry in cache.iter() {
                if entry.valid && &entry.key == bytes {
                    return Some(entry.neg_table_a);
                }
            }
            None
        }).flatten();

        if let Some(table) = cached {
            return Some(table);
        }

        let pt = Self::from_bytes_uncached(bytes)?;

        // Precompute the 8 affine odd multiples of -A as PrecomputedPoint for ultra-fast verification
        let neg_a = Point {
            x: FieldElement::ZERO.sub(&pt.x),
            y: pt.y,
            z: pt.z,
            t: FieldElement::ZERO.sub(&pt.t),
        };
        let double_neg_a = neg_a.double();
        let mut proj_table = [neg_a; 8];
        for i in 0..7 {
            proj_table[i + 1] = proj_table[i].add(&double_neg_a);
        }
        let z_invs = FieldElement::batch_invert_8([
            &proj_table[0].z,
            &proj_table[1].z,
            &proj_table[2].z,
            &proj_table[3].z,
            &proj_table[4].z,
            &proj_table[5].z,
            &proj_table[6].z,
            &proj_table[7].z,
        ]);
        let mut precomp_table = [PrecomputedPoint::IDENTITY; 8];
        for i in 0..8 {
            let ax = proj_table[i].x.mul(&z_invs[i]);
            let ay = proj_table[i].y.mul(&z_invs[i]);
            precomp_table[i] = PrecomputedPoint {
                yplusx: ay.add(&ax),
                yminusx: ay.sub(&ax),
                xy2d: ax.mul(&ay).mul(&D_TIMES_TWO),
            };
        }

        let _ = PUBKEY_POINT_CACHE.try_with(|cache| {
            let idx = (bytes[0] as usize) % 4;
            cache[idx] = PubkeyPointCache {
                key: *bytes,
                point: pt,
                neg_table_a: precomp_table,
                valid: true,
            };
        });

        Some(precomp_table)
    }

    #[inline(always)]
    pub fn get_cached_neg_table(bytes: &[u8; 32]) -> Option<[PrecomputedPoint; 8]> {
        PUBKEY_POINT_CACHE.try_with(|cache| {
            for entry in cache.iter() {
                if entry.valid && &entry.key == bytes {
                    return Some(entry.neg_table_a);
                }
            }
            None
        }).flatten()
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
        let cached = PUBKEY_POINT_CACHE.try_with(|cache| {
            for entry in cache.iter() {
                if entry.valid && &entry.key == bytes {
                    return Some(entry.point);
                }
            }
            None
        }).flatten();

        if let Some(pt) = cached {
            return Some(pt);
        }

        let _ = Self::get_or_create_neg_table(bytes)?;

        PUBKEY_POINT_CACHE.try_with(|cache| {
            for entry in cache.iter() {
                if entry.valid && &entry.key == bytes {
                    return Some(entry.point);
                }
            }
            None
        }).flatten()
    }

    fn from_bytes_uncached(bytes: &[u8; 32]) -> Option<Self> {
        let mut y_bytes = *bytes;
        let x_bit = (y_bytes[31] >> 7) & 1;
        y_bytes[31] &= 0x7f;
        let y = FieldElement::from_bytes(&y_bytes);

        let y2 = y.square();
        let u = y2.sub(&FieldElement::ONE);
        let v = D.mul(&y2).add(&FieldElement::ONE);
        let v_inv = v.invert();
        let x2 = u.mul(&v_inv);

        let mut x = x2.pow_p38();
        if x.square() != x2 {
            x = x.mul(&FieldElement([
                0xc4ee1b274a0ea0b0,
                0x2f431806ad2fe478,
                0x2b4d00993dfbd7a7,
                0x2b8324804fc1df0b,
            ]));
        }

        if x.square() != x2 {
            return None;
        }

        let x_reduced = x.reduce();
        if (x_reduced.0[0] & 1) as u8 != x_bit {
            x = FieldElement::ZERO.sub(&x);
        }

        Some(Point {
            x,
            y,
            z: FieldElement::ONE,
            t: x.mul(&y),
        })
    }

    /// Scalar multiplication of an arbitrary point using a 4-bit window (Radix-16).
    pub fn mul_window4(&self, scalar: &[u8; 32]) -> Self {
        let mut table = [*self; 16];
        table[0] = Point::identity();
        if self.z == FieldElement::ONE {
            for i in 1..15 {
                table[i + 1] = table[i].add_affine(self);
            }
        } else {
            for i in 1..15 {
                table[i + 1] = table[i].add(self);
            }
        }

        let mut res = Point::identity();
        let mut has_added = false;
        for j in (0..64).rev() {
            let nibble = ((scalar[j / 2] >> ((j % 2) * 4)) & 0x0F) as usize;
            if has_added {
                res = res.double().double().double().double();
            }
            if nibble != 0 {
                if !has_added {
                    res = table[nibble];
                    has_added = true;
                } else {
                    res = res.add(&table[nibble]);
                }
            }
        }
        res
    }

    /// Interleaved double scalar multiplication computing s*B + k*A in a single shared doubling loop.
    /// Evaluates both scalars within at most 255 shared curve doublings using ref10 coordinate pipeline:
    /// - ProjectivePoint::double (4S, 0M)
    /// - CompletedPoint::to_p2 (3M)
    /// - CompletedPoint::to_p3 (4M)
    /// - Point::madd / Point::msub (3M)
    pub fn double_scalar_mul_base_and_point_with_table(
        s: &[u8; 32],
        table_a: &[PrecomputedPoint; 8],
        k: &[u8; 32],
    ) -> ProjectivePoint {
        let slide_a = sliding_window(k, 5);
        let slide_b = sliding_window(s, 7);

        let mut i = 255;
        while i > 0 && slide_a[i] == 0 && slide_b[i] == 0 {
            i -= 1;
        }

        let mut r = ProjectivePoint::IDENTITY;

        loop {
            let mut t = r.double();

            let sa = slide_a[i];
            if sa > 0 {
                let u = t.to_p3();
                t = u.madd(&table_a[(sa as usize) / 2]);
            } else if sa < 0 {
                let u = t.to_p3();
                t = u.msub(&table_a[((-sa) as usize) / 2]);
            }

            let sb = slide_b[i];
            if sb > 0 {
                let u = t.to_p3();
                t = u.madd(&ODD_MULTIPLES_B[(sb as usize) / 2]);
            } else if sb < 0 {
                let u = t.to_p3();
                t = u.msub(&ODD_MULTIPLES_B[((-sb) as usize) / 2]);
            }

            if i == 0 {
                return t.to_p2();
            }
            r = t.to_p2();
            i -= 1;
        }
    }

    /// Interleaved double scalar multiplication computing s*B + k*A in a single shared doubling loop.
    /// Dynamically constructs an 8-entry PrecomputedPoint table of odd multiples of A via batch inversion.
    pub fn double_scalar_mul_base_and_point(s: &[u8; 32], a: &Point, k: &[u8; 32]) -> ProjectivePoint {
        let mut table_a = [*a; 8];
        let double_a = a.double();
        for i in 0..7 {
            table_a[i + 1] = table_a[i].add(&double_a);
        }
        let z_invs = FieldElement::batch_invert_8([
            &table_a[0].z,
            &table_a[1].z,
            &table_a[2].z,
            &table_a[3].z,
            &table_a[4].z,
            &table_a[5].z,
            &table_a[6].z,
            &table_a[7].z,
        ]);
        let mut precomp_table = [PrecomputedPoint::IDENTITY; 8];
        for i in 0..8 {
            let ax = table_a[i].x.mul(&z_invs[i]);
            let ay = table_a[i].y.mul(&z_invs[i]);
            precomp_table[i] = PrecomputedPoint {
                yplusx: ay.add(&ax),
                yminusx: ay.sub(&ax),
                xy2d: ax.mul(&ay).mul(&D_TIMES_TWO),
            };
        }
        Self::double_scalar_mul_base_and_point_with_table(s, &precomp_table, k)
    }
}

/// Converts a 256-bit scalar into signed sliding-window representation (wNAF)
/// with odd non-zero digits in range [-(2^(w-1)-1), 2^(w-1)-1].
fn sliding_window(scalar: &[u8; 32], windowsize: usize) -> [i8; 256] {
    let mut r = [0i8; 256];
    for (i, &byte) in scalar.iter().enumerate() {
        let mut b = byte;
        let base = i * 8;
        for bit in 0..8 {
            r[base + bit] = (b & 1) as i8;
            b >>= 1;
        }
    }
    let m = ((1 << (windowsize - 1)) - 1) as i8;
    let max_b = (windowsize - 1).min(6);
    for j in 0..256 {
        if r[j] == 0 {
            continue;
        }
        let limit = (256 - 1 - j).min(max_b);
        for b in 1..=limit {
            let term = r[j + b] << b;
            if r[j] + term <= m {
                r[j] += term;
                r[j + b] = 0;
            } else if r[j] - term >= -m {
                r[j] -= term;
                for k in (j + b)..256 {
                    if r[k] == 0 {
                        r[k] = 1;
                        break;
                    }
                    r[k] = 0;
                }
            } else if r[j + b] != 0 {
                break;
            }
        }
    }
    r
}

const K_BOTTOM_21_BITS: i64 = 0x1fffff;

#[inline(always)]
fn load_3(b: &[u8]) -> i64 {
    (b[0] as i64) | ((b[1] as i64) << 8) | ((b[2] as i64) << 16)
}

#[inline(always)]
fn load_4(b: &[u8]) -> i64 {
    (b[0] as i64) | ((b[1] as i64) << 8) | ((b[2] as i64) << 16) | ((b[3] as i64) << 24)
}

/// Constant-time, branch-free reduction of 24 21-bit limbs modulo L (ref10 sc_reduce).
#[inline(always)]
fn sc_reduce_limbs(
    mut s0: i64, mut s1: i64, mut s2: i64, mut s3: i64, mut s4: i64, mut s5: i64,
    mut s6: i64, mut s7: i64, mut s8: i64, mut s9: i64, mut s10: i64, mut s11: i64,
    mut s12: i64, mut s13: i64, mut s14: i64, mut s15: i64, mut s16: i64, mut s17: i64,
    s18: i64, s19: i64, s20: i64, s21: i64, s22: i64, s23: i64,
) -> [u8; 32] {
    s11 += s23 * 666643; s12 += s23 * 470296; s13 += s23 * 654183; s14 -= s23 * 997805; s15 += s23 * 136657; s16 -= s23 * 683901;
    s10 += s22 * 666643; s11 += s22 * 470296; s12 += s22 * 654183; s13 -= s22 * 997805; s14 += s22 * 136657; s15 -= s22 * 683901;
    s9 += s21 * 666643; s10 += s21 * 470296; s11 += s21 * 654183; s12 -= s21 * 997805; s13 += s21 * 136657; s14 -= s21 * 683901;
    s8 += s20 * 666643; s9 += s20 * 470296; s10 += s20 * 654183; s11 -= s20 * 997805; s12 += s20 * 136657; s13 -= s20 * 683901;
    s7 += s19 * 666643; s8 += s19 * 470296; s9 += s19 * 654183; s10 -= s19 * 997805; s11 += s19 * 136657; s12 -= s19 * 683901;
    s6 += s18 * 666643; s7 += s18 * 470296; s8 += s18 * 654183; s9 -= s18 * 997805; s10 += s18 * 136657; s11 -= s18 * 683901;

    let carry6 = (s6 + (1 << 20)) >> 21; s7 += carry6; s6 -= carry6 * (1 << 21);
    let carry8 = (s8 + (1 << 20)) >> 21; s9 += carry8; s8 -= carry8 * (1 << 21);
    let carry10 = (s10 + (1 << 20)) >> 21; s11 += carry10; s10 -= carry10 * (1 << 21);
    let carry12 = (s12 + (1 << 20)) >> 21; s13 += carry12; s12 -= carry12 * (1 << 21);
    let carry14 = (s14 + (1 << 20)) >> 21; s15 += carry14; s14 -= carry14 * (1 << 21);
    let carry16 = (s16 + (1 << 20)) >> 21; s17 += carry16; s16 -= carry16 * (1 << 21);

    let carry7 = (s7 + (1 << 20)) >> 21; s8 += carry7; s7 -= carry7 * (1 << 21);
    let carry9 = (s9 + (1 << 20)) >> 21; s10 += carry9; s9 -= carry9 * (1 << 21);
    let carry11 = (s11 + (1 << 20)) >> 21; s12 += carry11; s11 -= carry11 * (1 << 21);
    let carry13 = (s13 + (1 << 20)) >> 21; s14 += carry13; s13 -= carry13 * (1 << 21);
    let carry15 = (s15 + (1 << 20)) >> 21; s16 += carry15; s15 -= carry15 * (1 << 21);

    s5 += s17 * 666643; s6 += s17 * 470296; s7 += s17 * 654183; s8 -= s17 * 997805; s9 += s17 * 136657; s10 -= s17 * 683901;
    s4 += s16 * 666643; s5 += s16 * 470296; s6 += s16 * 654183; s7 -= s16 * 997805; s8 += s16 * 136657; s9 -= s16 * 683901;
    s3 += s15 * 666643; s4 += s15 * 470296; s5 += s15 * 654183; s6 -= s15 * 997805; s7 += s15 * 136657; s8 -= s15 * 683901;
    s2 += s14 * 666643; s3 += s14 * 470296; s4 += s14 * 654183; s5 -= s14 * 997805; s6 += s14 * 136657; s7 -= s14 * 683901;
    s1 += s13 * 666643; s2 += s13 * 470296; s3 += s13 * 654183; s4 -= s13 * 997805; s5 += s13 * 136657; s6 -= s13 * 683901;
    s0 += s12 * 666643; s1 += s12 * 470296; s2 += s12 * 654183; s3 -= s12 * 997805; s4 += s12 * 136657; s5 -= s12 * 683901;

    let carry0 = (s0 + (1 << 20)) >> 21; s1 += carry0; s0 -= carry0 * (1 << 21);
    let carry2 = (s2 + (1 << 20)) >> 21; s3 += carry2; s2 -= carry2 * (1 << 21);
    let carry4 = (s4 + (1 << 20)) >> 21; s5 += carry4; s4 -= carry4 * (1 << 21);
    let carry6_b = (s6 + (1 << 20)) >> 21; s7 += carry6_b; s6 -= carry6_b * (1 << 21);
    let carry8_b = (s8 + (1 << 20)) >> 21; s9 += carry8_b; s8 -= carry8_b * (1 << 21);
    let carry10_b = (s10 + (1 << 20)) >> 21; s11 += carry10_b; s10 -= carry10_b * (1 << 21);

    let carry1 = (s1 + (1 << 20)) >> 21; s2 += carry1; s1 -= carry1 * (1 << 21);
    let carry3 = (s3 + (1 << 20)) >> 21; s4 += carry3; s3 -= carry3 * (1 << 21);
    let carry5 = (s5 + (1 << 20)) >> 21; s6 += carry5; s5 -= carry5 * (1 << 21);
    let carry7_b = (s7 + (1 << 20)) >> 21; s8 += carry7_b; s7 -= carry7_b * (1 << 21);
    let carry9_b = (s9 + (1 << 20)) >> 21; s10 += carry9_b; s9 -= carry9_b * (1 << 21);
    let carry11_b = (s11 + (1 << 20)) >> 21; let s12_final = carry11_b; s11 -= carry11_b * (1 << 21);

    s0 += s12_final * 666643; s1 += s12_final * 470296; s2 += s12_final * 654183; s3 -= s12_final * 997805; s4 += s12_final * 136657; s5 -= s12_final * 683901;

    let c0 = s0 >> 21; s1 += c0; s0 -= c0 * (1 << 21);
    let c1 = s1 >> 21; s2 += c1; s1 -= c1 * (1 << 21);
    let c2 = s2 >> 21; s3 += c2; s2 -= c2 * (1 << 21);
    let c3 = s3 >> 21; s4 += c3; s3 -= c3 * (1 << 21);
    let c4 = s4 >> 21; s5 += c4; s4 -= c4 * (1 << 21);
    let c5 = s5 >> 21; s6 += c5; s5 -= c5 * (1 << 21);
    let c6 = s6 >> 21; s7 += c6; s6 -= c6 * (1 << 21);
    let c7 = s7 >> 21; s8 += c7; s7 -= c7 * (1 << 21);
    let c8 = s8 >> 21; s9 += c8; s8 -= c8 * (1 << 21);
    let c9 = s9 >> 21; s10 += c9; s9 -= c9 * (1 << 21);
    let c10 = s10 >> 21; s11 += c10; s10 -= c10 * (1 << 21);
    let c11 = s11 >> 21; let s12_f2 = c11; s11 -= c11 * (1 << 21);

    s0 += s12_f2 * 666643; s1 += s12_f2 * 470296; s2 += s12_f2 * 654183; s3 -= s12_f2 * 997805; s4 += s12_f2 * 136657; s5 -= s12_f2 * 683901;

    let c0 = s0 >> 21; s1 += c0; s0 -= c0 * (1 << 21);
    let c1 = s1 >> 21; s2 += c1; s1 -= c1 * (1 << 21);
    let c2 = s2 >> 21; s3 += c2; s2 -= c2 * (1 << 21);
    let c3 = s3 >> 21; s4 += c3; s3 -= c3 * (1 << 21);
    let c4 = s4 >> 21; s5 += c4; s4 -= c4 * (1 << 21);
    let c5 = s5 >> 21; s6 += c5; s5 -= c5 * (1 << 21);
    let c6 = s6 >> 21; s7 += c6; s6 -= c6 * (1 << 21);
    let c7 = s7 >> 21; s8 += c7; s7 -= c7 * (1 << 21);
    let c8 = s8 >> 21; s9 += c8; s8 -= c8 * (1 << 21);
    let c9 = s9 >> 21; s10 += c9; s9 -= c9 * (1 << 21);
    let c10 = s10 >> 21; s11 += c10; s10 -= c10 * (1 << 21);

    let mut out = [0u8; 32];
    out[0] = (s0 >> 0) as u8;
    out[1] = (s0 >> 8) as u8;
    out[2] = ((s0 >> 16) | (s1 << 5)) as u8;
    out[3] = (s1 >> 3) as u8;
    out[4] = (s1 >> 11) as u8;
    out[5] = ((s1 >> 19) | (s2 << 2)) as u8;
    out[6] = (s2 >> 6) as u8;
    out[7] = ((s2 >> 14) | (s3 << 7)) as u8;
    out[8] = (s3 >> 1) as u8;
    out[9] = (s3 >> 9) as u8;
    out[10] = ((s3 >> 17) | (s4 << 4)) as u8;
    out[11] = (s4 >> 4) as u8;
    out[12] = (s4 >> 12) as u8;
    out[13] = ((s4 >> 20) | (s5 << 1)) as u8;
    out[14] = (s5 >> 7) as u8;
    out[15] = ((s5 >> 15) | (s6 << 6)) as u8;
    out[16] = (s6 >> 2) as u8;
    out[17] = (s6 >> 10) as u8;
    out[18] = ((s6 >> 18) | (s7 << 3)) as u8;
    out[19] = (s7 >> 5) as u8;
    out[20] = (s7 >> 13) as u8;
    out[21] = (s8 >> 0) as u8;
    out[22] = (s8 >> 8) as u8;
    out[23] = ((s8 >> 16) | (s9 << 5)) as u8;
    out[24] = (s9 >> 3) as u8;
    out[25] = (s9 >> 11) as u8;
    out[26] = ((s9 >> 19) | (s10 << 2)) as u8;
    out[27] = (s10 >> 6) as u8;
    out[28] = ((s10 >> 14) | (s11 << 7)) as u8;
    out[29] = (s11 >> 1) as u8;
    out[30] = (s11 >> 9) as u8;
    out[31] = (s11 >> 17) as u8;
    out
}

/// Constant-time, branch-free reduction of a 64-byte scalar modulo L (ref10 sc_reduce).
pub fn sc_reduce(s: &[u8; 64]) -> [u8; 32] {
    let s0 = K_BOTTOM_21_BITS & load_3(&s[0..]);
    let s1 = K_BOTTOM_21_BITS & (load_4(&s[2..]) >> 5);
    let s2 = K_BOTTOM_21_BITS & (load_3(&s[5..]) >> 2);
    let s3 = K_BOTTOM_21_BITS & (load_4(&s[7..]) >> 7);
    let s4 = K_BOTTOM_21_BITS & (load_4(&s[10..]) >> 4);
    let s5 = K_BOTTOM_21_BITS & (load_3(&s[13..]) >> 1);
    let s6 = K_BOTTOM_21_BITS & (load_4(&s[15..]) >> 6);
    let s7 = K_BOTTOM_21_BITS & (load_3(&s[18..]) >> 3);
    let s8 = K_BOTTOM_21_BITS & load_3(&s[21..]);
    let s9 = K_BOTTOM_21_BITS & (load_4(&s[23..]) >> 5);
    let s10 = K_BOTTOM_21_BITS & (load_3(&s[26..]) >> 2);
    let s11 = K_BOTTOM_21_BITS & (load_4(&s[28..]) >> 7);
    let s12 = K_BOTTOM_21_BITS & (load_4(&s[31..]) >> 4);
    let s13 = K_BOTTOM_21_BITS & (load_3(&s[34..]) >> 1);
    let s14 = K_BOTTOM_21_BITS & (load_4(&s[36..]) >> 6);
    let s15 = K_BOTTOM_21_BITS & (load_3(&s[39..]) >> 3);
    let s16 = K_BOTTOM_21_BITS & load_3(&s[42..]);
    let s17 = K_BOTTOM_21_BITS & (load_4(&s[44..]) >> 5);
    let s18 = K_BOTTOM_21_BITS & (load_3(&s[47..]) >> 2);
    let s19 = K_BOTTOM_21_BITS & (load_4(&s[49..]) >> 7);
    let s20 = K_BOTTOM_21_BITS & (load_4(&s[52..]) >> 4);
    let s21 = K_BOTTOM_21_BITS & (load_3(&s[55..]) >> 1);
    let s22 = K_BOTTOM_21_BITS & (load_4(&s[57..]) >> 6);
    let s23 = load_4(&s[60..]) >> 3;

    sc_reduce_limbs(s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11,
                    s12, s13, s14, s15, s16, s17, s18, s19, s20, s21, s22, s23)
}

/// Constant-time, branch-free computation of s = (a * b + c) mod L (ref10 sc_muladd).
pub fn sc_muladd(s: &mut [u8; 32], a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) {
    let a0 = K_BOTTOM_21_BITS & load_3(&a[0..]);
    let a1 = K_BOTTOM_21_BITS & (load_4(&a[2..]) >> 5);
    let a2 = K_BOTTOM_21_BITS & (load_3(&a[5..]) >> 2);
    let a3 = K_BOTTOM_21_BITS & (load_4(&a[7..]) >> 7);
    let a4 = K_BOTTOM_21_BITS & (load_4(&a[10..]) >> 4);
    let a5 = K_BOTTOM_21_BITS & (load_3(&a[13..]) >> 1);
    let a6 = K_BOTTOM_21_BITS & (load_4(&a[15..]) >> 6);
    let a7 = K_BOTTOM_21_BITS & (load_3(&a[18..]) >> 3);
    let a8 = K_BOTTOM_21_BITS & load_3(&a[21..]);
    let a9 = K_BOTTOM_21_BITS & (load_4(&a[23..]) >> 5);
    let a10 = K_BOTTOM_21_BITS & (load_3(&a[26..]) >> 2);
    let a11 = load_4(&a[28..]) >> 7;

    let b0 = K_BOTTOM_21_BITS & load_3(&b[0..]);
    let b1 = K_BOTTOM_21_BITS & (load_4(&b[2..]) >> 5);
    let b2 = K_BOTTOM_21_BITS & (load_3(&b[5..]) >> 2);
    let b3 = K_BOTTOM_21_BITS & (load_4(&b[7..]) >> 7);
    let b4 = K_BOTTOM_21_BITS & (load_4(&b[10..]) >> 4);
    let b5 = K_BOTTOM_21_BITS & (load_3(&b[13..]) >> 1);
    let b6 = K_BOTTOM_21_BITS & (load_4(&b[15..]) >> 6);
    let b7 = K_BOTTOM_21_BITS & (load_3(&b[18..]) >> 3);
    let b8 = K_BOTTOM_21_BITS & load_3(&b[21..]);
    let b9 = K_BOTTOM_21_BITS & (load_4(&b[23..]) >> 5);
    let b10 = K_BOTTOM_21_BITS & (load_3(&b[26..]) >> 2);
    let b11 = load_4(&b[28..]) >> 7;

    let c0 = K_BOTTOM_21_BITS & load_3(&c[0..]);
    let c1 = K_BOTTOM_21_BITS & (load_4(&c[2..]) >> 5);
    let c2 = K_BOTTOM_21_BITS & (load_3(&c[5..]) >> 2);
    let c3 = K_BOTTOM_21_BITS & (load_4(&c[7..]) >> 7);
    let c4 = K_BOTTOM_21_BITS & (load_4(&c[10..]) >> 4);
    let c5 = K_BOTTOM_21_BITS & (load_3(&c[13..]) >> 1);
    let c6 = K_BOTTOM_21_BITS & (load_4(&c[15..]) >> 6);
    let c7 = K_BOTTOM_21_BITS & (load_3(&c[18..]) >> 3);
    let c8 = K_BOTTOM_21_BITS & load_3(&c[21..]);
    let c9 = K_BOTTOM_21_BITS & (load_4(&c[23..]) >> 5);
    let c10 = K_BOTTOM_21_BITS & (load_3(&c[26..]) >> 2);
    let c11 = load_4(&c[28..]) >> 7;

    let mut s0 = c0 + a0 * b0;
    let mut s1 = c1 + a0 * b1 + a1 * b0;
    let mut s2 = c2 + a0 * b2 + a1 * b1 + a2 * b0;
    let mut s3 = c3 + a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0;
    let mut s4 = c4 + a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0;
    let mut s5 = c5 + a0 * b5 + a1 * b4 + a2 * b3 + a3 * b2 + a4 * b1 + a5 * b0;
    let mut s6 = c6 + a0 * b6 + a1 * b5 + a2 * b4 + a3 * b3 + a4 * b2 + a5 * b1 + a6 * b0;
    let mut s7 = c7 + a0 * b7 + a1 * b6 + a2 * b5 + a3 * b4 + a4 * b3 + a5 * b2 + a6 * b1 + a7 * b0;
    let mut s8 = c8 + a0 * b8 + a1 * b7 + a2 * b6 + a3 * b5 + a4 * b4 + a5 * b3 + a6 * b2 + a7 * b1 + a8 * b0;
    let mut s9 = c9 + a0 * b9 + a1 * b8 + a2 * b7 + a3 * b6 + a4 * b5 + a5 * b4 + a6 * b3 + a7 * b2 + a8 * b1 + a9 * b0;
    let mut s10 = c10 + a0 * b10 + a1 * b9 + a2 * b8 + a3 * b7 + a4 * b6 + a5 * b5 + a6 * b4 + a7 * b3 + a8 * b2 + a9 * b1 + a10 * b0;
    let mut s11 = c11 + a0 * b11 + a1 * b10 + a2 * b9 + a3 * b8 + a4 * b7 + a5 * b6 + a6 * b5 + a7 * b4 + a8 * b3 + a9 * b2 + a10 * b1 + a11 * b0;
    let mut s12 = a1 * b11 + a2 * b10 + a3 * b9 + a4 * b8 + a5 * b7 + a6 * b6 + a7 * b5 + a8 * b4 + a9 * b3 + a10 * b2 + a11 * b1;
    let mut s13 = a2 * b11 + a3 * b10 + a4 * b9 + a5 * b8 + a6 * b7 + a7 * b6 + a8 * b5 + a9 * b4 + a10 * b3 + a11 * b2;
    let mut s14 = a3 * b11 + a4 * b10 + a5 * b9 + a6 * b8 + a7 * b7 + a8 * b6 + a9 * b5 + a10 * b4 + a11 * b3;
    let mut s15 = a4 * b11 + a5 * b10 + a6 * b9 + a7 * b8 + a8 * b7 + a9 * b6 + a10 * b5 + a11 * b4;
    let mut s16 = a5 * b11 + a6 * b10 + a7 * b9 + a8 * b8 + a9 * b7 + a10 * b6 + a11 * b5;
    let mut s17 = a6 * b11 + a7 * b10 + a8 * b9 + a9 * b8 + a10 * b7 + a11 * b6;
    let mut s18 = a7 * b11 + a8 * b10 + a9 * b9 + a10 * b8 + a11 * b7;
    let mut s19 = a8 * b11 + a9 * b10 + a10 * b9 + a11 * b8;
    let mut s20 = a9 * b11 + a10 * b10 + a11 * b9;
    let mut s21 = a10 * b11 + a11 * b10;
    let mut s22 = a11 * b11;
    let mut s23 = 0i64;

    let carry0 = (s0 + (1 << 20)) >> 21; s1 += carry0; s0 -= carry0 * (1 << 21);
    let carry2 = (s2 + (1 << 20)) >> 21; s3 += carry2; s2 -= carry2 * (1 << 21);
    let carry4 = (s4 + (1 << 20)) >> 21; s5 += carry4; s4 -= carry4 * (1 << 21);
    let carry6 = (s6 + (1 << 20)) >> 21; s7 += carry6; s6 -= carry6 * (1 << 21);
    let carry8 = (s8 + (1 << 20)) >> 21; s9 += carry8; s8 -= carry8 * (1 << 21);
    let carry10 = (s10 + (1 << 20)) >> 21; s11 += carry10; s10 -= carry10 * (1 << 21);
    let carry12 = (s12 + (1 << 20)) >> 21; s13 += carry12; s12 -= carry12 * (1 << 21);
    let carry14 = (s14 + (1 << 20)) >> 21; s15 += carry14; s14 -= carry14 * (1 << 21);
    let carry16 = (s16 + (1 << 20)) >> 21; s17 += carry16; s16 -= carry16 * (1 << 21);
    let carry18 = (s18 + (1 << 20)) >> 21; s19 += carry18; s18 -= carry18 * (1 << 21);
    let carry20 = (s20 + (1 << 20)) >> 21; s21 += carry20; s20 -= carry20 * (1 << 21);
    let carry22 = (s22 + (1 << 20)) >> 21; s23 += carry22; s22 -= carry22 * (1 << 21);

    let carry1 = (s1 + (1 << 20)) >> 21; s2 += carry1; s1 -= carry1 * (1 << 21);
    let carry3 = (s3 + (1 << 20)) >> 21; s4 += carry3; s3 -= carry3 * (1 << 21);
    let carry5 = (s5 + (1 << 20)) >> 21; s6 += carry5; s5 -= carry5 * (1 << 21);
    let carry7 = (s7 + (1 << 20)) >> 21; s8 += carry7; s7 -= carry7 * (1 << 21);
    let carry9 = (s9 + (1 << 20)) >> 21; s10 += carry9; s9 -= carry9 * (1 << 21);
    let carry11 = (s11 + (1 << 20)) >> 21; s12 += carry11; s11 -= carry11 * (1 << 21);
    let carry13 = (s13 + (1 << 20)) >> 21; s14 += carry13; s13 -= carry13 * (1 << 21);
    let carry15 = (s15 + (1 << 20)) >> 21; s16 += carry15; s15 -= carry15 * (1 << 21);
    let carry17 = (s17 + (1 << 20)) >> 21; s18 += carry17; s17 -= carry17 * (1 << 21);
    let carry19 = (s19 + (1 << 20)) >> 21; s20 += carry19; s19 -= carry19 * (1 << 21);
    let carry21 = (s21 + (1 << 20)) >> 21; s22 += carry21; s21 -= carry21 * (1 << 21);

    *s = sc_reduce_limbs(s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11,
                         s12, s13, s14, s15, s16, s17, s18, s19, s20, s21, s22, s23);
}

/// Scalar arithmetic modulo L
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scalar(pub [u64; 4]);

pub const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
];

impl Scalar {
    pub fn from_bytes_wide(bytes: &[u8; 64]) -> Self {
        let red = sc_reduce(bytes);
        Self::from_bytes(&red)
    }

    pub fn from_bytes_wide_32(bytes: &[u8; 32]) -> Self {
        let mut wide = [0u8; 64];
        wide[..32].copy_from_slice(bytes);
        Self::from_bytes_wide(&wide)
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            limbs[i] = u64::from_le_bytes(bytes[i * 8..(i + 1) * 8].try_into().unwrap());
        }
        Scalar(limbs)
    }

    pub fn reduce(r: &[u64; 8]) -> Self {
        let mut bytes = [0u8; 64];
        for i in 0..8 {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&r[i].to_le_bytes());
        }
        Self::from_bytes_wide(&bytes)
    }

    pub fn add(&self, other: &Self) -> Self {
        let a = self.to_bytes();
        let b = other.to_bytes();
        let mut one = [0u8; 32];
        one[0] = 1;
        let mut res = [0u8; 32];
        sc_muladd(&mut res, &a, &one, &b);
        Self::from_bytes(&res)
    }

    pub fn mul(&self, other: &Self) -> Self {
        let a = self.to_bytes();
        let b = other.to_bytes();
        let zero = [0u8; 32];
        let mut res = [0u8; 32];
        sc_muladd(&mut res, &a, &b, &zero);
        Self::from_bytes(&res)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&self.0[i].to_le_bytes());
        }
        bytes
    }
}

pub fn sign(private_key: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let cached = SIGN_KEY_CACHE.try_with(|cache| {
        for entry in cache.iter() {
            if entry.valid && &entry.priv_key == private_key {
                return Some((entry.pub_key, entry.s_bytes, entry.h_prefix));
            }
        }
        None
    }).flatten();

    let (public_key, s_bytes, h_prefix) = if let Some(c) = cached {
        c
    } else {
        let h = blake2b::hash64(private_key);
        let mut s = [0u8; 32];
        s.copy_from_slice(&h[..32]);
        s[0] &= 248;
        s[31] &= 63;
        s[31] |= 64;
        let pk = Point::mul_base(&s).to_bytes();
        let mut prefix = [0u8; 32];
        prefix.copy_from_slice(&h[32..64]);

        let _ = SIGN_KEY_CACHE.try_with(|cache| {
            let idx = (private_key[0] as usize) % 2;
            cache[idx] = SigningKeyCache {
                priv_key: *private_key,
                pub_key: pk,
                s_bytes: s,
                h_prefix: prefix,
                valid: true,
            };
        });
        (pk, s, prefix)
    };

    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&h_prefix);
    hasher.update(message);
    let r_hash = hasher.finalize();
    let r_bytes = sc_reduce(&r_hash);
    let r_point = Point::mul_base(&r_bytes).to_bytes();

    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&r_point);
    hasher.update(&public_key);
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_bytes = sc_reduce(&k_hash);

    let mut s_bytes_out = [0u8; 32];
    sc_muladd(&mut s_bytes_out, &k_bytes, &s_bytes, &r_bytes);

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_point);
    sig[32..].copy_from_slice(&s_bytes_out);
    sig
}

pub fn derive_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let cached = SIGN_KEY_CACHE.try_with(|cache| {
        for entry in cache.iter() {
            if entry.valid && &entry.priv_key == private_key {
                return Some(entry.pub_key);
            }
        }
        None
    }).flatten();

    if let Some(pk) = cached {
        return pk;
    }

    let h = blake2b::hash64(private_key);
    let mut s = [0u8; 32];
    s.copy_from_slice(&h[..32]);
    s[0] &= 248;
    s[31] &= 63;
    s[31] |= 64;
    let pub_key = Point::mul_base(&s).to_bytes();

    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..64]);

    let _ = SIGN_KEY_CACHE.try_with(|cache| {
        let idx = (private_key[0] as usize) % 2;
        cache[idx] = SigningKeyCache {
            priv_key: *private_key,
            pub_key,
            s_bytes: s,
            h_prefix: prefix,
            valid: true,
        };
    });

    pub_key
}

pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let neg_table = if let Some(t) = Point::get_or_create_neg_table(public_key) {
        t
    } else {
        return false;
    };
    
    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&signature[..32]); // Native R
    hasher.update(public_key); // Native A
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_bytes = sc_reduce(&k_hash);
    
    let mut s_le = [0u8; 32];
    s_le.copy_from_slice(&signature[32..]);
    
    let sb_minus_ka = Point::double_scalar_mul_base_and_point_with_table(&s_le, &neg_table, &k_bytes);
    sb_minus_ka.equals_signature_r((&signature[..32]).try_into().unwrap())
}

pub fn verify_batch_4(
    public_keys: [&[u8; 32]; 4],
    messages: [&[u8]; 4],
    signatures: [&[u8; 64]; 4],
) -> [bool; 4] {
    let mut results = [false; 4];
    
    // Check if all messages have matching lengths and fit within our zero-allocation stack buffer limits
    let len0 = messages[0].len();
    let all_match = messages[1].len() == len0 && messages[2].len() == len0 && messages[3].len() == len0;
    
    if all_match && len0 <= 128 {
        let mut neg_tables = [[PrecomputedPoint::IDENTITY; 8]; 4];
        for i in 0..4 {
            if let Some(t) = Point::get_or_create_neg_table(public_keys[i]) {
                neg_tables[i] = t;
            } else {
                return results;
            }
        }
        
        // Zero-allocation stack buffer: each slice has size (64 + len0)
        let mut payloads = [[0u8; 64 + 128]; 4];
        for i in 0..4 {
            payloads[i][0..32].copy_from_slice(&signatures[i][..32]);
            payloads[i][32..64].copy_from_slice(public_keys[i]);
            payloads[i][64..64 + len0].copy_from_slice(messages[i]);
        }
        
        let mut payload_refs: [&[u8]; 4] = [&[]; 4];
        for i in 0..4 {
            payload_refs[i] = &payloads[i][..64 + len0];
        }
        
        let k_hashes = blake2b::hash64_batch_4(payload_refs);
        
        let mut sb_minus_ka = [ProjectivePoint::IDENTITY; 4];
        for i in 0..4 {
            let k_bytes = sc_reduce(&k_hashes[i]);
            
            let mut s_le = [0u8; 32];
            s_le.copy_from_slice(&signatures[i][32..]);
            
            sb_minus_ka[i] = Point::double_scalar_mul_base_and_point_with_table(&s_le, &neg_tables[i], &k_bytes);
        }

        let z_invs = FieldElement::batch_invert_4([
            &sb_minus_ka[0].z,
            &sb_minus_ka[1].z,
            &sb_minus_ka[2].z,
            &sb_minus_ka[3].z,
        ]);

        for i in 0..4 {
            results[i] = sb_minus_ka[i].to_bytes_with_z_inv(&z_invs[i]) == signatures[i][..32];
        }
    } else {
        // Safe sequential fallback
        for i in 0..4 {
            results[i] = verify(public_keys[i], messages[i], signatures[i]);
        }
    }
    
    results
}


#[cfg(test)]
mod tests {
    use super::*;
    use core::{assert, assert_eq};

    #[test]
    fn test_field_invert() {
        let one = FieldElement::ONE;
        let inv_one = one.invert();
        assert_eq!(inv_one.to_bytes(), one.to_bytes());

        let two = FieldElement([2, 0, 0, 0]);
        let inv_two = two.invert();
        let prod = two.mul(&inv_two).reduce();
        assert_eq!(prod.to_bytes(), one.to_bytes());
    }

    #[test]
    fn test_derive_public_key() {
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let expected_pub =
            hex_to_32("78e65bf30f893d32fc57ef051c341bdede242544fc2a2112f0fa2c7afdebc02f");
        let actual_pub = derive_public_key(&priv_key);
        assert_eq!(actual_pub, expected_pub);
    }

    #[test]
    fn test_point_identity() {
        let id = Point::identity();
        let p = B.add(&id);
        assert_eq!(p.to_bytes(), B.to_bytes());
    }

    #[test]
    fn test_point_double() {
        let b2 = B.add(&B);
        let b2_mul = B.mul(&[
            2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ]);
        assert_eq!(b2.to_bytes(), b2_mul.to_bytes());
    }

    #[test]
    fn test_sign_verify() {
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pub_key = derive_public_key(&priv_key);
        let message = b"hello riscv";

        let signature = sign(&priv_key, message);
        assert!(verify(&pub_key, message, &signature));
    }

    #[test]
    fn test_nano_blake2b_vector() {
        // Test Case generated with Python hashlib.blake2b
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let message = b"hello riscv";
        let expected_sig = hex_to_64("d9c55a774266ff9affac714049a50befaada8f587b79b2fd2436746a68eaace24384b84a4c721d3c5ca5f9456dca44739d09f6cc12467932fb9a24455e3f3b04");

        let pub_key = derive_public_key(&priv_key);
        assert_eq!(
            pub_key,
            hex_to_32("78e65bf30f893d32fc57ef051c341bdede242544fc2a2112f0fa2c7afdebc02f")
        );

        assert!(
            verify(&pub_key, message, &expected_sig),
            "Verification of Nano Blake2b signature failed"
        );

        let actual_sig = sign(&priv_key, message);
        assert_eq!(actual_sig, expected_sig);
    }

    fn hex_to_32(s: &str) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        bytes
    }

    fn hex_to_64(s: &str) -> [u8; 64] {
        let mut bytes = [0u8; 64];
        for i in 0..64 {
            bytes[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        bytes
    }

    #[test]
    fn test_point_encoding() {
        let b_bytes = B.to_bytes();
        let b2 = Point::from_bytes(&b_bytes).expect("Failed to parse B");
        assert_eq!(b2.to_bytes(), b_bytes);
    }

    #[test]
    fn test_nano_genesis_verification() {
        let public_key_hex = "E89208DD038FBB269987689621D52292AE9C35941A7484756ECCED92A65093BA";
        let signature_hex = "9F0C933C8ADE004D808EA1985FA746A7E95BA2A38F867640F53EC8F180BDFE9E2C1268DEAD7C2664F356E37ABA362BC58E46DBA03E523A7B5A19E4B6EB12BB02";
        let message_hex = "991CF190094C00F0B68E2E5F75F6BEE95A2E0BD93CEAA4A6734DB9F19B728948"; // Mathematically correct Open Block hash
        
        let public_key = hex_to_32(public_key_hex);
        let signature = hex_to_64(signature_hex);
        let message = hex_to_32(message_hex);
        
        assert!(verify(&public_key, &message, &signature));
    }

    #[test]
    fn test_scalar_reduce_l() {
        let mut l_wide = [0u64; 8];
        l_wide[0..4].copy_from_slice(&L);
        let s = Scalar::reduce(&l_wide);
        assert_eq!(s.0, [0, 0, 0, 0]);
    }

    #[test]
    fn test_b_mul_1() {
        let mut scalar = [0u8; 32];
        scalar[0] = 1;
        let p = B.mul(&scalar);
        assert_eq!(p.to_bytes(), B.to_bytes());
    }

    #[test]
    fn test_b_mul_2() {
        let mut scalar = [0u8; 32];
        scalar[0] = 2;
        let p1 = B.mul(&scalar);
        let p2 = B.double();
        assert_eq!(p1.to_bytes(), p2.to_bytes());
    }
    #[test]
    fn test_scalar_mul_large() {
        let mut l_minus_1 = L;
        l_minus_1[0] -= 1;
        let s = Scalar(l_minus_1);
        let prod = s.mul(&s);
        // (L-1)*(L-1) = L^2 - 2L + 1 \equiv 1 (mod L)
        assert_eq!(prod.0, [1, 0, 0, 0]);
    }

    /// Test that our Ed25519 produces a valid sign+verify round-trip
    /// for the exact V2 handshake payload: cookie || salt || genesis_hash
    #[test]
    fn test_v2_handshake_roundtrip() {
        // A known private key (32 bytes)
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pub_key = derive_public_key(&priv_key);

        // Simulate handshake payload: cookie (32) + salt (32) + genesis (32) = 96 bytes
        let cookie = [0x01u8; 32];
        let salt = [0x02u8; 32];
        let genesis_hash = hex_to_32(
            "991cf190094c00f0b68e2e5f75f6bee95a2e0bd93ceaa4a6734db9f19b728948",
        );

        let mut payload = [0u8; 96];
        payload[0..32].copy_from_slice(&cookie);
        payload[32..64].copy_from_slice(&salt);
        payload[64..96].copy_from_slice(&genesis_hash);

        let sig = sign(&priv_key, &payload);
        assert!(
            verify(&pub_key, &payload, &sig),
            "V2 handshake round-trip failed: sign+verify mismatch"
        );
    }

    /// Cross-check: produce a signature with our code, verify it matches
    /// what the Nano network expects by checking against a reference value
    /// generated offline with the reference ed25519-dalek (Blake2b) library.
    ///
    /// Reference was generated with:
    ///   priv = 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
    ///   msg  = "hello riscv"
    ///   using Nano's fork of ed25519-dalek (Blake2b based)
    #[test]
    fn test_reference_signature_cross_check() {
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pub_key = derive_public_key(&priv_key);
        assert_eq!(
            pub_key,
            hex_to_32("78e65bf30f893d32fc57ef051c341bdede242544fc2a2112f0fa2c7afdebc02f"),
            "Public key derivation mismatch — ed25519 scalar multiply is broken"
        );

        let message = b"hello riscv";
        let sig = sign(&priv_key, message);
        // Verify our own signature round-trips
        assert!(verify(&pub_key, message, &sig), "self-verify failed");

        // Cross-check against stored reference signature
        let reference_sig = hex_to_64(
            "d9c55a774266ff9affac714049a50befaada8f587b79b2fd2436746a68eaace24384b84a4c721d3c5ca5f9456dca44739d09f6cc12467932fb9a24455e3f3b04",
        );
        // If this fails the signing is not deterministic or differs from reference
        assert_eq!(
            sig, reference_sig,
            "Our signature differs from the reference Blake2b-based ed25519 signature"
        );
    }

    #[test]
    fn test_verify_batch_4() {
        let priv_key =
            hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pub_key = derive_public_key(&priv_key);
        let message0 = b"hello riscv 0";
        let message1 = b"hello riscv 1";
        let message2 = b"hello riscv 2";
        let message3 = b"hello riscv 3";
        
        let sig0 = sign(&priv_key, message0);
        let sig1 = sign(&priv_key, message1);
        let sig2 = sign(&priv_key, message2);
        let sig3 = sign(&priv_key, message3);
        
        let results = verify_batch_4(
            [&pub_key, &pub_key, &pub_key, &pub_key],
            [message0, message1, message2, message3],
            [&sig0, &sig1, &sig2, &sig3],
        );
        
        assert!(results[0]);
        assert!(results[1]);
        assert!(results[2]);
        assert!(results[3]);
    }
}

