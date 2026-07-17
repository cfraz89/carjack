use num_bigint::{BigUint, RandBigInt};
use num_traits::Zero;
use ring::digest::{digest, SHA512};

// RFC 3526 Group 16 - 3072-bit MODP Prime
const N_HEX: &str = "\
    FFFFFFFF FFFFFFFF C90FDAA2 2168C234 C4C6628B 80DC1CD1 \
    29024E08 8A67CC74 020BBEA6 3B139B22 514A0879 8E3404DD \
    EF9519B3 CD3A431B 302B0A6D F25F1437 4FE1356D 6D51C245 \
    E485B576 625E7EC6 F44C42E9 A637ED6B 0BFF5CB6 F406B7ED \
    EE386BFB 5A899FA5 AE9F2411 7C4B1FE6 49286651 ECE45B3D \
    C2007CB8 A163BF05 98DA4836 1C55D39A 69163FA8 FD24CF5F \
    83655D23 DCA3AD96 1C62F356 208552BB 9ED52907 7096966D \
    670C354E 4ABC9804 F1746C08 CA18217C 32905E46 2E36CE3B \
    E39E772C 180E8603 9B2783A2 EC07A28F B5C55DF0 6F4C52C9 \
    DE2BCBF6 95581718 3995497C EA956AE5 15D22618 98FA0510 \
    15728E5A 8AACAA68 FFFFFFFF FFFFFFFF";

pub const SRP_BYTES: usize = 384;

fn sha512(data: &[u8]) -> Vec<u8> {
    digest(&SHA512, data).as_ref().to_vec()
}

pub fn tlv8_parse(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        let tag = data[i];
        let len = data[i + 1] as usize;
        i += 2;
        if i + len > data.len() {
            break;
        }
        entries.push((tag, data[i..i + len].to_vec()));
        i += len;
    }
    entries
}

pub fn tlv8_build(entries: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut result = Vec::new();
    for (tag, value) in entries {
        for chunk in value.chunks(255) {
            result.push(*tag);
            result.push(chunk.len() as u8);
            result.extend_from_slice(chunk);
        }
    }
    result
}

fn get_n() -> BigUint {
    BigUint::parse_bytes(
        N_HEX.replace(' ', "").as_bytes(),
        16,
    )
    .unwrap()
}

fn get_g() -> BigUint {
    BigUint::from(5u32)
}

fn biguint_to_bytes_padded(num: &BigUint, size: usize) -> Vec<u8> {
    let mut bytes = num.to_bytes_be();
    if bytes.len() < size {
        let mut padded = vec![0u8; size - bytes.len()];
        padded.append(&mut bytes);
        padded
    } else {
        bytes
    }
}

// Combine multiple TLV8 entries with the same tag into one byte vector
pub fn tlv8_combine(entries: &[(u8, Vec<u8>)], tag: u8) -> Vec<u8> {
    let mut result = Vec::new();
    for (t, data) in entries {
        if *t == tag {
            result.extend_from_slice(data);
        }
    }
    result
}

// Extract a single TLV8 value (first entry with given tag)
pub fn tlv8_get(entries: &[(u8, Vec<u8>)], tag: u8) -> Option<Vec<u8>> {
    entries.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.clone())
}

// x = H(salt || H(I || ":" || p))
pub fn compute_x(salt: &[u8], username: &str, pin: &str) -> BigUint {
    let inner = sha512(format!("{}:{}", username, pin).as_bytes());
    let mut input = Vec::new();
    input.extend_from_slice(salt);
    input.extend_from_slice(&inner);
    BigUint::from_bytes_be(&sha512(&input))
}

// k = H(pad(N) || pad(g))
pub fn compute_k() -> BigUint {
    let n = get_n();
    let g = get_g();
    let n_padded = biguint_to_bytes_padded(&n, SRP_BYTES);
    let g_padded = biguint_to_bytes_padded(&g, SRP_BYTES);
    let mut input = Vec::new();
    input.extend_from_slice(&n_padded);
    input.extend_from_slice(&g_padded);
    BigUint::from_bytes_be(&sha512(&input))
}

// Generate random private key a in [1, N)
pub fn generate_private_key() -> BigUint {
    let mut rng = rand::rngs::OsRng;
    let n = get_n();
    loop {
        let a = rng.gen_biguint_below(&n);
        if !a.is_zero() {
            return a;
        }
    }
}

// A = g^a mod N
pub fn compute_public_key(a: &BigUint) -> BigUint {
    get_g().modpow(a, &get_n())
}

// u = H(pad(A) || pad(B))
pub fn compute_u(A: &BigUint, B: &BigUint) -> BigUint {
    let a_bytes = biguint_to_bytes_padded(A, SRP_BYTES);
    let b_bytes = biguint_to_bytes_padded(B, SRP_BYTES);
    let mut input = Vec::new();
    input.extend_from_slice(&a_bytes);
    input.extend_from_slice(&b_bytes);
    BigUint::from_bytes_be(&sha512(&input))
}

// S = (B - k*v) ^ (a + u*x) mod N
pub fn compute_S(B: &BigUint, k: &BigUint, v: &BigUint, a: &BigUint, u: &BigUint, x: &BigUint) -> BigUint {
    let n = get_n();
    let kv = (k * v) % &n;
    let base = if *B >= kv { B - &kv } else { &n - (&kv - B) };
    let base = &base % &n;
    let exponent = a + u * x;
    base.modpow(&exponent, &n)
}

// K = H(pad(S))
pub fn compute_K(S: &BigUint) -> Vec<u8> {
    sha512(&biguint_to_bytes_padded(S, SRP_BYTES))
}

// M1 = H(H(N) XOR H(g) || H(salt) || H(A) || H(B) || K)
pub fn compute_M1(
    N_bytes: &[u8],
    g_bytes: &[u8],
    salt: &[u8],
    A_bytes: &[u8],
    B_bytes: &[u8],
    K: &[u8],
) -> Vec<u8> {
    let hn = sha512(N_bytes);
    let hg = sha512(g_bytes);
    let hnxhg: Vec<u8> = hn.iter().zip(hg.iter()).map(|(a, b)| a ^ b).collect();
    let hsalt = sha512(salt);
    let hA = sha512(A_bytes);
    let hB = sha512(B_bytes);
    let mut input = Vec::new();
    input.extend_from_slice(&hnxhg);
    input.extend_from_slice(&hsalt);
    input.extend_from_slice(&hA);
    input.extend_from_slice(&hB);
    input.extend_from_slice(K);
    sha512(&input)
}

// M2 = H(H(N) XOR H(g) || H(salt) || H(A) || M1 || K)
pub fn compute_M2(
    N_bytes: &[u8],
    g_bytes: &[u8],
    salt: &[u8],
    A_bytes: &[u8],
    M1: &[u8],
    K: &[u8],
) -> Vec<u8> {
    let hn = sha512(N_bytes);
    let hg = sha512(g_bytes);
    let hnxhg: Vec<u8> = hn.iter().zip(hg.iter()).map(|(a, b)| a ^ b).collect();
    let hsalt = sha512(salt);
    let hA = sha512(A_bytes);
    let mut input = Vec::new();
    input.extend_from_slice(&hnxhg);
    input.extend_from_slice(&hsalt);
    input.extend_from_slice(&hA);
    input.extend_from_slice(M1);
    input.extend_from_slice(K);
    sha512(&input)
}

// Full SRP computation: given salt and B from M2, returns (A_padded, M1, expected_M2, K)
pub fn do_srp(salt: &[u8], B_bytes: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let n = get_n();
    let g = get_g();
    let k = compute_k();
    let B = BigUint::from_bytes_be(B_bytes);
    let x = compute_x(salt, "CarPlay", "3939");
    let v = g.modpow(&x, &n);
    let a = generate_private_key();
    let A = compute_public_key(&a);
    let u = compute_u(&A, &B);
    let S = compute_S(&B, &k, &v, &a, &u, &x);
    let K = compute_K(&S);

    let a_bytes_min = A.to_bytes_be();
    let b_bytes_min = B.to_bytes_be();
    let n_bytes = n.to_bytes_be();
    let g_bytes = g.to_bytes_be();

    let M1 = compute_M1(&n_bytes, &g_bytes, salt, &a_bytes_min, &b_bytes_min, &K);
    let M2 = compute_M2(&n_bytes, &g_bytes, salt, &a_bytes_min, &M1, &K);

    (
        biguint_to_bytes_padded(&A, SRP_BYTES),
        M1,
        M2,
        K,
    )
}

// Build pair-setup M1 body
pub fn pair_setup_m1() -> Vec<u8> {
    tlv8_build(&[(0x00, vec![0x00]), (0x06, vec![0x01])])
}

// Parse M2 response, returns (salt, server_public_key)
pub fn parse_m2(body: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let entries = tlv8_parse(body);
    let state = tlv8_get(&entries, 0x06).ok_or("Missing State in M2")?;
    if state.len() != 1 || state[0] != 2 {
        return Err("Unexpected state in M2");
    }
    let salt = tlv8_get(&entries, 0x02).ok_or("Missing Salt in M2")?;
    let pubkey = tlv8_combine(&entries, 0x03);
    if pubkey.is_empty() {
        return Err("Missing PublicKey in M2");
    }
    Ok((salt, pubkey))
}

// Build pair-setup M3 body
pub fn pair_setup_m3(A_padded: &[u8], M1: &[u8]) -> Vec<u8> {
    tlv8_build(&[
        (0x06, vec![0x03]),
        (0x03, A_padded.to_vec()),
        (0x04, M1.to_vec()),
    ])
}

// Parse M4 response, returns server proof
pub fn parse_m4(body: &[u8]) -> Result<Vec<u8>, &'static str> {
    let entries = tlv8_parse(body);
    let state = tlv8_get(&entries, 0x06).ok_or("Missing State in M4")?;
    if state.len() != 1 || state[0] != 4 {
        return Err("Unexpected state in M4");
    }
    let proof = tlv8_get(&entries, 0x04).ok_or("Missing Proof in M4")?;
    Ok(proof)
}
