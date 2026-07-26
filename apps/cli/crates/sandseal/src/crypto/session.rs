use std::time::Instant;

use anyhow::{Result, bail};
use x25519_dalek::PublicKey as X25519Public;
use zeroize::Zeroize;

use crate::crypto::encrypt::{
    self, KEY_SIZE, NONCE_SIZE, TAG_SIZE, hkdf_derive_pair,
};
use crate::crypto::keys::X25519KeyPair;

const ROTATION_INTERVAL_SECS: u64 = 3600;
const ROTATION_MESSAGE_COUNT: u64 = 1_000_000;

/// Message types in the encrypted frame protocol.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Data = 0x01,
    KeyRotation = 0x02,
    Ping = 0x03,
    Pong = 0x04,
    Close = 0x05,
    /// Terminal resize: payload is `[cols: u16 BE][rows: u16 BE]` (4 bytes).
    Resize = 0x06,
}

impl MessageType {
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            0x01 => Ok(Self::Data),
            0x02 => Ok(Self::KeyRotation),
            0x03 => Ok(Self::Ping),
            0x04 => Ok(Self::Pong),
            0x05 => Ok(Self::Close),
            0x06 => Ok(Self::Resize),
            _ => bail!("unknown message type: 0x{b:02x}"),
        }
    }
}

/// Frame layout: [type:1][seq:4][nonce:12][ciphertext+tag:N+16]
const FRAME_HEADER_SIZE: usize = 1 + 4; // type + seq
const FRAME_OVERHEAD: usize = FRAME_HEADER_SIZE + NONCE_SIZE + TAG_SIZE;

pub struct SessionKeys {
    send_key: [u8; KEY_SIZE],
    recv_key: [u8; KEY_SIZE],
    send_seq: u32,
    recv_seq: u32,
    send_count: u64,
    recv_count: u64,
    created_at: Instant,
    shared_secret: Vec<u8>,
    generation: u32,
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        self.send_key.zeroize();
        self.recv_key.zeroize();
        self.shared_secret.zeroize();
    }
}

impl SessionKeys {
    /// Establish session keys from an X25519 key exchange.
    /// `is_initiator` determines which side gets send vs recv key.
    pub fn from_key_exchange(
        our_keypair: &X25519KeyPair,
        their_public: &X25519Public,
        is_initiator: bool,
    ) -> Self {
        let shared = our_keypair.diffie_hellman(their_public);
        let shared_bytes = shared.as_bytes().to_vec();

        let info = format!("sandseal-session-v1-gen0").into_bytes();
        let (key_a, key_b) = hkdf_derive_pair(&shared_bytes, None, &info);

        let (send_key, recv_key) = if is_initiator {
            (key_a, key_b)
        } else {
            (key_b, key_a)
        };

        Self {
            send_key,
            recv_key,
            send_seq: 0,
            recv_seq: 0,
            send_count: 0,
            recv_count: 0,
            created_at: Instant::now(),
            shared_secret: shared_bytes,
            generation: 0,
        }
    }

    /// Encrypt a message into a framed packet.
    pub fn seal(&mut self, msg_type: MessageType, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = encrypt::generate_nonce();
        let ciphertext = encrypt::encrypt_with_nonce(&self.send_key, &nonce, plaintext)?;

        let mut frame = Vec::with_capacity(FRAME_HEADER_SIZE + NONCE_SIZE + ciphertext.len());
        frame.push(msg_type as u8);
        frame.extend_from_slice(&self.send_seq.to_be_bytes());
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&ciphertext);

        self.send_seq = self.send_seq.wrapping_add(1);
        self.send_count += 1;

        Ok(frame)
    }

    /// Decrypt a framed packet.
    pub fn open(&mut self, frame: &[u8]) -> Result<(MessageType, Vec<u8>)> {
        if frame.len() < FRAME_OVERHEAD {
            bail!("frame too short: {} bytes", frame.len());
        }

        let msg_type = MessageType::from_byte(frame[0])?;
        let seq = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);

        if seq != self.recv_seq {
            bail!("sequence mismatch: expected {}, got {seq}", self.recv_seq);
        }

        let nonce_start = FRAME_HEADER_SIZE;
        let nonce_end = nonce_start + NONCE_SIZE;
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&frame[nonce_start..nonce_end]);

        let ciphertext = &frame[nonce_end..];
        let plaintext = encrypt::decrypt_with_nonce(&self.recv_key, &nonce, ciphertext)?;

        self.recv_seq = self.recv_seq.wrapping_add(1);
        self.recv_count += 1;

        Ok((msg_type, plaintext))
    }

    /// Check if key rotation is needed.
    pub fn needs_rotation(&self) -> bool {
        self.send_count >= ROTATION_MESSAGE_COUNT
            || self.created_at.elapsed().as_secs() >= ROTATION_INTERVAL_SECS
    }

    /// Rotate keys by deriving a new generation from the shared secret.
    pub fn rotate(&mut self) {
        self.generation += 1;
        let info = format!("sandseal-session-v1-gen{}", self.generation).into_bytes();
        let (key_a, key_b) = hkdf_derive_pair(&self.shared_secret, None, &info);

        let is_initiator = self.send_seq > 0 || self.generation > 1;
        let (new_send, new_recv) = if is_initiator {
            (key_a, key_b)
        } else {
            (key_b, key_a)
        };

        self.send_key.zeroize();
        self.recv_key.zeroize();
        self.send_key = new_send;
        self.recv_key = new_recv;
        self.send_seq = 0;
        self.recv_seq = 0;
        self.send_count = 0;
        self.recv_count = 0;
        self.created_at = Instant::now();
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn send_count(&self) -> u64 {
        self.send_count
    }
}

/// Derive session keys from a pre-shared password (for password-based pairing).
pub fn session_keys_from_password(
    password: &[u8],
    salt: &[u8],
    is_initiator: bool,
) -> Result<SessionKeys> {
    let psk = encrypt::derive_key_from_password(password, salt)?;
    let info = b"sandseal-session-v1-gen0";
    let (key_a, key_b) = hkdf_derive_pair(&psk, None, info);

    let (send_key, recv_key) = if is_initiator {
        (key_a, key_b)
    } else {
        (key_b, key_a)
    };

    Ok(SessionKeys {
        send_key,
        recv_key,
        send_seq: 0,
        recv_seq: 0,
        send_count: 0,
        recv_count: 0,
        created_at: Instant::now(),
        shared_secret: psk.to_vec(),
        generation: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session_pair() -> (SessionKeys, SessionKeys) {
        let alice_kp = X25519KeyPair::generate();
        let bob_kp = X25519KeyPair::generate();

        let alice = SessionKeys::from_key_exchange(&alice_kp, &bob_kp.public, true);
        let bob = SessionKeys::from_key_exchange(&bob_kp, &alice_kp.public, false);

        (alice, bob)
    }

    #[test]
    fn seal_open_roundtrip() {
        let (mut alice, mut bob) = make_session_pair();

        let frame = alice.seal(MessageType::Data, b"hello bob").unwrap();
        let (msg_type, plaintext) = bob.open(&frame).unwrap();

        assert_eq!(msg_type, MessageType::Data);
        assert_eq!(plaintext, b"hello bob");
    }

    #[test]
    fn bidirectional_communication() {
        let (mut alice, mut bob) = make_session_pair();

        let f1 = alice.seal(MessageType::Data, b"alice->bob").unwrap();
        let (_, p1) = bob.open(&f1).unwrap();
        assert_eq!(p1, b"alice->bob");

        let f2 = bob.seal(MessageType::Data, b"bob->alice").unwrap();
        let (_, p2) = alice.open(&f2).unwrap();
        assert_eq!(p2, b"bob->alice");

        let f3 = alice.seal(MessageType::Data, b"second from alice").unwrap();
        let (_, p3) = bob.open(&f3).unwrap();
        assert_eq!(p3, b"second from alice");
    }

    #[test]
    fn sequence_mismatch_detected() {
        let (mut alice, mut bob) = make_session_pair();

        let _f1 = alice.seal(MessageType::Data, b"first").unwrap();
        let f2 = alice.seal(MessageType::Data, b"second").unwrap();

        // Skip f1, try to open f2 — should fail on seq mismatch
        assert!(bob.open(&f2).is_err());
    }

    #[test]
    fn cross_session_decryption_fails() {
        let (mut alice1, _) = make_session_pair();
        let (_, mut bob2) = make_session_pair();

        let frame = alice1.seal(MessageType::Data, b"wrong session").unwrap();
        assert!(bob2.open(&frame).is_err());
    }

    #[test]
    fn key_rotation() {
        let (mut alice, mut bob) = make_session_pair();

        let f1 = alice.seal(MessageType::Data, b"before rotation").unwrap();
        let (_, p1) = bob.open(&f1).unwrap();
        assert_eq!(p1, b"before rotation");

        alice.rotate();
        bob.rotate();

        assert_eq!(alice.generation(), 1);

        let f2 = alice.seal(MessageType::Data, b"after rotation").unwrap();
        let (_, p2) = bob.open(&f2).unwrap();
        assert_eq!(p2, b"after rotation");
    }

    #[test]
    fn password_based_session() {
        let salt = encrypt::generate_salt();
        let mut alice = session_keys_from_password(b"shared-secret", &salt, true).unwrap();
        let mut bob = session_keys_from_password(b"shared-secret", &salt, false).unwrap();

        let frame = alice.seal(MessageType::Data, b"password-paired").unwrap();
        let (_, plaintext) = bob.open(&frame).unwrap();
        assert_eq!(plaintext, b"password-paired");
    }

    #[test]
    fn wrong_password_fails() {
        let salt = encrypt::generate_salt();
        let mut alice = session_keys_from_password(b"correct", &salt, true).unwrap();
        let mut bob = session_keys_from_password(b"wrong", &salt, false).unwrap();

        let frame = alice.seal(MessageType::Data, b"secret").unwrap();
        assert!(bob.open(&frame).is_err());
    }

    #[test]
    fn message_types_roundtrip() {
        let (mut alice, mut bob) = make_session_pair();

        for msg_type in [MessageType::Data, MessageType::Ping, MessageType::Pong, MessageType::Close, MessageType::Resize] {
            let frame = alice.seal(msg_type, b"").unwrap();
            let (decoded_type, _) = bob.open(&frame).unwrap();
            assert_eq!(decoded_type, msg_type);
        }
    }
}
