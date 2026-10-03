//! TOTP (RFC 6238, SHA-1, 6 digits, 30 s) compatible with pyotp and every
//! authenticator app, plus the provisioning URI and its QR code as a PNG.

use crate::error::{AppError, Result};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use subtle::ConstantTimeEq;

const STEP: u64 = 30;
const DIGITS: u32 = 6;

/// 32-character base32 secret (160 bits), same as `pyotp.random_base32()`.
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    data_encoding::BASE32_NOPAD.encode(&bytes)
}

fn decode_secret(secret: &str) -> Result<Vec<u8>> {
    let cleaned: String = secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect::<String>()
        .to_uppercase();
    data_encoding::BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .map_err(|_| AppError::Internal("invalid TOTP secret".into()))
}

fn code_at(key: &[u8], counter: u64) -> Result<u32> {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key)
        .map_err(|_| AppError::Internal("invalid TOTP key".into()))?;
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[h.len() - 1] & 0x0f) as usize;
    let bin = ((h[offset] as u32 & 0x7f) << 24)
        | ((h[offset + 1] as u32) << 16)
        | ((h[offset + 2] as u32) << 8)
        | (h[offset + 3] as u32);
    Ok(bin % 10u32.pow(DIGITS))
}

pub fn generate(secret: &str, unix_time: u64) -> Result<String> {
    let key = decode_secret(secret)?;
    Ok(format!(
        "{:0width$}",
        code_at(&key, unix_time / STEP)?,
        width = DIGITS as usize
    ))
}

/// Accepts the current step and one step either side for clock drift.
pub fn verify(secret: &str, code: &str, unix_time: u64) -> bool {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let key = match decode_secret(secret) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let counter = unix_time / STEP;
    let mut ok = false;
    for c in [counter.saturating_sub(1), counter, counter + 1] {
        if let Ok(v) = code_at(&key, c) {
            let s = format!("{:0width$}", v, width = DIGITS as usize);
            ok |= bool::from(s.as_bytes().ct_eq(code.as_bytes()));
        }
    }
    ok
}

/// `otpauth://` URI in the same form pyotp produces.
pub fn provisioning_uri(secret: &str, account: &str) -> String {
    format!(
        "otpauth://totp/OpenAlgo:{}?secret={}&issuer=OpenAlgo",
        urlencoding::encode(account),
        secret
    )
}

/// QR code of `data` as a base64 PNG (box size 10, border 5, like the web).
pub fn qr_png_base64(data: &str) -> Result<String> {
    let code = qrcode::QrCode::new(data.as_bytes())
        .map_err(|e| AppError::Internal(format!("qr: {}", e)))?;
    let width = code.width();
    let colors = code.to_colors();
    let scale = 10usize;
    let border = 5usize;
    let size = (width + 2 * border) * scale;
    let mut pixels = vec![255u8; size * size];
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    let row = (y + border) * scale + dy;
                    let start = row * size + (x + border) * scale;
                    pixels[start..start + scale].fill(0);
                }
            }
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size as u32, size as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| AppError::Internal(format!("png: {}", e)))?;
        w.write_image_data(&pixels)
            .map_err(|e| AppError::Internal(format!("png: {}", e)))?;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 appendix B, SHA-1 key "12345678901234567890", 8 digits; we use
    // the last 6 digits of the published values.
    #[test]
    fn rfc6238_vectors() {
        let secret = data_encoding::BASE32_NOPAD.encode(b"12345678901234567890");
        assert_eq!(generate(&secret, 59).unwrap(), "287082");
        assert_eq!(generate(&secret, 1111111109).unwrap(), "081804");
        assert_eq!(generate(&secret, 1234567890).unwrap(), "005924");
    }

    #[test]
    fn verify_accepts_adjacent_step_only() {
        let s = generate_secret();
        assert_eq!(s.len(), 32);
        let t = 1_800_000_000u64;
        let code = generate(&s, t).unwrap();
        assert!(verify(&s, &code, t));
        assert!(verify(&s, &code, t + 30));
        assert!(!verify(&s, &code, t + 120));
        assert!(!verify(&s, "12345", t));
        assert!(!verify(&s, "abcdef", t));
    }

    #[test]
    fn uri_and_qr() {
        let uri = provisioning_uri("ABC", "me@example.com");
        assert_eq!(
            uri,
            "otpauth://totp/OpenAlgo:me%40example.com?secret=ABC&issuer=OpenAlgo"
        );
        let png = qr_png_base64(&uri).unwrap();
        assert!(png.starts_with("iVBORw0KGgo"));
    }
}
