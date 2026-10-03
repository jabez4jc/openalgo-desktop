//! Minimal NATS nkeys for the Groww feed (web `streaming/groww_nkeys.py`):
//! an Ed25519 user key pair, its base32 public key (`U...`) and seed
//! (`SU...`), and the nonce signature sent in `CONNECT`.
//!
//! Encoding: `base32_nopad(prefix bytes + key + crc16_xmodem LE)`; the user
//! public prefix byte is `0xA0` (`U`), the seed prefix is two bytes
//! `[0x90 | (0xA0 >> 5), (0xA0 & 31) << 3]` (`SU`).

use base64::Engine;
use data_encoding::BASE32_NOPAD;
use ed25519_dalek::{Signer, SigningKey};
use zeroize::Zeroizing;

pub const PREFIX_BYTE_SEED: u8 = 18 << 3; // 0x90
pub const PREFIX_BYTE_USER: u8 = 20 << 3; // 0xA0

/// CRC-16/XMODEM (poly 0x1021, init 0).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for b in data {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn encode(mut raw: Vec<u8>) -> String {
    let crc = crc16(&raw);
    raw.extend_from_slice(&crc.to_le_bytes());
    BASE32_NOPAD.encode(&raw)
}

/// Decode an nkey and check its checksum; returns the bytes before the CRC.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let raw = BASE32_NOPAD.decode(s.as_bytes()).ok()?;
    if raw.len() < 3 {
        return None;
    }
    let (body, crc) = raw.split_at(raw.len() - 2);
    (crc16(body).to_le_bytes() == [crc[0], crc[1]]).then(|| body.to_vec())
}

/// A user key pair. The seed is zeroed on drop and never logged.
pub struct KeyPair {
    signing: SigningKey,
}

impl std::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl KeyPair {
    /// Fresh random key pair.
    pub fn generate() -> Self {
        let seed = Zeroizing::new(rand::random::<[u8; 32]>());
        Self::from_seed(&seed)
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    /// `U...` public key.
    pub fn public_key(&self) -> String {
        let mut raw = vec![PREFIX_BYTE_USER];
        raw.extend_from_slice(self.signing.verifying_key().as_bytes());
        encode(raw)
    }

    /// `SU...` seed.
    pub fn seed(&self) -> Zeroizing<String> {
        let mut raw = Zeroizing::new(vec![
            PREFIX_BYTE_SEED | (PREFIX_BYTE_USER >> 5),
            (PREFIX_BYTE_USER & 31) << 3,
        ]);
        raw.extend_from_slice(self.signing.as_bytes());
        Zeroizing::new(encode(raw.to_vec()))
    }

    /// Standard base64 Ed25519 signature of `nonce` (web `sig`).
    pub fn sign_nonce(&self, nonce: &str) -> String {
        let sig = self.signing.sign(nonce.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
    }
}

/// Public key bytes of a `U...` nkey.
pub fn public_key_bytes(nkey: &str) -> Option<[u8; 32]> {
    let raw = decode(nkey)?;
    if raw.first() != Some(&PREFIX_BYTE_USER) || raw.len() != 33 {
        return None;
    }
    raw[1..].try_into().ok()
}
