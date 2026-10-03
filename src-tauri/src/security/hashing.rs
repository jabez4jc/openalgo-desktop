//! Argon2id password hashing with a pepper, and the HMAC lookup index for
//! API keys.

use crate::error::{AppError, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, Params, Version,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const PEPPER_SIZE: usize = 32;

/// OWASP baseline for Argon2id. Parameters are stored in the PHC string, so
/// verification of older hashes keeps working if these change.
fn argon2() -> Result<Argon2<'static>> {
    let params = Params::new(19456, 2, 1, None)
        .map_err(|e| AppError::Internal(format!("argon2 params: {}", e)))?;
    Ok(Argon2::new(
        argon2::Algorithm::Argon2id,
        Version::V0x13,
        params,
    ))
}

fn peppered(pepper: &[u8], password: &str) -> String {
    let pepper_b64 = base64::engine::general_purpose::STANDARD.encode(pepper);
    format!("{}{}", password, pepper_b64)
}

pub fn hash_password(pepper: &[u8], password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon2()?
        .hash_password(peppered(pepper, password).as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(format!("password hashing failed: {}", e)))
}

pub fn verify_password(pepper: &[u8], password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| AppError::Internal(format!("invalid password hash: {}", e)))?;
    match Argon2::default().verify_password(peppered(pepper, password).as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(AppError::Internal(format!("password verification: {}", e))),
    }
}

/// Deterministic, indexable digest of an API key: HMAC-SHA256 keyed with the
/// pepper. Lets the server find the one candidate row without running Argon2
/// over every stored key.
pub fn lookup_hmac(pepper: &[u8], api_key: &str) -> String {
    // HMAC accepts any key length, so this cannot fail.
    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(pepper) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    mac.update(b"openalgo-api-key:");
    mac.update(api_key.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Raw 32-byte key derived from a password, used to wrap the key vault when
/// no OS keychain is available.
pub fn derive_kek(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    argon2()?
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Encryption(format!("key derivation failed: {}", e)))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let pepper = [7u8; 32];
        let h = hash_password(&pepper, "Secret@123").unwrap();
        assert!(verify_password(&pepper, "Secret@123", &h).unwrap());
        assert!(!verify_password(&pepper, "wrong", &h).unwrap());
        assert!(!verify_password(&[8u8; 32], "Secret@123", &h).unwrap());
    }

    #[test]
    fn hmac_is_deterministic_and_keyed() {
        let a = lookup_hmac(&[1u8; 32], "key");
        assert_eq!(a, lookup_hmac(&[1u8; 32], "key"));
        assert_ne!(a, lookup_hmac(&[2u8; 32], "key"));
        assert_ne!(a, lookup_hmac(&[1u8; 32], "key2"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn kek_depends_on_password_and_salt() {
        let a = derive_kek("pw", b"0123456789abcdef").unwrap();
        assert_eq!(a, derive_kek("pw", b"0123456789abcdef").unwrap());
        assert_ne!(a, derive_kek("pw2", b"0123456789abcdef").unwrap());
        assert_ne!(a, derive_kek("pw", b"fedcba9876543210").unwrap());
    }
}
