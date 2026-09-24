//! Windows Credential Manager backend.
//!
//! Owned by agent S. Ground truth:
//! `packages/core/src/store/keyringCredentialStore.ts` and
//! `capture-stores.md` §9.3 (empirically verified). COMPAT INVARIANT — the
//! exact entry layout of `@napi-rs/keyring` 1.3.0 defaults so existing
//! credentials are found:
//! `TargetName = "<deviceId>.yohaku-companion-win"`, `UserName = <deviceId>`,
//! Type `CRED_TYPE_GENERIC` (1), Persist `CRED_PERSIST_ENTERPRISE` (3),
//! Flags 0, empty Comment/TargetAlias, no attributes, blob = token as
//! UTF-16LE (no BOM, no NUL terminator). Write preserves existing
//! UserName/TargetAlias/Comment when overwriting; read maps
//! ERROR_NOT_FOUND and any blob decode failure (odd length / invalid
//! UTF-16) to "no token"; delete ignores ERROR_NOT_FOUND.

use std::ffi::c_void;

use async_trait::async_trait;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_ENTERPRISE,
    CRED_TYPE_GENERIC,
};

use crate::model::CredentialBackend;
use crate::store::credentials::{CredentialStore, CredentialStoreError};

/// Service component of the target name (`<user>.<service>`).
pub const KEYRING_SERVICE: &str = "yohaku-companion-win";

pub struct KeyringCredentialStore;

impl KeyringCredentialStore {
    pub fn new() -> Self {
        Self
    }

    /// `"{device_id}.{KEYRING_SERVICE}"` — the windows-native-keyring-store
    /// 1.0.0 default composition `{prefix}{user}{divider}{service}{suffix}`
    /// with `prefix = ""`, `divider = "."`, `suffix = ""`.
    pub fn target_name(device_id: &str) -> String {
        format!("{device_id}.{KEYRING_SERVICE}")
    }
}

impl Default for KeyringCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

/// NUL-terminated UTF-16 buffer for PCWSTR/PWSTR parameters.
fn to_wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The credential blob layout: token as UTF-16LE code units, NO BOM, NO NUL
/// terminator (measured vector in capture-stores §11).
fn token_to_utf16le_blob(token: &str) -> Vec<u8> {
    token.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Blob -> token: odd byte count or invalid UTF-16 -> `None` (the crate's
/// BadEncoding error, which the TS wrapper mapped to null). An empty blob is
/// the empty string.
fn decode_utf16le_blob(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// `CredReadW` + blob decode. EVERY failure (ERROR_NOT_FOUND, access
/// errors, decode failures) reads as "no token" — parity with the TS
/// catch -> null.
fn read_credential(target: &str) -> Option<String> {
    let target_wide = to_wide_nul(target);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `target_wide` is a NUL-terminated UTF-16 buffer that outlives
    // the call; `credential` is a valid out-pointer.
    let read = unsafe {
        CredReadW(
            PCWSTR(target_wide.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut credential,
        )
    };
    if read.is_err() || credential.is_null() {
        return None;
    }
    // SAFETY: on success `credential` points at a CredReadW-allocated
    // CREDENTIALW; the blob pointer/size pair comes from that allocation.
    // It is freed exactly once below, after the bytes are copied out.
    unsafe {
        let blob_size = (*credential).CredentialBlobSize as usize;
        let blob_ptr = (*credential).CredentialBlob;
        let secret = if blob_size == 0 {
            Some(String::new())
        } else if blob_ptr.is_null() {
            None
        } else {
            decode_utf16le_blob(std::slice::from_raw_parts(blob_ptr, blob_size))
        };
        CredFree(credential as *const c_void);
        secret
    }
}

/// `CredWriteW` with the exact measured field table. When a credential with
/// this target already exists, its UserName/TargetAlias/Comment are
/// PRESERVED (windows-native-keyring-store 1.0.0 semantics) — only the blob
/// and Persist are ours; a fresh credential gets `UserName = user`, empty
/// (null) Comment/TargetAlias.
fn write_credential(target: &str, user: &str, token: &str) -> Result<(), CredentialStoreError> {
    let target_wide = to_wide_nul(target);
    let mut target_for_write = target_wide.clone();
    let mut user_wide = to_wide_nul(user);
    let mut blob = token_to_utf16le_blob(token);

    // SAFETY: every pointer stored in the CREDENTIALW below refers to a
    // local buffer (`target_for_write`, `user_wide`, `blob`) or to the
    // CredReadW allocation (`existing`), all of which live until after
    // CredWriteW returns; `existing` is freed exactly once afterwards.
    unsafe {
        let mut existing: *mut CREDENTIALW = std::ptr::null_mut();
        let has_existing = CredReadW(
            PCWSTR(target_wide.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut existing,
        )
        .is_ok()
            && !existing.is_null();

        let (user_name, comment, target_alias) = if has_existing {
            let cred = &*existing;
            (cred.UserName, cred.Comment, cred.TargetAlias)
        } else {
            (PWSTR(user_wide.as_mut_ptr()), PWSTR::null(), PWSTR::null())
        };

        let credential = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target_for_write.as_mut_ptr()),
            Comment: comment,
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_ENTERPRISE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: target_alias,
            UserName: user_name,
            ..Default::default()
        };
        let result = CredWriteW(&credential, 0);
        if has_existing {
            CredFree(existing as *const c_void);
        }
        // Content-free error: the win32 message never contains the token.
        result.map_err(|e| CredentialStoreError::Backend(format!("CredWriteW failed: {e}")))
    }
}

/// Raw `CredDeleteW`; the caller decides how to map failures.
fn delete_credential(target: &str) -> windows::core::Result<()> {
    let target_wide = to_wide_nul(target);
    // SAFETY: NUL-terminated UTF-16 buffer outlives the call.
    unsafe { CredDeleteW(PCWSTR(target_wide.as_ptr()), CRED_TYPE_GENERIC, None) }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    fn backend(&self) -> CredentialBackend {
        CredentialBackend::Keyring
    }

    async fn get(&self, device_id: &str) -> Option<String> {
        read_credential(&Self::target_name(device_id))
    }

    async fn set(&self, device_id: &str, token: &str) -> Result<(), CredentialStoreError> {
        write_credential(&Self::target_name(device_id), device_id, token)
    }

    async fn delete(&self, device_id: &str) -> Result<(), CredentialStoreError> {
        // TS parity: the wrapper swallowed EVERY deletePassword error, not
        // just ERROR_NOT_FOUND ("deleting a missing entry is fine").
        let _ = delete_credential(&Self::target_name(device_id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn target_name_matches_measured_convention() {
        // Verified empirically against @napi-rs/keyring 1.3.0 (spec §9.3).
        assert_eq!(
            KeyringCredentialStore::target_name("__spec_probe__"),
            "__spec_probe__.yohaku-companion-win"
        );
        assert_eq!(
            KeyringCredentialStore::target_name("01J8ME9FZW3W7T2C4Y8K5Q6R9S"),
            "01J8ME9FZW3W7T2C4Y8K5Q6R9S.yohaku-companion-win"
        );
    }

    /// The MEASURED blob vector from capture-stores §11: service
    /// yohaku-companion-win, password "probe-secret-123" -> 32 bytes,
    /// UTF-16LE, no BOM, no NUL terminator.
    #[test]
    fn blob_encoding_matches_measured_vector() {
        let blob = token_to_utf16le_blob("probe-secret-123");
        assert_eq!(blob.len(), 32);
        assert_eq!(
            to_hex(&blob),
            "700072006f00620065002d007300650063007200650074002d00310032003300"
        );
        // Same bytes as the measured getSecret() array.
        assert_eq!(
            blob,
            vec![
                112, 0, 114, 0, 111, 0, 98, 0, 101, 0, 45, 0, 115, 0, 101, 0, 99, 0, 114, 0, 101,
                0, 116, 0, 45, 0, 49, 0, 50, 0, 51, 0
            ]
        );
        // No BOM (FF FE) prefix, no trailing NUL code unit.
        assert_ne!(&blob[0..2], &[0xFF, 0xFE]);
        assert_ne!(&blob[30..32], &[0x00, 0x00]);
    }

    #[test]
    fn blob_decode_round_trips_and_rejects_bad_encodings() {
        for token in ["probe-secret-123", "", "unicode \u{00e9}\u{4e2d}\u{1F600}"] {
            let blob = token_to_utf16le_blob(token);
            assert_eq!(decode_utf16le_blob(&blob).as_deref(), Some(token));
        }
        // Odd byte count -> BadEncoding -> "no token".
        assert_eq!(decode_utf16le_blob(&[0x70]), None);
        assert_eq!(decode_utf16le_blob(&[0x70, 0x00, 0x72]), None);
        // Unpaired surrogate (0xD800 LE) -> invalid UTF-16 -> "no token".
        assert_eq!(decode_utf16le_blob(&[0x00, 0xD8]), None);
        // Empty blob decodes to the empty string (not "no token").
        assert_eq!(decode_utf16le_blob(&[]).as_deref(), Some(""));
    }

    #[test]
    fn wide_strings_are_nul_terminated_without_bom() {
        let wide = to_wide_nul("abc");
        assert_eq!(wide, vec![0x61, 0x62, 0x63, 0x0000]);
    }

    // -- manual round trip against the real Credential Manager --------------

    /// Test-only raw view of a credential for byte-compat assertions.
    struct RawCredential {
        cred_type: u32,
        persist: u32,
        flags: u32,
        attribute_count: u32,
        blob: Vec<u8>,
        user_name: String,
    }

    fn read_raw_credential(target: &str) -> Option<RawCredential> {
        let target_wide = to_wide_nul(target);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: same contract as read_credential; freed exactly once below.
        unsafe {
            CredReadW(
                PCWSTR(target_wide.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
            .ok()?;
            let cred = &*credential;
            let blob = if cred.CredentialBlobSize == 0 || cred.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize)
                    .to_vec()
            };
            let user_name = if cred.UserName.is_null() {
                String::new()
            } else {
                cred.UserName.to_string().unwrap_or_default()
            };
            let raw = RawCredential {
                cred_type: cred.Type.0,
                persist: cred.Persist.0,
                flags: cred.Flags.0,
                attribute_count: cred.AttributeCount,
                blob,
                user_name,
            };
            CredFree(credential as *const c_void);
            Some(raw)
        }
    }

    /// Deletes the throwaway entry even when an assertion panics mid-test.
    struct CleanupGuard(String);

    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            let _ = delete_credential(&self.0);
        }
    }

    /// MANUAL-RUN ONLY (touches the real Windows Credential Manager):
    /// `cargo test -p yohaku-core --lib keyring_manual_round_trip -- --ignored`
    /// Uses a clearly-throwaway target under the non-production service
    /// suffix `yohaku-companion-win-test` — NEVER a real
    /// `*.yohaku-companion-win` entry — and removes it afterwards.
    #[test]
    #[ignore = "creates/deletes a throwaway entry in the real Credential Manager"]
    fn keyring_manual_round_trip() {
        let target = format!(
            "selftest-{}.yohaku-companion-win-test",
            uuid::Uuid::new_v4()
        );
        let _cleanup = CleanupGuard(target.clone());
        let token = "probe-secret-123";

        write_credential(&target, "selftest-user", token).expect("write");
        assert_eq!(read_credential(&target).as_deref(), Some(token));

        // Byte-compat field table (measured vector, spec §9.3/§11).
        let raw = read_raw_credential(&target).expect("raw read");
        assert_eq!(raw.cred_type, 1, "CRED_TYPE_GENERIC");
        assert_eq!(raw.persist, 3, "CRED_PERSIST_ENTERPRISE");
        assert_eq!(raw.flags, 0);
        assert_eq!(raw.attribute_count, 0);
        assert_eq!(raw.user_name, "selftest-user");
        assert_eq!(
            to_hex(&raw.blob),
            "700072006f00620065002d007300650063007200650074002d00310032003300"
        );

        // Overwrite preserves the existing UserName (crate semantics).
        write_credential(&target, "different-user", "second-token").expect("overwrite");
        assert_eq!(read_credential(&target).as_deref(), Some("second-token"));
        let raw = read_raw_credential(&target).expect("raw read after overwrite");
        assert_eq!(
            raw.user_name, "selftest-user",
            "UserName preserved on overwrite"
        );

        // Delete succeeds once, then reports ERROR_NOT_FOUND; reads say None.
        delete_credential(&target).expect("delete");
        assert_eq!(read_credential(&target), None);
        let second = delete_credential(&target);
        assert_eq!(
            second.unwrap_err().code(),
            windows::core::HRESULT::from_win32(windows::Win32::Foundation::ERROR_NOT_FOUND.0),
            "second delete reports ERROR_NOT_FOUND (swallowed by the trait impl)"
        );
    }
}
