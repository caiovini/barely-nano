use crate::blake2b;
use crate::curve25519::FieldElement;
use core::convert::TryInto;
use core::iter::Iterator;
use core::option::Option::{self, None, Some};

/// Point in Extended Coordinates (X, Y, Z, T)
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: FieldElement,
    pub y: FieldElement,
    pub z: FieldElement,
    pub t: FieldElement,
}

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
        // Standard addition formula for Extended Coordinates
        let x1y2 = self.x.mul(&other.y);
        let x2y1 = self.y.mul(&other.x);
        let y1y2 = self.y.mul(&other.y);
        let x1x2 = self.x.mul(&other.x);
        let z1z2 = self.z.mul(&other.z);
        let t1t2 = self.t.mul(&other.t);
        let dt1t2 = D.mul(&t1t2);

        let e = x1y2.add(&x2y1);
        let f = z1z2.sub(&dt1t2);
        let g = z1z2.add(&dt1t2);
        let h = y1y2.add(&x1x2);

        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    pub fn double(&self) -> Self {
        self.add(self)
    }

    pub fn mul(&self, scalar: &[u8; 32]) -> Self {
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

    pub fn to_bytes(&self) -> [u8; 32] {
        let z_inv = self.z.invert();
        let x = self.x.mul(&z_inv).reduce();
        let y = self.y.mul(&z_inv).reduce();
        let mut bytes = y.to_bytes();
        if (x.0[0] & 1) != 0 {
            bytes[31] |= 0x80;
        }
        bytes
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
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

    pub fn double_scalar_mul_base_and_point(s: &[u8; 32], a: &Point, k: &[u8; 32]) -> Self {
        let mut res = Point::identity();
        let b_plus_a = B.add(a);
        for i in (0..256).rev() {
            res = res.double();
            let bit1 = (s[i / 8] >> (i % 8)) & 1;
            let bit2 = (k[i / 8] >> (i % 8)) & 1;
            match (bit1, bit2) {
                (1, 1) => res = res.add(&b_plus_a),
                (1, 0) => res = res.add(&B),
                (0, 1) => res = res.add(a),
                _ => {}
            }
        }
        res
    }
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
        let mut r = [0u64; 8];
        for i in 0..8 {
            r[i] = u64::from_le_bytes(bytes[i * 8..(i + 1) * 8].try_into().unwrap());
        }
        Self::reduce(&r)
    }

    pub fn from_bytes_wide_32(bytes: &[u8; 32]) -> Self {
        let mut wide = [0u8; 64];
        wide[..32].copy_from_slice(bytes);
        Self::from_bytes_wide(&wide)
    }

    fn reduce(r: &[u64; 8]) -> Self {
        let mut x = [0u64; 9];
        for i in 0..8 {
            x[i] = r[i];
        }

        for i in (0..=260).rev() {
            let mut l_shifted = [0u64; 9];
            let s_limbs = i / 64;
            let s_bits = i % 64;
            if s_bits == 0 {
                for j in 0..4 {
                    l_shifted[j + s_limbs] = L[j];
                }
            } else {
                let mut carry = 0u64;
                for j in 0..4 {
                    l_shifted[j + s_limbs] = (L[j] << s_bits) | carry;
                    carry = L[j] >> (64 - s_bits);
                }
                if s_limbs + 4 < 9 {
                    l_shifted[s_limbs + 4] = carry;
                }
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
            if a[i] > b[i] {
                return true;
            }
            if a[i] < b[i] {
                return false;
            }
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

    pub fn to_bytes(&self) -> [u8; 32] {
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
    
    let pub_bytes = B.mul(&s_bytes).to_bytes();
    // Do not reverse
    let public_key = pub_bytes;
    
    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&h[32..64]);
    hasher.update(message);
    let r_hash = hasher.finalize();
    let r_scalar = Scalar::from_bytes_wide(&r_hash);
    let r_bytes = B.mul(&r_scalar.to_bytes()).to_bytes();
    // Do not reverse
    let r_point = r_bytes;
    
    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&r_point); // Native R
    hasher.update(&public_key); // Native A
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_scalar = Scalar::from_bytes_wide(&k_hash);
    
    // S = r + k*s (mod L)
    let s_val = Scalar::from_bytes_wide_32(&s_bytes);
    let s_scalar = r_scalar.add(&k_scalar.mul(&s_val));
    
    let s_bytes_out = s_scalar.to_bytes();
    // Do not reverse
    
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_point);
    sig[32..].copy_from_slice(&s_bytes_out);
    sig
}

pub fn derive_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let h = blake2b::hash64(private_key);
    let mut s = [0u8; 32];
    s.copy_from_slice(&h[..32]);
    s[0] &= 248;
    s[31] &= 63;
    s[31] |= 64;
    let pub_key = B.mul(&s).to_bytes();
    // Do not reverse
    pub_key
}

pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let a = if let Some(p) = Point::from_bytes(public_key) { p } else { return false };
    
    let mut r_le = [0u8; 32];
    r_le.copy_from_slice(&signature[..32]);
    let r = if let Some(p) = Point::from_bytes(&r_le) { p } else { return false };
    
    let mut hasher = blake2b::Blake2bHasher::new(64);
    hasher.update(&signature[..32]); // Native R
    hasher.update(public_key); // Native A
    hasher.update(message);
    let k_hash = hasher.finalize();
    let k_bytes = Scalar::from_bytes_wide(&k_hash).to_bytes();
    
    let mut s_le = [0u8; 32];
    s_le.copy_from_slice(&signature[32..]);
    // Do not reverse
    
    let neg_a = Point {
        x: FieldElement::ZERO.sub(&a.x),
        y: a.y,
        z: a.z,
        t: FieldElement::ZERO.sub(&a.t),
    };
    
    let sb_minus_ka = Point::double_scalar_mul_base_and_point(&s_le, &neg_a, &k_bytes);
    sb_minus_ka == r
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
        let mut points_a = [Point::identity(); 4];
        let mut points_r = [Point::identity(); 4];
        
        for i in 0..4 {
            let a = if let Some(p) = Point::from_bytes(public_keys[i]) { p } else { return results };
            let mut r_le = [0u8; 32];
            r_le.copy_from_slice(&signatures[i][..32]);
            let r = if let Some(p) = Point::from_bytes(&r_le) { p } else { return results };
            points_a[i] = a;
            points_r[i] = r;
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
        
        for i in 0..4 {
            let k_bytes = Scalar::from_bytes_wide(&k_hashes[i]).to_bytes();
            
            let mut s_le = [0u8; 32];
            s_le.copy_from_slice(&signatures[i][32..]);
            
            let neg_a = Point {
                x: FieldElement::ZERO.sub(&points_a[i].x),
                y: points_a[i].y,
                z: points_a[i].z,
                t: FieldElement::ZERO.sub(&points_a[i].t),
            };
            
            let sb_minus_ka = Point::double_scalar_mul_base_and_point(&s_le, &neg_a, &k_bytes);
            results[i] = sb_minus_ka == points_r[i];
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

