use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use ed25519_dalek::Signer;
use rand_core_06::{RngCore, OsRng};
use sha2::{Sha256, Digest};

use crate::crypto::encrypt;
use crate::crypto::keys::{IdentityKeyPair, X25519KeyPair};

/// QR pairing payload — encoded into QR code by CLI, scanned by browser.
#[derive(Debug)]
pub struct QrPairingOffer {
    pub ephemeral_public: [u8; 32],
    pub verification_code: String,
    pub identity_public: [u8; 32],
}

impl QrPairingOffer {
    pub fn to_url(&self, base_url: &str) -> String {
        let eph = BASE64URL.encode(self.ephemeral_public);
        let id = BASE64URL.encode(self.identity_public);
        format!(
            "{base_url}/pair?eph={eph}&id={id}&code={}",
            self.verification_code
        )
    }

    pub fn from_params(eph_b64: &str, id_b64: &str, code: &str) -> Result<Self> {
        let eph_bytes = BASE64URL.decode(eph_b64)?;
        let id_bytes = BASE64URL.decode(id_b64)?;

        if eph_bytes.len() != 32 || id_bytes.len() != 32 {
            bail!("invalid key length in QR payload");
        }

        let mut ephemeral_public = [0u8; 32];
        let mut identity_public = [0u8; 32];
        ephemeral_public.copy_from_slice(&eph_bytes);
        identity_public.copy_from_slice(&id_bytes);

        Ok(Self {
            ephemeral_public,
            verification_code: code.to_string(),
            identity_public,
        })
    }
}

/// Generate a 6-digit verification code from shared secret.
pub fn derive_verification_code(shared_secret: &[u8]) -> String {
    let hash = Sha256::digest(shared_secret);
    let num = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 1_000_000;
    format!("{num:06}")
}

/// CLI-side QR pairing: generate offer for browser to scan.
pub fn create_qr_offer(identity: &IdentityKeyPair) -> (QrPairingOffer, X25519KeyPair) {
    let ephemeral = X25519KeyPair::generate();

    let preview_secret = Sha256::digest(ephemeral.public.as_bytes());
    let code = derive_verification_code(&preview_secret);

    let offer = QrPairingOffer {
        ephemeral_public: *ephemeral.public.as_bytes(),
        verification_code: code,
        identity_public: identity.public_key_bytes(),
    };

    (offer, ephemeral)
}

/// Complete QR pairing on CLI side after receiving browser's ephemeral public key.
/// Returns the shared secret and signs the browser's identity key.
pub fn complete_qr_pairing(
    identity: &IdentityKeyPair,
    our_ephemeral: &X25519KeyPair,
    their_ephemeral_public: &[u8; 32],
    their_identity_public: &[u8; 32],
) -> Result<PairingResult> {
    let their_x25519 = x25519_dalek::PublicKey::from(*their_ephemeral_public);
    let shared = our_ephemeral.diffie_hellman(&their_x25519);

    let verification = derive_verification_code(shared.as_bytes());

    let key = encrypt::hkdf_derive(shared.as_bytes(), None, b"sandseal-pairing-v1");

    let sig = identity.signing_key.sign(their_identity_public);

    Ok(PairingResult {
        shared_secret: key,
        verification_code: verification,
        our_signature: sig.to_bytes(),
    })
}

pub struct PairingResult {
    pub shared_secret: [u8; 32],
    pub verification_code: String,
    pub our_signature: [u8; 64],
}

/// Password-based pairing: derive shared key from password + salt.
pub struct PasswordPairing {
    pub salt: [u8; 16],
    pub shared_key: [u8; 32],
}

impl PasswordPairing {
    pub fn initiate(password: &str) -> Result<Self> {
        let salt = encrypt::generate_salt();
        let shared_key = encrypt::derive_key_from_password(password.as_bytes(), &salt)?;
        Ok(Self { salt, shared_key })
    }

    pub fn join(password: &str, salt: &[u8; 16]) -> Result<Self> {
        let shared_key = encrypt::derive_key_from_password(password.as_bytes(), salt)?;
        Ok(Self {
            salt: *salt,
            shared_key,
        })
    }

    /// Encrypt our identity public key with the password-derived key.
    pub fn encrypt_identity(&self, identity: &IdentityKeyPair) -> Result<Vec<u8>> {
        encrypt::encrypt(&self.shared_key, &identity.public_key_bytes())
    }

    /// Decrypt their identity public key.
    pub fn decrypt_identity(&self, ciphertext: &[u8]) -> Result<[u8; 32]> {
        let plaintext = encrypt::decrypt(&self.shared_key, ciphertext)?;
        if plaintext.len() != 32 {
            bail!("invalid identity key length");
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&plaintext);
        Ok(key)
    }
}

/// Generate a random pairing password (human-friendly).
pub fn generate_pairing_password() -> String {
    let charset = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut password = [0u8; 12];
    OsRng.fill_bytes(&mut password);

    let mut result = String::with_capacity(15);
    for (i, byte) in password.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            result.push('-');
        }
        result.push(charset[(*byte as usize) % charset.len()] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_offer_url_roundtrip() {
        let identity = IdentityKeyPair::generate();
        let (offer, _eph) = create_qr_offer(&identity);

        let url = offer.to_url("https://app.sandseal.io");
        assert!(url.starts_with("https://app.sandseal.io/pair?"));
        assert!(url.contains(&offer.verification_code));
    }

    #[test]
    fn qr_pairing_exchange() {
        let cli_identity = IdentityKeyPair::generate();
        let browser_identity = IdentityKeyPair::generate();

        let (offer, cli_ephemeral) = create_qr_offer(&cli_identity);

        // Browser side: generate ephemeral, compute shared secret
        let browser_ephemeral = X25519KeyPair::generate();
        let browser_shared = browser_ephemeral.diffie_hellman(
            &x25519_dalek::PublicKey::from(offer.ephemeral_public),
        );

        // CLI side: complete pairing
        let result = complete_qr_pairing(
            &cli_identity,
            &cli_ephemeral,
            browser_ephemeral.public.as_bytes(),
            &browser_identity.public_key_bytes(),
        ).unwrap();

        // Both sides should derive the same verification code
        let browser_code = derive_verification_code(browser_shared.as_bytes());
        assert_eq!(result.verification_code, browser_code);
    }

    #[test]
    fn password_pairing_exchange() {
        let password = "test-password-1234";

        let cli_identity = IdentityKeyPair::generate();
        let browser_identity = IdentityKeyPair::generate();

        let initiator = PasswordPairing::initiate(password).unwrap();
        let joiner = PasswordPairing::join(password, &initiator.salt).unwrap();

        // Exchange identity keys
        let cli_encrypted = initiator.encrypt_identity(&cli_identity).unwrap();
        let browser_encrypted = joiner.encrypt_identity(&browser_identity).unwrap();

        let received_cli_id = joiner.decrypt_identity(&cli_encrypted).unwrap();
        let received_browser_id = initiator.decrypt_identity(&browser_encrypted).unwrap();

        assert_eq!(received_cli_id, cli_identity.public_key_bytes());
        assert_eq!(received_browser_id, browser_identity.public_key_bytes());
    }

    #[test]
    fn wrong_password_pairing_fails() {
        let cli_identity = IdentityKeyPair::generate();

        let initiator = PasswordPairing::initiate("correct").unwrap();
        let joiner = PasswordPairing::join("wrong", &initiator.salt).unwrap();

        let encrypted = initiator.encrypt_identity(&cli_identity).unwrap();
        assert!(joiner.decrypt_identity(&encrypted).is_err());
    }

    #[test]
    fn generate_pairing_password_format() {
        let pw = generate_pairing_password();
        assert_eq!(pw.len(), 14); // 12 chars + 2 dashes
        assert_eq!(pw.chars().filter(|c| *c == '-').count(), 2);
    }

    #[test]
    fn verification_code_deterministic() {
        let secret = b"some shared secret bytes here!!!";
        let a = derive_verification_code(secret);
        let b = derive_verification_code(secret);
        assert_eq!(a, b);
        assert_eq!(a.len(), 6);
    }
}
