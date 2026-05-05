use crate::blake2b;
use crate::curve25519::FieldElement;
use core::convert::TryInto;
use core::iter::Iterator;
use core::option::Option::{self, Some, None};

/// Point in Extended Coordinates (X, Y, Z, T)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: FieldElement,
    pub y: FieldElement,
    pub z: FieldElement,
    pub t: FieldElement,
}

const D: FieldElement = FieldElement([
    0x75eb4dca135978a3,
    0x00700a4d4141d8ab,
    0x8cc740797779e898,
    0x52036cee2b6ffe73,
]); // d = -121665/121666

const B: Point = Point {
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
    fn identity() -> Self {
        Point {
            x: FieldElement::ZERO,
            y: FieldElement::ONE,
            z: FieldElement::ONE,
            t: FieldElement::ZERO,
        }
    }

    fn add(&self, other: &Self) -> Self {
        let a = self.y.sub(&self.x).mul(&other.y.sub(&other.x));
        let b = self.y.add(&self.x).mul(&other.y.add(&other.x));
        let c = self.t.mul(&FieldElement([2, 0, 0, 0])).mul(&other.t).mul(&D);
        let d = self.z.mul(&FieldElement([2, 0, 0, 0])).mul(&other.z);
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

    fn double(&self) -> Self {
        self.add(self)
    }

    fn mul(&self, scalar: &[u8; 32]) -> Self {
        let mut res = Point::identity();
        let mut base = *self;
        for i in 0..256 {
            if (scalar[i / 8] >> (i % 8)) & 1 != 0 {
                res = res.add(&base);
            }
            base = base.double();
        }
        res
    }

    fn to_bytes(&self) -> [u8; 32] {
        let z_inv = self.z.invert();
        let x = self.x.mul(&z_inv).reduce();
        let y = self.y.mul(&z_inv).reduce();
        let mut bytes = y.to_bytes();
        if (x.0[0] & 1) != 0 {
            bytes[31] |= 0x80;
        }
        bytes
    }

    fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
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

        if (x.0[0] & 1) as u8 != x_bit {
            x = FieldElement::ZERO.sub(&x);
        }

        Some(Point {
            x,
            y,
            z: FieldElement::ONE,
            t: x.mul(&y),
        })
    }
}

/// Scalar arithmetic modulo L
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scalar([u64; 4]);

const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
];

impl Scalar {
    fn from_bytes_wide(bytes: &[u8; 64]) -> Self {
        let mut r = [0u64; 8];
        for i in 0..8 {
            r[i] = u64::from_le_bytes(bytes[i * 8..(i + 1) * 8].try_into().unwrap());
        }
        Self::reduce(&r)
    }

    fn reduce(r: &[u64; 8]) -> Self {
        let mut x = [0u64; 9];
        x[0..8].copy_from_slice(r);

        for i in (0..=260).rev() {
            let mut l_shifted = [0u64; 9];
            let s_limbs = i / 64;
            let s_bits = i % 64;
            let mut carry = 0u64;
            for j in 0..4 {
                l_shifted[j + s_limbs] = (L[j] << s_bits) | carry;
                carry = if s_bits > 0 { L[j] >> (64 - s_bits) } else { 0 };
            }
            if 4 + s_limbs < 9 {
                l_shifted[4 + s_limbs] = carry;
            }

            while Self::ge_wide(&x, &l_shifted) {
                let mut borrow = 0u64;
                for j in 0..9 {
                    let (v1, b1) = x[j].overflowing_sub(l_shifted[j]);
                    let (v2, b2) = v1.overflowing_sub(borrow);
                    x[j] = v2;
                    borrow = (b1 as u64) | (b2 as u64);
                }
            }
        }
        Scalar([x[0], x[1], x[2], x[3]])
    }

    fn ge_wide(a: &[u64; 9], b: &[u64; 9]) -> bool {
        for i in (0..9).rev() {
            if a[i] > b[i] { return true; }
            if a[i] < b[i] { return false; }
        }
        true
    }

    fn add(&self, other: &Self) -> Self {
        let mut r = [0u64; 8];
        let mut carry = 0u64;
        for i in 0..4 {
            let (sum, next_carry1) = self.0[i].overflowing_add(other.0[i]);
            let (sum, next_carry2) = sum.overflowing_add(carry);
            r[i] = sum;
            carry = (next_carry1 as u64) + (next_carry2 as u64);
        }
        r[4] = carry;
        Self::reduce(&r)
    }

    fn mul(&self, other: &Self) -> Self {
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
        Self::reduce(&r)
    }

    fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&self.0[i].to_le_bytes());
        }
        bytes
    }
}

pub fn sign(private_key: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let h = blake2b::hash64(private_key);

    let mut s_bytes = [0u8; 32];
    s_bytes.copy_from_slice(&h[..32]);
    s_bytes[0] &= 248;
    s_bytes[31] &= 63;
    s_bytes[31] |= 64;

    let public_key = B.mul(&s_bytes).to_bytes();

    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&h[32..64]);
    hasher.update(message);
    let r_hash = hasher.finalize();
    let r_scalar = Scalar::from_bytes_wide(&r_hash);
    let r_point = B.mul(&r_scalar.to_bytes()).to_bytes();

    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&r_point);
    hasher.update(&public_key);
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_scalar = Scalar::from_bytes_wide(&k_hash);

    // S = r + k*s (mod L)
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(&s_bytes);
    let s_scalar = Scalar::from_bytes_wide(&b);

    let s_signature = r_scalar.add(&k_scalar.mul(&s_scalar));

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_point);
    sig[32..].copy_from_slice(&s_signature.to_bytes());
    sig
}

pub fn derive_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let h = blake2b::hash64(private_key);
    let mut s = [0u8; 32];
    s.copy_from_slice(&h[..32]);
    s[0] &= 248;
    s[31] &= 63;
    s[31] |= 64;
    B.mul(&s).to_bytes()
}

pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let a = if let Some(p) = Point::from_bytes(public_key) { p } else { 
        #[cfg(test)] eprintln!("verify: failed to parse public key");
        return false 
    };
    let r = if let Some(p) = Point::from_bytes(&signature[..32].try_into().unwrap()) { p } else { 
        #[cfg(test)] eprintln!("verify: failed to parse R");
        return false 
    };

    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&signature[..32]);
    hasher.update(public_key);
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_scalar = Scalar::from_bytes_wide(&k_hash);
    let k_bytes = k_scalar.to_bytes();

    let sb = B.mul(&signature[32..].try_into().unwrap());
    let ka = a.mul(&k_bytes);
    let rka = r.add(&ka);

    let sb_bytes = sb.to_bytes();
    let rka_bytes = rka.to_bytes();
    
    if sb_bytes != rka_bytes {
        false
    } else {
        true
    }
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
        let priv_key = hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let expected_pub = hex_to_32("78e65bf30f893d32fc57ef051c341bdede242544fc2a2112f0fa2c7afdebc02f");
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
        let priv_key = hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pub_key = derive_public_key(&priv_key);
        let message = b"hello riscv";

        let signature = sign(&priv_key, message);
        assert!(verify(&pub_key, message, &signature));
    }

    #[test]
    fn test_nano_blake2b_vector() {
        // Test Case generated with Python hashlib.blake2b
        let priv_key = hex_to_32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let message = b"hello riscv";
        let expected_sig = hex_to_64("d9c55a774266ff9affac714049a50befaada8f587b79b2fd2436746a68eaace24384b84a4c721d3c5ca5f9456dca44739d09f6cc12467932fb9a24455e3f3b04");

        let pub_key = derive_public_key(&priv_key);
        assert_eq!(pub_key, hex_to_32("78e65bf30f893d32fc57ef051c341bdede242544fc2a2112f0fa2c7afdebc02f"));

        assert!(verify(&pub_key, message, &expected_sig), "Verification of Nano Blake2b signature failed");

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
    fn test_scalar_reduce_l() {
        let mut l_wide = [0u64; 8];
        l_wide[0..4].copy_from_slice(&L);
        let s = Scalar::reduce(&l_wide);
        assert_eq!(s.0, [0, 0, 0, 0]);
    }
}
