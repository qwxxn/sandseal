use anyhow::{Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Nonce, generic_array::GenericArray},
};
use hkdf::Hkdf;
use sha2::Sha256;
use rand_core_06::{RngCore, OsRng};
use zeroize::Zeroize;

pub const NONCE_SIZE: usize = 12;
pub const TAG_SIZE: usize = 16;
pub const KEY_SIZE: usize = 32;

pub fn hkdf_derive(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> [u8; KEY_SIZE] {
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    let mut okm = [0u8; KEY_SIZE];
    hk.expand(info, &mut okm).expect("HKDF expand failed — output length is valid");
    okm
}

pub fn hkdf_derive_pair(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> ([u8; KEY_SIZE], [u8; KEY_SIZE]) {
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    let mut okm = [0u8; 64];
    hk.expand(info, &mut okm).expect("HKDF expand failed — output length is valid");
    let mut send_key = [0u8; KEY_SIZE];
    let mut recv_key = [0u8; KEY_SIZE];
    send_key.copy_from_slice(&okm[..32]);
    recv_key.copy_from_slice(&okm[32..]);
    okm.zeroize();
    (send_key, recv_key)
}

pub fn generate_nonce() -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

pub fn encrypt(key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce_bytes = generate_nonce();
    let nonce = Nonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes);

    let ciphertext = cipher.encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

    // nonce || ciphertext (ciphertext includes tag appended by AEAD)
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt(key: &[u8; KEY_SIZE], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < NONCE_SIZE + TAG_SIZE {
        bail!("ciphertext too short");
    }

    let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce = Nonce::<ChaCha20Poly1305>::from_slice(nonce_bytes);

    cipher.decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))
}

pub fn encrypt_with_nonce(key: &[u8; KEY_SIZE], nonce: &[u8; NONCE_SIZE], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let n = Nonce::<ChaCha20Poly1305>::from_slice(nonce);

    cipher.encrypt(n, plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))
}

pub fn decrypt_with_nonce(key: &[u8; KEY_SIZE], nonce: &[u8; NONCE_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let n = Nonce::<ChaCha20Poly1305>::from_slice(nonce);

    cipher.decrypt(n, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))
}

pub fn derive_key_from_password(password: &[u8], salt: &[u8]) -> Result<[u8; KEY_SIZE]> {
    use argon2::{Argon2, Algorithm, Version, Params};

    let params = Params::new(65536, 3, 1, Some(KEY_SIZE))
        .map_err(|e| anyhow::anyhow!("argon2 params error: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; KEY_SIZE];
    argon2.hash_password_into(password, salt, &mut key)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?;

    Ok(key)
}

pub fn generate_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = hkdf_derive(b"test secret", None, b"test");
        let plaintext = b"hello, sandseal!";

        let encrypted = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_with_nonce_roundtrip() {
        let key = hkdf_derive(b"test secret", None, b"test");
        let nonce = generate_nonce();
        let plaintext = b"hello with explicit nonce";

        let ciphertext = encrypt_with_nonce(&key, &nonce, plaintext).unwrap();
        let decrypted = decrypt_with_nonce(&key, &nonce, &ciphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = hkdf_derive(b"key1", None, b"test");
        let key2 = hkdf_derive(b"key2", None, b"test");

        let encrypted = encrypt(&key1, b"secret").unwrap();
        assert!(decrypt(&key2, &encrypted).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = hkdf_derive(b"test", None, b"test");
        let mut encrypted = encrypt(&key, b"secret").unwrap();

        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xff;
        assert!(decrypt(&key, &encrypted).is_err());
    }

    #[test]
    fn hkdf_deterministic() {
        let a = hkdf_derive(b"ikm", Some(b"salt"), b"info");
        let b = hkdf_derive(b"ikm", Some(b"salt"), b"info");
        assert_eq!(a, b);
    }

    #[test]
    fn hkdf_different_info_different_keys() {
        let a = hkdf_derive(b"ikm", None, b"send");
        let b = hkdf_derive(b"ikm", None, b"recv");
        assert_ne!(a, b);
    }

    #[test]
    fn password_key_derivation() {
        let salt = generate_salt();
        let key1 = derive_key_from_password(b"mypassword", &salt).unwrap();
        let key2 = derive_key_from_password(b"mypassword", &salt).unwrap();
        assert_eq!(key1, key2);

        let key3 = derive_key_from_password(b"wrongpassword", &salt).unwrap();
        assert_ne!(key1, key3);
    }

    #[test]
    fn derive_pair_produces_different_keys() {
        let (send, recv) = hkdf_derive_pair(b"shared", None, b"session-keys");
        assert_ne!(send, recv);
    }
}
