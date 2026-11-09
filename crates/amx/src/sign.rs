//! Optional Ed25519 bundle signatures (SPEC section 9.2), behind the
//! `signing` feature.
//!
//! `manifest.sig` is one line: `ed25519:<base64 public key>:<base64
//! signature>`, signing the bytes of `CHECKSUMS`. `CHECKSUMS` covers the
//! manifest and every data file, tombstones included, so the signature
//! covers the whole bundle.
use crate::{Bundle, CHECKSUMS, Error, Result, SIGNATURE};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::{collections::BTreeMap, fs, path::Path};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn signature_error(message: impl Into<String>) -> Error {
    Error::new("signature", message)
}

fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for chunk in text.as_bytes().chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || chunk[..4 - pad].contains(&b'=') {
            return None;
        }
        let mut n = 0u32;
        for &c in chunk {
            let value = if c == b'=' {
                0
            } else {
                ALPHABET.iter().position(|&a| a == c)? as u32
            };
            n = (n << 6) | value;
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

/// The public key for a 32-byte Ed25519 secret seed.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

/// Sign a bundle in place: declare the `signing` capability, then sign the
/// `CHECKSUMS` its encoding produces. Any later change to the bundle
/// invalidates the signature.
pub fn sign_bundle(bundle: &mut Bundle, secret: &[u8; 32]) -> Result<()> {
    let capabilities = bundle.manifest.capabilities.get_or_insert_with(Vec::new);
    if !capabilities.iter().any(|c| c == "signing") {
        capabilities.push("signing".into());
        capabilities.sort();
    }
    bundle.signature = None;
    let files = bundle.encode()?;
    let key = SigningKey::from_bytes(secret);
    let signature = key.sign(&files[CHECKSUMS]);
    bundle.signature = Some(format!(
        "ed25519:{}:{}\n",
        base64(&key.verifying_key().to_bytes()),
        base64(&signature.to_bytes())
    ));
    Ok(())
}

/// Check `manifest.sig` against the exact `CHECKSUMS` bytes. Returns the
/// signer's public key; with `trusted`, the signer must be that key.
fn check(
    signature: Option<&[u8]>,
    checksums: &[u8],
    trusted: Option<&[u8; 32]>,
) -> Result<[u8; 32]> {
    let signature =
        signature.ok_or_else(|| Error::new("unsigned", "bundle has no manifest.sig"))?;
    let text =
        std::str::from_utf8(signature).map_err(|_| signature_error("manifest.sig is not UTF-8"))?;
    let line = text
        .strip_suffix('\n')
        .ok_or_else(|| signature_error("manifest.sig must be one line"))?;
    let mut parts = line.split(':');
    let (Some("ed25519"), Some(key), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(signature_error(
            "manifest.sig must be ed25519:<key>:<signature>",
        ));
    };
    let key: [u8; 32] = unbase64(key)
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| signature_error("public key is not 32 base64 bytes"))?;
    let sig: [u8; 64] = unbase64(sig)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| signature_error("signature is not 64 base64 bytes"))?;
    if trusted.is_some_and(|t| *t != key) {
        return Err(signature_error("bundle is signed by an untrusted key"));
    }
    let verifying = VerifyingKey::from_bytes(&key)
        .map_err(|_| signature_error("public key is not a valid Ed25519 point"))?;
    verifying
        .verify(checksums, &Signature::from_bytes(&sig))
        .map_err(|_| signature_error("signature does not match the bundle"))?;
    Ok(key)
}

/// Decode in-memory bundle files and require a valid signature.
pub fn verify_files(
    files: &BTreeMap<String, Vec<u8>>,
    trusted: Option<&[u8; 32]>,
) -> Result<(Bundle, [u8; 32])> {
    let bundle = Bundle::decode(files)?;
    let checksums = files
        .get(CHECKSUMS)
        .ok_or_else(|| Error::new("invalid", "bundle is missing CHECKSUMS"))?;
    let key = check(files.get(SIGNATURE).map(Vec::as_slice), checksums, trusted)?;
    Ok((bundle, key))
}

fn read_regular(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => fs::read(path)
            .map(Some)
            .map_err(|e| Error::new("io", e.to_string())),
        Ok(_) => Err(Error::new(
            "invalid",
            format!("{} must be a regular file", path.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new("io", e.to_string())),
    }
}

/// Read a bundle directory and require a valid signature. The signed
/// `CHECKSUMS` bytes are read before and after the verified read; a change
/// in between is rejected.
pub fn read_verified(path: &Path, trusted: Option<&[u8; 32]>) -> Result<(Bundle, [u8; 32])> {
    let before = read_regular(&path.join(CHECKSUMS))?;
    let bundle = Bundle::read(path)?;
    let after = read_regular(&path.join(CHECKSUMS))?;
    let checksums = match (before, after) {
        (Some(before), Some(after)) if before == after => before,
        _ => {
            return Err(signature_error(
                "CHECKSUMS changed while the bundle was read",
            ));
        }
    };
    let key = check(
        bundle.signature.as_deref().map(str::as_bytes),
        &checksums,
        trusted,
    )?;
    Ok((bundle, key))
}

#[cfg(test)]
mod tests {
    use super::{base64, unbase64};

    #[test]
    fn base64_round_trips() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(unbase64(&base64(input)).as_deref(), Some(input));
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(unbase64("Zm9=v"), None);
        assert_eq!(unbase64("Z=9v"), None);
        assert_eq!(unbase64("Zm9*"), None);
    }
}
