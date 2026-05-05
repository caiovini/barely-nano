import hashlib

def hex_to_bytes(s):
    return bytes.fromhex(s)

r = hex_to_bytes("B0FD724D1B341C7FB117AC51EB6B8D0BD56F424E7188F31718321C8B0CAEC92A")
a = hex_to_bytes("E89208DD038FBB269987689621D52292AE9C35941A7484756ECCED92A65093BA")
m = hex_to_bytes("023B94B7D27B311666C8636954FE17F1FD2EAA97A8BAC27DE5084FBBD5C6B02C")

h = hashlib.blake2b(digest_size=64)
h.update(r)
h.update(a)
h.update(m)
k_hash = h.digest()

print(f"Expected k_hash: {k_hash.hex().upper()}")

l = 2**252 + 27742317777372353535851937790883648493
k_int = int.from_bytes(k_hash, 'little')
k_reduced = k_int % l

print(f"Expected k_scalar (int): {k_reduced}")
print(f"Expected k_scalar (hex): {hex(k_reduced)}")

# Convert to limbs (little-endian u64)
limbs = []
temp = k_reduced
for _ in range(4):
    limbs.append(temp & 0xFFFFFFFFFFFFFFFF)
    temp >>= 64

print(f"Expected k_scalar (limbs): {[hex(x) for x in limbs]}")
