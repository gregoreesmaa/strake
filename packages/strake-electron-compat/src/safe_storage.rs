//! `safeStorage` OS keychain binding (issue #94).
//!
//! Secrets storage (sync passwords, API tokens) is a hard requirement for
//! Joplin-class apps. Production encrypts through the OS keychain
//! (Keychain on macOS, Credential Manager on Windows, Secret Service on
//! Linux); the [`KeychainBackend`] trait is that seam. CI keychains are
//! unavailable, so headless tests use the [`RecordingKeychain`] fallback,
//! whose Electron-parity decision is explicit: it reports itself available
//! and round-trips, is documented NOT secure, and exists only so the binding
//! contract (encrypt → opaque string → decrypt) stays covered without OS
//! credentials.

use std::sync::{Arc, Mutex};

/// Failures encrypting or decrypting secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeStorageError {
    /// No keychain is available on this machine.
    BackendUnavailable,
    /// The ciphertext did not decode or authenticate.
    DecryptFailed,
}

impl std::fmt::Display for SafeStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackendUnavailable => write!(f, "no OS keychain available"),
            Self::DecryptFailed => write!(f, "ciphertext failed to decrypt"),
        }
    }
}

impl std::error::Error for SafeStorageError {}

/// OS keychain seam. Production backends seal with a machine key; the
/// recorder below stands in headless.
pub trait KeychainBackend: Send + Sync {
    /// Whether encryption is available (`isEncryptionAvailable`).
    fn is_available(&self) -> bool;
    /// Seal `plain` into opaque bytes.
    fn encrypt(&self, plain: &[u8]) -> Result<Vec<u8>, SafeStorageError>;
    /// Open opaque bytes sealed by [`Self::encrypt`].
    fn decrypt(&self, sealed: &[u8]) -> Result<Vec<u8>, SafeStorageError>;
}

/// Headless fallback: available, round-tripping, explicitly NOT secure.
/// XOR-obfuscates with a fixed pad so the stored form is opaque but the
/// binding stays testable; never use for real secrets.
#[derive(Debug, Default)]
pub struct RecordingKeychain {
    writes: Mutex<Vec<Vec<u8>>>,
}

impl RecordingKeychain {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sealed payloads written so far (test observation).
    pub fn sealed_writes(&self) -> Vec<Vec<u8>> {
        self.writes.lock().expect("keychain mutex").clone()
    }
}

const RECORDING_PAD: &[u8] = b"strake-recording-keychain-pad-v1";

fn obfuscate(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ RECORDING_PAD[index % RECORDING_PAD.len()])
        .collect()
}

impl KeychainBackend for RecordingKeychain {
    fn is_available(&self) -> bool {
        true
    }

    fn encrypt(&self, plain: &[u8]) -> Result<Vec<u8>, SafeStorageError> {
        let sealed = obfuscate(plain);
        self.writes
            .lock()
            .expect("keychain mutex")
            .push(sealed.clone());
        Ok(sealed)
    }

    fn decrypt(&self, sealed: &[u8]) -> Result<Vec<u8>, SafeStorageError> {
        Ok(obfuscate(sealed))
    }
}

/// Electron `safeStorage` (`isEncryptionAvailable`/`encryptString`/
/// `decryptString`). Encrypted bytes cross the JS boundary as base64 text
/// (the headless encoding of Electron's `Buffer`).
#[derive(Clone)]
pub struct SafeStorage {
    backend: Arc<dyn KeychainBackend>,
}

impl SafeStorage {
    /// Bind an explicit backend (OS keychain at runtime, recorder in tests).
    pub fn new(backend: Arc<dyn KeychainBackend>) -> Self {
        Self { backend }
    }

    /// Headless/CI storage backed by [`RecordingKeychain`].
    pub fn recording() -> Self {
        Self::new(Arc::new(RecordingKeychain::new()))
    }

    /// `safeStorage.isEncryptionAvailable()`.
    pub fn is_encryption_available(&self) -> bool {
        self.backend.is_available()
    }

    /// `safeStorage.encryptString(plain)` → base64 ciphertext.
    pub fn encrypt_string(&self, plain: &str) -> Result<String, SafeStorageError> {
        if !self.backend.is_available() {
            return Err(SafeStorageError::BackendUnavailable);
        }
        Ok(encode_base64(&self.backend.encrypt(plain.as_bytes())?))
    }

    /// `safeStorage.decryptString(base64)` → plaintext.
    pub fn decrypt_string(&self, sealed_base64: &str) -> Result<String, SafeStorageError> {
        if !self.backend.is_available() {
            return Err(SafeStorageError::BackendUnavailable);
        }
        let sealed = decode_base64(sealed_base64).ok_or(SafeStorageError::DecryptFailed)?;
        let plain = self.backend.decrypt(&sealed)?;
        String::from_utf8(plain).map_err(|_| SafeStorageError::DecryptFailed)
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut word = [0u8; 3];
        word[..chunk.len()].copy_from_slice(chunk);
        let triple = (u32::from(word[0]) << 16) | (u32::from(word[1]) << 8) | u32::from(word[2]);
        let chars = [
            BASE64_ALPHABET[(triple >> 18) as usize & 63],
            BASE64_ALPHABET[(triple >> 12) as usize & 63],
            BASE64_ALPHABET[(triple >> 6) as usize & 63],
            BASE64_ALPHABET[triple as usize & 63],
        ];
        let shown = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for (index, char) in chars.iter().enumerate() {
            out.push(if index < shown { *char as char } else { '=' });
        }
    }
    out
}

fn decode_base64_char(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut sextets = [0u32; 4];
        let mut padding = 0;
        for (index, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                padding += 1;
            } else if padding > 0 {
                return None;
            } else {
                sextets[index] = decode_base64_char(*byte)?;
            }
        }
        if padding > 2 {
            return None;
        }
        let triple = (sextets[0] << 18) | (sextets[1] << 12) | (sextets[2] << 6) | sextets[3];
        out.push((triple >> 16) as u8);
        if padding < 2 {
            out.push((triple >> 8) as u8);
        }
        if padding < 1 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_backend_round_trips_strings() {
        let storage = SafeStorage::recording();
        assert!(storage.is_encryption_available());
        let sealed = storage.encrypt_string("hunter2").expect("encrypt");
        assert_ne!(sealed.as_bytes(), b"hunter2", "stored form is opaque");
        assert_eq!(storage.decrypt_string(&sealed).as_deref(), Ok("hunter2"));
    }

    #[test]
    fn garbage_ciphertext_fails_to_decrypt() {
        let storage = SafeStorage::recording();
        assert_eq!(
            storage.decrypt_string("!!!not-base64!!!"),
            Err(SafeStorageError::DecryptFailed)
        );
    }

    #[test]
    fn base64_codec_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
        assert_eq!(decode_base64("Zm9vYmFy"), Some(b"foobar".to_vec()));
        assert_eq!(decode_base64("Zg=="), Some(b"f".to_vec()));
        assert_eq!(decode_base64("abc"), None, "length must be a multiple of 4");
        assert_eq!(decode_base64("===="), None, "pure padding is invalid");
    }
}
