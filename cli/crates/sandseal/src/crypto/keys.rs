use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core_06::OsRng;
use x25519_dalek::{StaticSecret, PublicKey as X25519Public};
use zeroize::Zeroize;

const IDENTITY_PRIVATE_FILE: &str = "identity.key";
const IDENTITY_PUBLIC_FILE: &str = "identity.pub";

pub struct IdentityKeyPair {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl IdentityKeyPair {
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        Self { signing_key, verifying_key }
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    pub fn public_key_base64(&self) -> String {
        BASE64.encode(self.verifying_key.as_bytes())
    }
}

pub struct X25519KeyPair {
    pub secret: StaticSecret,
    pub public: X25519Public,
}

impl X25519KeyPair {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = X25519Public::from(&secret);
        Self { secret, public }
    }

    pub fn diffie_hellman(&self, their_public: &X25519Public) -> x25519_dalek::SharedSecret {
        self.secret.diffie_hellman(their_public)
    }
}

fn keys_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let dir = home.join(".sandseal").join("keys");
    Ok(dir)
}

pub fn ensure_identity() -> Result<IdentityKeyPair> {
    let dir = keys_dir()?;
    let priv_path = dir.join(IDENTITY_PRIVATE_FILE);
    let pub_path = dir.join(IDENTITY_PUBLIC_FILE);

    if priv_path.exists() && pub_path.exists() {
        return load_identity(&priv_path, &pub_path);
    }

    let kp = IdentityKeyPair::generate();
    save_identity(&kp, &priv_path, &pub_path)?;
    Ok(kp)
}

fn save_identity(kp: &IdentityKeyPair, priv_path: &Path, pub_path: &Path) -> Result<()> {
    if let Some(parent) = priv_path.parent() {
        fs::create_dir_all(parent).context("failed to create keys directory")?;
    }

    let mut priv_bytes = kp.signing_key.to_bytes();
    fs::write(priv_path, BASE64.encode(&priv_bytes)).context("failed to write private key")?;
    priv_bytes.zeroize();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(priv_path, fs::Permissions::from_mode(0o600))?;
    }

    fs::write(pub_path, BASE64.encode(kp.verifying_key.as_bytes()))
        .context("failed to write public key")?;

    Ok(())
}

fn load_identity(priv_path: &Path, pub_path: &Path) -> Result<IdentityKeyPair> {
    let priv_b64 = fs::read_to_string(priv_path).context("failed to read private key")?;
    let mut priv_bytes = BASE64.decode(priv_b64.trim()).context("invalid private key encoding")?;

    if priv_bytes.len() != 32 {
        bail!("invalid private key length: expected 32 bytes, got {}", priv_bytes.len());
    }

    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&priv_bytes);
    priv_bytes.zeroize();

    let signing_key = SigningKey::from_bytes(&key_array);
    key_array.zeroize();

    let pub_b64 = fs::read_to_string(pub_path).context("failed to read public key")?;
    let pub_bytes = BASE64.decode(pub_b64.trim()).context("invalid public key encoding")?;

    if pub_bytes.len() != 32 {
        bail!("invalid public key length: expected 32 bytes, got {}", pub_bytes.len());
    }

    let mut pub_array = [0u8; 32];
    pub_array.copy_from_slice(&pub_bytes);
    let verifying_key = VerifyingKey::from_bytes(&pub_array)
        .context("invalid public key")?;

    if signing_key.verifying_key() != verifying_key {
        bail!("public key does not match private key");
    }

    Ok(IdentityKeyPair { signing_key, verifying_key })
}

pub fn identity_exists() -> bool {
    keys_dir()
        .map(|d| d.join(IDENTITY_PRIVATE_FILE).exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_identity_keypair() {
        let kp = IdentityKeyPair::generate();
        assert_eq!(kp.public_key_bytes().len(), 32);
        assert_eq!(kp.signing_key.verifying_key(), kp.verifying_key);
    }

    #[test]
    fn generate_x25519_keypair() {
        let kp = X25519KeyPair::generate();
        assert_eq!(kp.public.as_bytes().len(), 32);
    }

    #[test]
    fn x25519_key_exchange() {
        let alice = X25519KeyPair::generate();
        let bob = X25519KeyPair::generate();

        let alice_shared = alice.diffie_hellman(&bob.public);
        let bob_shared = bob.diffie_hellman(&alice.public);

        assert_eq!(alice_shared.as_bytes(), bob_shared.as_bytes());
    }

    #[test]
    fn save_load_identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let priv_path = dir.path().join("test.key");
        let pub_path = dir.path().join("test.pub");

        let original = IdentityKeyPair::generate();
        save_identity(&original, &priv_path, &pub_path).unwrap();

        let loaded = load_identity(&priv_path, &pub_path).unwrap();
        assert_eq!(original.signing_key.to_bytes(), loaded.signing_key.to_bytes());
        assert_eq!(original.verifying_key, loaded.verifying_key);
    }

    #[test]
    fn public_key_base64_roundtrip() {
        let kp = IdentityKeyPair::generate();
        let b64 = kp.public_key_base64();
        let decoded = BASE64.decode(&b64).unwrap();
        assert_eq!(decoded, kp.verifying_key.as_bytes());
    }
}
