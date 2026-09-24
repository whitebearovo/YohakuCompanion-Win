//! DPAPI-encrypted file fallback backend.
//!
//! Owned by agent S. Ground truth:
//! `packages/core/src/store/dpapiCredentialStore.ts` and
//! `capture-stores.md` §9.4. COMPAT INVARIANT — file
//! `<dataDir>/credentials.bin.json`: JSON object deviceId -> base64
//! (standard alphabet, padded, single line) of raw `CryptProtectData`
//! output over the UTF-8 token bytes, scope CurrentUser,
//! `CRYPTPROTECT_UI_FORBIDDEN`, NO optional entropy. Written with the
//! `atomic_write_json` protocol. `get`: missing key or ANY
//! decode/unprotect failure -> "no token". `delete`: only acts when the
//! key exists; DELETES THE FILE when the object empties (contrast:
//! sequence.json keeps `{}`). Do NOT inherit the PowerShell-era `.Trim()`
//! calls — store the token verbatim, in-process.

use std::ffi::c_void;
use std::path::PathBuf;

use async_trait::async_trait;
use parking_lot::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use crate::model::CredentialBackend;
use crate::store::config::{atomic_write_json, data_directory, read_json_object};
use crate::store::credentials::{CredentialStore, CredentialStoreError};

/// File name under the data directory.
pub const CREDENTIALS_FILE_NAME: &str = "credentials.bin.json";

// ---------------------------------------------------------------------------
// Base64 (RFC 4648 standard alphabet, padded, single line) — byte-identical
// to `[Convert]::ToBase64String` / accepted by `[Convert]::FromBase64String`.
// Local implementation because the scaffold Cargo.toml carries no base64
// crate (flagged in integration-notes "S — base64 helper").
// ---------------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(BASE64_ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

fn base64_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strict decode: length multiple of 4, standard alphabet, `=` only as
/// trailing padding (1 or 2). Legacy files were written by
/// `[Convert]::ToBase64String` (no whitespace), so strictness cannot orphan
/// an existing token; any invalid text reads as "no token".
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let chunk_count = bytes.len() / 4;
    let mut out = Vec::with_capacity(chunk_count * 3);
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let last = index + 1 == chunk_count;
        let pad = match (chunk[2], chunk[3]) {
            (b'=', b'=') => 2,
            (b'=', _) => return None, // "=x" tail is never legal
            (_, b'=') => 1,
            _ => 0,
        };
        if pad > 0 && !last {
            return None;
        }
        let v0 = base64_value(chunk[0])?;
        let v1 = base64_value(chunk[1])?;
        let v2 = if pad >= 2 { 0 } else { base64_value(chunk[2])? };
        let v3 = if pad >= 1 { 0 } else { base64_value(chunk[3])? };
        let triple = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// In-process DPAPI (replaces the PowerShell one-shot; capture-stores §12)
// ---------------------------------------------------------------------------

/// `CryptProtectData` over the raw input: CurrentUser scope (no
/// `CRYPTPROTECT_LOCAL_MACHINE`), NULL optional entropy, no prompt,
/// `CRYPTPROTECT_UI_FORBIDDEN` — the exact call shape .NET
/// `ProtectedData.Protect(bytes, null, CurrentUser)` made, so new blobs stay
/// decryptable by the legacy path and vice versa.
fn dpapi_protect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` borrows `data`, which outlives the call; `output` is a
    // valid out-blob. The API only reads through `pbData`.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        Ok(copy_and_free_local_blob(output))
    }
}

/// `CryptUnprotectData`, same flag/entropy shape as [`dpapi_protect`].
fn dpapi_unprotect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in dpapi_protect.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        Ok(copy_and_free_local_blob(output))
    }
}

/// Copies the LocalAlloc'd output of a successful Crypt(Un)ProtectData call
/// and frees it exactly once.
///
/// SAFETY: `blob` must be the untouched out-blob of a SUCCESSFUL
/// Crypt(Un)ProtectData call (its `pbData` is LocalAlloc'd and owned by us).
unsafe fn copy_and_free_local_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    let bytes = if blob.cbData == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec()
    };
    let _ = LocalFree(Some(HLOCAL(blob.pbData as *mut c_void)));
    bytes
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct DpapiCredentialStore {
    path: PathBuf,
    /// Serializes read-modify-write cycles on credentials.bin.json. Held
    /// only across synchronous file IO — never an `.await`.
    io_lock: Mutex<()>,
}

impl DpapiCredentialStore {
    /// `directory = None` uses `store::config::data_directory()`; the
    /// directory is created recursively.
    pub fn new(directory: Option<PathBuf>) -> Result<Self, std::io::Error> {
        let directory = match directory {
            Some(directory) => directory,
            None => data_directory().map_err(std::io::Error::other)?,
        };
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            path: directory.join(CREDENTIALS_FILE_NAME),
            io_lock: Mutex::new(()),
        })
    }

    /// Missing/corrupt/non-object file -> empty map (mirrors sequenceStore).
    fn read(&self) -> serde_json::Map<String, serde_json::Value> {
        read_json_object(&self.path)
    }
}

#[async_trait]
impl CredentialStore for DpapiCredentialStore {
    fn backend(&self) -> CredentialBackend {
        CredentialBackend::Dpapi
    }

    async fn get(&self, device_id: &str) -> Option<String> {
        let blob_b64 = {
            let _guard = self.io_lock.lock();
            // A non-string value under the key is treated as absent.
            self.read().get(device_id)?.as_str()?.to_string()
        };
        // ANY failure — bad base64, undecryptable blob (different user or
        // machine), non-UTF-8 plaintext — reads as "no token" (TS parity).
        let encrypted = base64_decode(&blob_b64)?;
        let decrypted = dpapi_unprotect(&encrypted).ok()?;
        String::from_utf8(decrypted).ok()
    }

    async fn set(&self, device_id: &str, token: &str) -> Result<(), CredentialStoreError> {
        // The token is protected VERBATIM (its UTF-8 bytes): the
        // PowerShell-era `.Trim()` on protect input / unprotect output is
        // deliberately NOT inherited (spec §9.4, ambiguity §13.8 resolution;
        // recorded in integration-notes "S — DPAPI verbatim token").
        let encrypted = dpapi_protect(token.as_bytes())
            .map_err(|e| CredentialStoreError::Backend(format!("CryptProtectData failed: {e}")))?;
        if encrypted.is_empty() {
            // Exact TS message.
            return Err(CredentialStoreError::Backend(
                "DPAPI protect produced no output".to_string(),
            ));
        }
        let blob = base64_encode(&encrypted);
        let _guard = self.io_lock.lock();
        let mut all = self.read();
        all.insert(device_id.to_string(), serde_json::Value::String(blob));
        atomic_write_json(&self.path, &all).map_err(|e| {
            CredentialStoreError::Backend(format!("credentials.bin.json write failed: {e}"))
        })
    }

    async fn delete(&self, device_id: &str) -> Result<(), CredentialStoreError> {
        let _guard = self.io_lock.lock();
        let mut all = self.read();
        if all.remove(device_id).is_none() {
            // Only acts when the key exists (no disk touch otherwise).
            return Ok(());
        }
        if all.is_empty() {
            // TS `rmSync(path, { force: true })`: a missing file is
            // tolerated, any other failure propagates.
            match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(CredentialStoreError::Backend(format!(
                    "credentials.bin.json delete failed: {error}"
                ))),
            }
        } else {
            atomic_write_json(&self.path, &all).map_err(|e| {
                CredentialStoreError::Backend(format!("credentials.bin.json write failed: {e}"))
            })
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("yohaku-core-dpapi-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }

        fn file(&self) -> PathBuf {
            self.0.join(CREDENTIALS_FILE_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store_in(dir: &TempDir) -> DpapiCredentialStore {
        DpapiCredentialStore::new(Some(dir.path())).unwrap()
    }

    const DEVICE: &str = "01J8ME9FZW3W7T2C4Y8K5Q6R9S";

    // -- base64 (RFC 4648 test vectors) --------------------------------------

    #[test]
    fn base64_encode_rfc4648_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(base64_encode(input), *expected);
            assert_eq!(base64_decode(expected).as_deref(), Some(*input));
        }
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        let all_bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(
            base64_decode(&base64_encode(&all_bytes)).unwrap(),
            all_bytes
        );
    }

    #[test]
    fn base64_decode_rejects_invalid_input() {
        for invalid in [
            "Zg",       // bad length
            "Zg=",      // bad length
            "Zg== ",    // trailing space (strict single-line contract)
            "Z\ng==",   // embedded newline
            "Zg==Zg==", // padding before a later chunk
            "=Zg=",     // '=' outside trailing positions
            "Z===",     // "=x" tail shape
            "Zm9$",     // non-alphabet char
        ] {
            assert_eq!(base64_decode(invalid), None, "input: {invalid:?}");
        }
        assert_eq!(base64_decode("").as_deref(), Some(&[][..]));
    }

    // -- in-process DPAPI -----------------------------------------------------

    #[test]
    fn dpapi_protect_unprotect_round_trip() {
        let plain = b"companion-device-token-0123456789";
        let encrypted = dpapi_protect(plain).unwrap();
        assert!(!encrypted.is_empty());
        assert_ne!(encrypted.as_slice(), plain, "output must be ciphertext");
        assert_eq!(dpapi_unprotect(&encrypted).unwrap(), plain);
    }

    #[test]
    fn dpapi_unprotect_rejects_garbage() {
        assert!(dpapi_unprotect(b"definitely not a DPAPI blob").is_err());
    }

    // -- store behavior -------------------------------------------------------

    #[tokio::test]
    async fn set_get_round_trip_and_file_layout() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        let token = "raw-device-token-\u{00e9}\u{4e2d}";
        store.set(DEVICE, token).await.unwrap();
        assert_eq!(store.get(DEVICE).await.as_deref(), Some(token));

        // File layout: JSON object deviceId -> single-line padded base64 of
        // the raw CryptProtectData output, atomic-write formatting.
        let text = fs::read_to_string(tmp.file()).unwrap();
        assert!(!text.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let blob = parsed[DEVICE].as_str().expect("base64 string value");
        assert!(!blob.is_empty());
        assert!(blob.len().is_multiple_of(4));
        assert!(blob
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='));
        // The stored payload IS a decryptable DPAPI blob over the UTF-8 token.
        let decrypted = dpapi_unprotect(&base64_decode(blob).unwrap()).unwrap();
        assert_eq!(decrypted, token.as_bytes());
        assert!(!PathBuf::from(format!("{}.tmp", tmp.file().display())).exists());

        // Survives a "process restart" (fresh store instance).
        let reopened = store_in(&tmp);
        assert_eq!(reopened.get(DEVICE).await.as_deref(), Some(token));
    }

    /// Documented deviation-by-spec: the PowerShell era trimmed the token on
    /// both sides; the Rust port stores it verbatim.
    #[tokio::test]
    async fn token_with_surrounding_whitespace_round_trips_verbatim() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        let token = "  padded-token \n";
        store.set(DEVICE, token).await.unwrap();
        assert_eq!(store.get(DEVICE).await.as_deref(), Some(token));
    }

    #[tokio::test]
    async fn get_missing_or_invalid_reads_as_absent() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        assert_eq!(store.get(DEVICE).await, None, "missing file");

        fs::write(tmp.file(), "corrupt {{{").unwrap();
        assert_eq!(store.get(DEVICE).await, None, "corrupt file");

        fs::write(tmp.file(), format!("{{\"{DEVICE}\": \"not base64!!\"}}")).unwrap();
        assert_eq!(store.get(DEVICE).await, None, "invalid base64");

        // Valid base64 but not a DPAPI blob.
        fs::write(
            tmp.file(),
            format!("{{\"{DEVICE}\": \"{}\"}}", base64_encode(b"junk")),
        )
        .unwrap();
        assert_eq!(store.get(DEVICE).await, None, "undecryptable blob");

        fs::write(tmp.file(), format!("{{\"{DEVICE}\": 42}}")).unwrap();
        assert_eq!(store.get(DEVICE).await, None, "non-string value");
    }

    #[tokio::test]
    async fn delete_removes_key_and_deletes_file_when_empty() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.set("a", "token-a").await.unwrap();
        store.set("b", "token-b").await.unwrap();

        store.delete("a").await.unwrap();
        assert!(tmp.file().exists(), "file stays while keys remain");
        assert_eq!(store.get("a").await, None);
        assert_eq!(store.get("b").await.as_deref(), Some("token-b"));

        store.delete("b").await.unwrap();
        // Contrast with sequence.json: the FILE is deleted when it empties.
        assert!(!tmp.file().exists());
    }

    #[tokio::test]
    async fn delete_of_missing_key_touches_nothing() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.delete(DEVICE).await.unwrap();
        assert!(!tmp.file().exists());

        store.set("other", "t").await.unwrap();
        let before = fs::metadata(tmp.file()).unwrap().modified().unwrap();
        store.delete(DEVICE).await.unwrap();
        let after = fs::metadata(tmp.file()).unwrap().modified().unwrap();
        assert_eq!(before, after, "no rewrite for a missing key");
    }

    #[tokio::test]
    async fn empty_token_round_trips() {
        // TS had no special case for ""; protect of zero bytes is a valid
        // DPAPI blob and must round-trip.
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.set(DEVICE, "").await.unwrap();
        assert_eq!(store.get(DEVICE).await.as_deref(), Some(""));
    }
}
