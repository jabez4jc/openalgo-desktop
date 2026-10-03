//! Minimal ZIP reader for the Noren `*_symbols.txt.zip` masters: one
//! stored or deflated entry, located through the central directory (so
//! entries written with a trailing data descriptor read correctly).
//! ZIP64 is not needed (each file is a few MB) and is refused.

use crate::error::{AppError, Result};
use std::io::Read;

/// Refuse anything that would inflate past this (zip-bomb guard).
pub const MAX_UNCOMPRESSED: u64 = 512 * 1024 * 1024;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn bad() -> AppError {
    AppError::Broker(
        "The broker's instrument file could not be read. Try downloading the master contract again."
            .into(),
    )
}

/// The first file entry of a ZIP archive, decompressed.
pub fn first_entry(data: &[u8]) -> Result<Vec<u8>> {
    // End of central directory: signature 0x06054b50 within the last 64 KiB.
    let min = data.len().saturating_sub(22 + 65535);
    let eocd = (min..data.len().saturating_sub(21))
        .rev()
        .find(|&o| u32le(data, o) == Some(0x0605_4b50))
        .ok_or_else(bad)?;
    let entries = u16le(data, eocd + 10).ok_or_else(bad)?;
    let mut cd = u32le(data, eocd + 16).ok_or_else(bad)? as usize;
    for _ in 0..entries {
        if u32le(data, cd) != Some(0x0201_4b50) {
            return Err(bad());
        }
        let method = u16le(data, cd + 10).ok_or_else(bad)?;
        let csize = u32le(data, cd + 20).ok_or_else(bad)? as usize;
        let usize_ = u32le(data, cd + 24).ok_or_else(bad)? as u64;
        let nlen = u16le(data, cd + 28).ok_or_else(bad)? as usize;
        let elen = u16le(data, cd + 30).ok_or_else(bad)? as usize;
        let clen = u16le(data, cd + 32).ok_or_else(bad)? as usize;
        let local = u32le(data, cd + 42).ok_or_else(bad)? as usize;
        let name = data.get(cd + 46..cd + 46 + nlen).ok_or_else(bad)?;
        cd += 46 + nlen + elen + clen;
        if name.ends_with(b"/") {
            continue;
        }
        if csize == u32::MAX as usize || usize_ == u64::from(u32::MAX) || usize_ > MAX_UNCOMPRESSED
        {
            return Err(bad());
        }
        if u32le(data, local) != Some(0x0403_4b50) {
            return Err(bad());
        }
        let lnlen = u16le(data, local + 26).ok_or_else(bad)? as usize;
        let lelen = u16le(data, local + 28).ok_or_else(bad)? as usize;
        let start = local + 30 + lnlen + lelen;
        let body = data.get(start..start + csize).ok_or_else(bad)?;
        return match method {
            0 => Ok(body.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(usize_ as usize);
                flate2::read::DeflateDecoder::new(body)
                    .take(MAX_UNCOMPRESSED + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| bad())?;
                if out.len() as u64 > MAX_UNCOMPRESSED {
                    return Err(bad());
                }
                Ok(out)
            }
            _ => Err(bad()),
        };
    }
    Err(bad())
}

/// Build a one-entry archive (tests and fixtures).
#[cfg(any(test, feature = "test-support"))]
pub fn build(name: &str, content: &[u8], deflate: bool) -> Vec<u8> {
    use std::io::Write;
    let payload = if deflate {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = e.write_all(content);
        e.finish().unwrap_or_default()
    } else {
        content.to_vec()
    };
    let crc = crc32(content);
    let method: u16 = if deflate { 8 } else { 0 };
    let mut out = Vec::new();
    let push16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let push32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    push32(&mut out, 0x0403_4b50);
    push16(&mut out, 20);
    push16(&mut out, 0);
    push16(&mut out, method);
    push32(&mut out, 0);
    push32(&mut out, crc);
    push32(&mut out, payload.len() as u32);
    push32(&mut out, content.len() as u32);
    push16(&mut out, name.len() as u16);
    push16(&mut out, 0);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&payload);
    let cd_start = out.len();
    push32(&mut out, 0x0201_4b50);
    push16(&mut out, 20);
    push16(&mut out, 20);
    push16(&mut out, 0);
    push16(&mut out, method);
    push32(&mut out, 0);
    push32(&mut out, crc);
    push32(&mut out, payload.len() as u32);
    push32(&mut out, content.len() as u32);
    push16(&mut out, name.len() as u16);
    push16(&mut out, 0);
    push16(&mut out, 0);
    push16(&mut out, 0);
    push16(&mut out, 0);
    push32(&mut out, 0);
    push32(&mut out, 0);
    out.extend_from_slice(name.as_bytes());
    let cd_len = out.len() - cd_start;
    push32(&mut out, 0x0605_4b50);
    push16(&mut out, 0);
    push16(&mut out, 0);
    push16(&mut out, 1);
    push16(&mut out, 1);
    push32(&mut out, cd_len as u32);
    push32(&mut out, cd_start as u32);
    push16(&mut out, 0);
    out
}

#[cfg(any(test, feature = "test-support"))]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_stored_and_deflated() {
        let text = b"Exchange,Token,LotSize\nNSE,22,1\n".repeat(50);
        for deflate in [false, true] {
            let z = build("NSE_symbols.txt", &text, deflate);
            assert_eq!(first_entry(&z).unwrap(), text);
        }
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(first_entry(b"not a zip").is_err());
        assert!(first_entry(&[]).is_err());
        let mut z = build("a.txt", b"hello", true);
        let n = z.len();
        z.truncate(n - 30);
        assert!(first_entry(&z).is_err());
    }
}
