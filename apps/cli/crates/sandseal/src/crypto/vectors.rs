//! The Rust half of the cross-platform crypto contract.
//!
//! Both implementations of Sandseal's E2E crypto — this one and the TypeScript
//! one in `apps/web/src/lib/crypto` — are checked against the same fixed values
//! in `test-vectors.json`. Neither side simulates the other: a Rust test that
//! paired a Rust initiator with a Rust joiner would pass even if the browser had
//! drifted, which is exactly the failure worth catching. Agreeing with the
//! fixtures is what makes the two halves interoperate.
//!
//! The TypeScript half lives in `apps/web/src/lib/crypto/__tests__/vectors.test.ts`
//! and reads this same file.

use serde_json::Value;

use crate::crypto::encrypt;

const VECTORS_JSON: &str = include_str!("../../../../../../test-vectors.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS_JSON).expect("test-vectors.json is not valid JSON")
}

fn field<'a>(vectors: &'a Value, group: &str, key: &str) -> &'a str {
    vectors[group][key]
        .as_str()
        .unwrap_or_else(|| panic!("test-vectors.json is missing {group}.{key}"))
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "hex string has an odd length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex string has a non-hex digit"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn hkdf_matches_vector() {
    let v = vectors();
    let ikm = unhex(field(&v, "hkdf", "ikm_hex"));
    let salt = unhex(field(&v, "hkdf", "salt_hex"));
    let info = unhex(field(&v, "hkdf", "info_hex"));
    let expected = field(&v, "hkdf", "expected_hex");

    // The fixture is RFC 5869 Test Case 1, whose OKM is 42 bytes, while
    // hkdf_derive always returns 32. HKDF-Expand is a stream, so the short
    // output is a prefix of the long one — compare against that prefix.
    let okm = encrypt::hkdf_derive(&ikm, Some(&salt), &info);
    assert_eq!(hex(&okm), expected[..64]);
}

#[test]
fn argon2id_matches_vector() {
    let v = vectors();
    let password = unhex(field(&v, "argon2id", "password_hex"));
    let salt = unhex(field(&v, "argon2id", "salt_hex"));

    // If this fails after touching derive_key_from_password, check its Params
    // against argon2id.{t,m,p,dkLen} in the fixture before touching the fixture.
    let key = encrypt::derive_key_from_password(&password, &salt).unwrap();
    assert_eq!(hex(&key), field(&v, "argon2id", "expected_hex"));
}

#[test]
fn x25519_matches_vector() {
    let v = vectors();
    let alice_bytes: [u8; 32] = unhex(field(&v, "x25519", "alice_private_hex"))
        .try_into()
        .expect("alice_private_hex is not 32 bytes");
    let bob_bytes: [u8; 32] = unhex(field(&v, "x25519", "bob_private_hex"))
        .try_into()
        .expect("bob_private_hex is not 32 bytes");
    let expected = field(&v, "x25519", "expected_shared_hex");

    let alice = x25519_dalek::StaticSecret::from(alice_bytes);
    let bob = x25519_dalek::StaticSecret::from(bob_bytes);

    let from_alice = alice.diffie_hellman(&x25519_dalek::PublicKey::from(&bob));
    let from_bob = bob.diffie_hellman(&x25519_dalek::PublicKey::from(&alice));

    assert_eq!(hex(from_alice.as_bytes()), expected);
    assert_eq!(hex(from_bob.as_bytes()), expected);
}

#[test]
fn session_keys_match_vector() {
    let v = vectors();
    let shared = unhex(field(&v, "session_keys", "shared_hex"));
    let info = field(&v, "session_keys", "info");

    // Same derivation SessionKeys::from_key_exchange performs once the X25519
    // exchange has produced a shared secret, which is what the browser mirrors.
    let (key_a, key_b) = encrypt::hkdf_derive_pair(&shared, None, info.as_bytes());

    assert_eq!(hex(&key_a), field(&v, "session_keys", "expected_send_hex"));
    assert_eq!(hex(&key_b), field(&v, "session_keys", "expected_recv_hex"));
}

#[test]
fn chacha20poly1305_roundtrip_matches_vector() {
    let v = vectors();
    let key: [u8; 32] = unhex(field(&v, "chacha20poly1305", "key_hex"))
        .try_into()
        .expect("key_hex is not 32 bytes");
    let nonce: [u8; 12] = unhex(field(&v, "chacha20poly1305", "nonce_hex"))
        .try_into()
        .expect("nonce_hex is not 12 bytes");
    let plaintext = field(&v, "chacha20poly1305", "plaintext");

    // Only a roundtrip: the framing around the AEAD output differs between the
    // two implementations, so the ciphertext itself is not a shared fixture.
    let ciphertext = encrypt::encrypt_with_nonce(&key, &nonce, plaintext.as_bytes()).unwrap();
    let decrypted = encrypt::decrypt_with_nonce(&key, &nonce, &ciphertext).unwrap();

    assert_eq!(decrypted, plaintext.as_bytes());
}
