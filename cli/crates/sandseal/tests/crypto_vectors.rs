use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Nonce, generic_array::GenericArray},
};
use hkdf::Hkdf;
use sha2::Sha256;
use argon2::{Argon2, Algorithm, Version, Params};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// RFC 5869 Test Case 1 — deterministic, cross-platform verifiable.
#[test]
fn hkdf_sha256_rfc5869_test1() {
    let ikm = unhex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
    let salt = unhex("000102030405060708090a0b0c");
    let info = unhex("f0f1f2f3f4f5f6f7f8f9");

    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut okm = [0u8; 42];
    hk.expand(&info, &mut okm).unwrap();

    assert_eq!(
        hex(&okm),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );
}

/// ChaCha20-Poly1305 encrypt/decrypt roundtrip with fixed key+nonce.
#[test]
fn chacha20poly1305_roundtrip_deterministic() {
    let key = unhex("0000000000000000000000000000000000000000000000000000000000000001");
    let nonce = unhex("000000000000000000000002");
    let plaintext = b"Hello, Sandseal!";

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&key));
    let n = Nonce::<ChaCha20Poly1305>::from_slice(&nonce);

    let encrypted = cipher.encrypt(n, plaintext.as_ref()).unwrap();
    let ct_hex = hex(&encrypted);
    println!("chacha20poly1305_ciphertext_hex: {ct_hex}");

    let decrypted = cipher.decrypt(n, encrypted.as_ref()).unwrap();
    assert_eq!(decrypted, plaintext);
}

/// Argon2id with known inputs — output printed for JS verification.
#[test]
fn argon2id_deterministic() {
    let password = b"password";
    let salt = unhex("736f6d6573616c74736f6d6573616c74");

    let params = Params::new(65536, 3, 1, Some(32)).unwrap();
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; 32];
    argon2.hash_password_into(password, &salt, &mut key).unwrap();

    let key_hex = hex(&key);
    println!("argon2id_key_hex: {key_hex}");

    // Must be deterministic
    let mut key2 = [0u8; 32];
    argon2.hash_password_into(password, &salt, &mut key2).unwrap();
    assert_eq!(key, key2);
    assert_ne!(key, [0u8; 32]);
}

/// X25519 DH with fixed private keys — prints shared secret for JS verification.
#[test]
fn x25519_deterministic_exchange() {
    let alice_key: [u8; 32] = [
        0xa8, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
        0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
        0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
        0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
    ];
    let bob_key: [u8; 32] = [
        0xb8, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba,
        0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba,
        0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba,
        0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba, 0xba,
    ];

    let alice_secret = x25519_dalek::StaticSecret::from(alice_key);
    let bob_secret = x25519_dalek::StaticSecret::from(bob_key);

    let alice_public = x25519_dalek::PublicKey::from(&alice_secret);
    let bob_public = x25519_dalek::PublicKey::from(&bob_secret);

    let alice_shared = alice_secret.diffie_hellman(&bob_public);
    let bob_shared = bob_secret.diffie_hellman(&alice_public);

    assert_eq!(alice_shared.as_bytes(), bob_shared.as_bytes());

    println!("alice_public_hex: {}", hex(alice_public.as_bytes()));
    println!("bob_public_hex: {}", hex(bob_public.as_bytes()));
    println!("shared_secret_hex: {}", hex(alice_shared.as_bytes()));
}

/// HKDF derive pair for session keys — deterministic with fixed shared secret.
#[test]
fn hkdf_session_keys_deterministic() {
    let shared = unhex("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let info = b"sandseal-session-v1-gen0";

    let hk = Hkdf::<Sha256>::new(None, &shared);
    let mut okm = [0u8; 64];
    hk.expand(info, &mut okm).unwrap();

    println!("send_key_hex: {}", hex(&okm[..32]));
    println!("recv_key_hex: {}", hex(&okm[32..]));

    assert_ne!(&okm[..32], &okm[32..]);

    // Deterministic
    let hk2 = Hkdf::<Sha256>::new(None, &shared);
    let mut okm2 = [0u8; 64];
    hk2.expand(info, &mut okm2).unwrap();
    assert_eq!(okm, okm2);
}
