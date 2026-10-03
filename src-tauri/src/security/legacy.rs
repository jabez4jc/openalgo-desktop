//! Reader for the retired `secrets.dat` file.
//!
//! Earlier builds stored the data key and pepper in `secrets.dat`, XOR-ed
//! with a constant compiled into the binary. That is not encryption. This
//! module exists only so the first run of this version can carry those keys
//! into the keychain (or the password vault), re-encrypt every stored secret
//! with associated data, and delete the file. Nothing writes this format.

use crate::error::{AppError, Result};
use crate::security::secret::SecretBytes;
use base64::Engine;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "secrets.dat";
const OBFUSCATION: &[u8] = b"OpenAlgo-Desktop-v1.0-SecretKey!";

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Decode `secrets.dat` into (data key, pepper).
pub fn read(data_dir: &Path) -> Result<Option<(SecretBytes, SecretBytes)>> {
    let p = path(data_dir);
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&p)?;
    decode(&raw).map(Some)
}

fn decode(raw: &[u8]) -> Result<(SecretBytes, SecretBytes)> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| AppError::Config("secrets.dat is not readable".into()))?;
    let mut parts = text.trim().split(':');
    let (a, b) = match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => (a, b),
        _ => return Err(AppError::Config("secrets.dat has an unknown format".into())),
    };
    let dec = |s: &str| -> Result<SecretBytes> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| AppError::Config("secrets.dat has an unknown format".into()))?;
        Ok(SecretBytes::new(
            bytes
                .iter()
                .zip(OBFUSCATION.iter().cycle())
                .map(|(x, k)| x ^ k)
                .collect(),
        ))
    };
    Ok((dec(a)?, dec(b)?))
}

/// Remove the file after its keys have been moved and old rows re-encrypted.
pub fn remove(data_dir: &Path) -> Result<()> {
    let p = path(data_dir);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

#[cfg(test)]
pub fn write_for_test(data_dir: &Path, key: &[u8], pepper: &[u8]) {
    let enc = |v: &[u8]| {
        base64::engine::general_purpose::STANDARD.encode(
            v.iter()
                .zip(OBFUSCATION.iter().cycle())
                .map(|(x, k)| x ^ k)
                .collect::<Vec<u8>>(),
        )
    };
    std::fs::write(path(data_dir), format!("{}:{}", enc(key), enc(pepper))).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_old_format() {
        let tmp = tempfile::tempdir().unwrap();
        write_for_test(tmp.path(), &[1u8; 32], &[2u8; 32]);
        let (k, p) = read(tmp.path()).unwrap().unwrap();
        assert_eq!(k.expose(), &[1u8; 32]);
        assert_eq!(p.expose(), &[2u8; 32]);
        remove(tmp.path()).unwrap();
        assert!(read(tmp.path()).unwrap().is_none());
    }
}
