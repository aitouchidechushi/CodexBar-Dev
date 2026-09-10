//! Small helper for storing local secret-bearing JSON files.

use std::io;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::storage::{Protection, RealAtomicFileOps, StoreKind, write_bytes_verified_for};

const FORMAT: &str = "codexbar.secure-file";
const VERSION: u32 = 1;
const WINDOWS_DPAPI_USER: &str = "windows-dpapi-user";
const WINDOWS_DPAPI_MACHINE: &str = "windows-dpapi-machine";

#[derive(Debug, Serialize, Deserialize)]
struct ProtectedFile {
    format: String,
    version: u32,
    protection: String,
    payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecureFileStatus {
    Missing,
    Plaintext,
    Protected(String),
    Unreadable(String),
}

/// Return a non-secret storage status for diagnostics/UI surfaces.
pub fn status(path: &Path) -> SecureFileStatus {
    if !path.exists() {
        return SecureFileStatus::Missing;
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => {
            return SecureFileStatus::Unreadable(format!("io-error:{:?}", error.kind()));
        }
    };

    let Ok(file) = serde_json::from_str::<ProtectedFile>(&raw) else {
        return SecureFileStatus::Plaintext;
    };

    if file.format != FORMAT {
        return SecureFileStatus::Plaintext;
    }
    if file.version != VERSION {
        return SecureFileStatus::Unreadable(format!(
            "unsupported secure file version {}",
            file.version
        ));
    }

    match file.protection.as_str() {
        WINDOWS_DPAPI_USER => SecureFileStatus::Protected(file.protection),
        WINDOWS_DPAPI_MACHINE => {
            SecureFileStatus::Unreadable("unsupported-machine-scope".to_string())
        }
        _ => SecureFileStatus::Unreadable("unsupported-protection".to_string()),
    }
}

/// Read a UTF-8 file that may be protected by this module.
pub fn read_string(path: &Path) -> io::Result<String> {
    read_string_with(path, &PlatformSecretProtector)
}

pub fn read_string_with(path: &Path, protector: &dyn SecretProtector) -> io::Result<String> {
    let raw = std::fs::read(path)?;
    decode_string_bytes_with(&raw, protector)
}

/// Decode in-memory bytes produced by [`protected_file_bytes_with`].
///
/// Typed StorageV2 repositories use this during migration verification before
/// a destination file is exposed to consumers.
pub fn decode_string_bytes_with(raw: &[u8], protector: &dyn SecretProtector) -> io::Result<String> {
    let raw = std::str::from_utf8(raw)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid utf-8 file"))?;
    let Ok(file) = serde_json::from_str::<ProtectedFile>(raw) else {
        return Ok(raw.to_string());
    };

    if file.format != FORMAT {
        return Ok(raw.to_string());
    }
    if file.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported secure file version {}", file.version),
        ));
    }

    match file.protection.as_str() {
        WINDOWS_DPAPI_USER => {
            let encrypted = base64::engine::general_purpose::STANDARD
                .decode(file.payload.as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid payload"))?;
            let plain = protector.unprotect_current_user(&encrypted)?;
            String::from_utf8(plain)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid utf-8 payload"))
        }
        WINDOWS_DPAPI_MACHINE => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "historical machine-scope protection is not supported",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported secure file protection",
        )),
    }
}

/// Compatibility write entrypoint for stores that have not yet migrated to a
/// typed StorageV2 repository. Protection is completed before any file is
/// created, then the bytes go through the same atomic verified transaction.
pub fn write_string(path: &Path, contents: &str) -> io::Result<()> {
    write_string_with(
        path,
        contents,
        default_protection(),
        &PlatformSecretProtector,
    )
}

pub fn write_string_with(
    path: &Path,
    contents: &str,
    protection: Protection,
    protector: &dyn SecretProtector,
) -> io::Result<()> {
    let bytes = protected_file_bytes_with(contents, protection, protector)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    restrict_directory_permissions(parent)?;
    write_bytes_verified_for(StoreKind::Unspecified, path, &bytes, &RealAtomicFileOps)
        .map_err(|error| io::Error::other(error.to_string()))?;
    restrict_file_permissions(path)?;
    Ok(())
}

/// Pure capability used by StorageV2: turn a string into protected bytes
/// without creating, truncating, replacing, or chmod'ing any file.
pub fn protected_file_bytes(contents: &str) -> io::Result<Vec<u8>> {
    protected_file_bytes_with(contents, default_protection(), &PlatformSecretProtector)
}

pub fn protected_file_bytes_with(
    contents: &str,
    protection: Protection,
    protector: &dyn SecretProtector,
) -> io::Result<Vec<u8>> {
    match protection {
        Protection::Plaintext => Ok(contents.as_bytes().to_vec()),
        Protection::CurrentUserDpapi => {
            let encrypted = protector.protect_current_user(contents.as_bytes())?;
            let file = ProtectedFile {
                format: FORMAT.to_string(),
                version: VERSION,
                protection: WINDOWS_DPAPI_USER.to_string(),
                payload: base64::engine::general_purpose::STANDARD.encode(encrypted),
            };
            serde_json::to_vec_pretty(&file).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "could not serialize secure wrapper",
                )
            })
        }
    }
}

/// Apply the platform's private-file permissions to a typed storage file.
pub fn restrict_storage_file(path: &Path) -> io::Result<()> {
    restrict_file_permissions(path)
}

pub fn restrict_storage_directory(path: &Path) -> io::Result<()> {
    restrict_directory_permissions(path)
}

#[cfg(windows)]
pub(crate) const fn default_protection() -> Protection {
    Protection::CurrentUserDpapi
}

#[cfg(not(windows))]
pub(crate) const fn default_protection() -> Protection {
    Protection::Plaintext
}

#[cfg(windows)]
impl SecretProtector for PlatformSecretProtector {
    fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
        dpapi_protect_current_user(plain)
    }

    fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
        dpapi_unprotect_current_user(encrypted)
    }
}

#[cfg(not(windows))]
impl SecretProtector for PlatformSecretProtector {
    fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "current-user DPAPI is only available on Windows",
        ))
    }

    fn unprotect_current_user(&self, _encrypted: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "current-user DPAPI is only available on Windows",
        ))
    }
}

#[cfg(windows)]
fn dpapi_protect_current_user(plain: &[u8]) -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let plain_length = u32::try_from(plain.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload is too large"))?;

    unsafe {
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: plain_length,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptProtectData(
            &input_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
        .map_err(|_| io::Error::last_os_error())?;

        if output_blob.pbData.is_null() {
            return Err(io::Error::other("CryptProtectData returned null output"));
        }

        let encrypted =
            std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output_blob.pbData as *mut _));
        Ok(encrypted)
    }
}

#[cfg(windows)]
fn dpapi_unprotect_current_user(encrypted: &[u8]) -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    let encrypted_length = u32::try_from(encrypted.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload is too large"))?;

    unsafe {
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: encrypted_length,
            pbData: encrypted.as_ptr() as *mut u8,
        };
        let mut output_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptUnprotectData(
            &input_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
        .map_err(|_| io::Error::last_os_error())?;

        if output_blob.pbData.is_null() {
            return Err(io::Error::other("CryptUnprotectData returned null output"));
        }

        let plain =
            std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output_blob.pbData as *mut _));
        Ok(plain)
    }
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
}

#[cfg(windows)]
fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    restrict_windows_permissions(path, false)
}

#[cfg(windows)]
fn restrict_directory_permissions(path: &Path) -> io::Result<()> {
    restrict_windows_permissions(path, true)
}

#[cfg(windows)]
fn restrict_windows_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };
    use windows::core::PCWSTR;

    let current_user_sid = current_user_sid_string()?;
    let sddl = if directory {
        format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{current_user_sid})")
    } else {
        format!("D:P(A;;FA;;;SY)(A;;FA;;;{current_user_sid})")
    };
    let sddl = std::ffi::OsStr::new(&sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();

    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|_| io::Error::last_os_error())?;

        let result = SetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        let error = (!result.as_bool()).then(io::Error::last_os_error);
        let _ = LocalFree(HLOCAL(descriptor.0));
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(windows)]
fn current_user_sid_string() -> io::Result<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PWSTR;

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|_| io::Error::last_os_error())?;
        let token = TokenHandle(token);
        let mut required = 0_u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut required);
        if required == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut token_buffer = vec![0_u8; required as usize];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(token_buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
        .map_err(|_| io::Error::last_os_error())?;
        let token_user = std::ptr::read_unaligned(token_buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid_string = PWSTR::null();
        ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string)
            .map_err(|_| io::Error::last_os_error())?;
        let result = sid_string
            .to_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid user SID"));
        let _ = LocalFree(HLOCAL(sid_string.0.cast()));
        result
    }
}

#[cfg(all(not(unix), not(windows)))]
fn restrict_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn restrict_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
fn windows_broad_write_principals(path: &Path) -> io::Result<Vec<&'static str>> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, IsWellKnownSid,
        PSECURITY_DESCRIPTOR, PSID, WinBuiltinUsersSid, WinWorldSid,
    };
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_GENERIC_WRITE, FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER,
    };
    use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
    use windows::core::PCWSTR;

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut dacl = std::ptr::null_mut::<ACL>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.0 as i32));
    }

    let result = (|| {
        if dacl.is_null() {
            return Ok(vec!["NoDacl"]);
        }

        let mut principals = Vec::new();
        let ace_count = unsafe { (*dacl).AceCount };
        let write_mask =
            FILE_GENERIC_WRITE.0 | FILE_WRITE_DATA.0 | WRITE_DAC.0 | WRITE_OWNER.0 | DELETE.0;
        for index in 0..u32::from(ace_count) {
            let mut raw_ace = std::ptr::null_mut();
            unsafe { GetAce(dacl, index, &mut raw_ace) }.map_err(|_| io::Error::last_os_error())?;
            let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                continue;
            }

            let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
            if ace.Mask & write_mask == 0 {
                continue;
            }
            let sid = PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast());
            if unsafe { IsWellKnownSid(sid, WinWorldSid) }.as_bool() {
                principals.push("Everyone");
            }
            if unsafe { IsWellKnownSid(sid, WinBuiltinUsersSid) }.as_bool() {
                principals.push("Users");
            }
        }
        principals.sort_unstable();
        principals.dedup();
        Ok(principals)
    })();

    unsafe {
        let _ = LocalFree(HLOCAL(descriptor.0));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Protection;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FailingCurrentUserProtector {
        current_user_calls: AtomicUsize,
        machine_scope_calls: AtomicUsize,
    }

    impl SecretProtector for FailingCurrentUserProtector {
        fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
            self.current_user_calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::from_raw_os_error(5))
        }

        fn unprotect_current_user(&self, _encrypted: &[u8]) -> io::Result<Vec<u8>> {
            panic!("machine-scope wrapper must be rejected before unprotect")
        }
    }

    impl FailingCurrentUserProtector {
        #[allow(dead_code)]
        fn protect_machine_scope(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
            self.machine_scope_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    #[test]
    fn current_user_protection_failure_is_returned_without_machine_fallback() {
        let protector = FailingCurrentUserProtector::default();
        let error = protected_file_bytes_with(
            "fixture-secret-never-print",
            Protection::CurrentUserDpapi,
            &protector,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(protector.current_user_calls.load(Ordering::SeqCst), 1);
        assert_eq!(protector.machine_scope_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_current_user_protection_does_not_create_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secure.json");
        let protector = FailingCurrentUserProtector::default();

        let error = write_string_with(
            &path,
            "fixture-secret-never-print",
            Protection::CurrentUserDpapi,
            &protector,
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(5));
        assert!(!path.exists());
        assert_eq!(protector.machine_scope_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reads_plaintext_json_without_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.json");
        std::fs::write(&path, r#"{"hello":"world"}"#).unwrap();

        assert_eq!(read_string(&path).unwrap(), r#"{"hello":"world"}"#);
    }

    #[test]
    fn write_roundtrips_on_this_platform() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();

        assert_eq!(read_string(&path).unwrap(), r#"{"secret":"value"}"#);
    }

    #[test]
    fn status_reports_missing_plaintext_and_protected_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert_eq!(status(&missing), SecureFileStatus::Missing);

        let plain = dir.path().join("plain.json");
        std::fs::write(&plain, r#"{"secret":"value"}"#).unwrap();
        assert_eq!(status(&plain), SecureFileStatus::Plaintext);

        let protected = dir.path().join("protected.json");
        std::fs::write(
            &protected,
            serde_json::to_string(&ProtectedFile {
                format: FORMAT.to_string(),
                version: VERSION,
                protection: WINDOWS_DPAPI_USER.to_string(),
                payload: "AA==".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            status(&protected),
            SecureFileStatus::Protected(WINDOWS_DPAPI_USER.to_string())
        );
    }

    #[test]
    fn status_reports_unsupported_wrappers_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("protected.json");
        std::fs::write(
            &path,
            serde_json::to_string(&ProtectedFile {
                format: FORMAT.to_string(),
                version: VERSION + 1,
                protection: WINDOWS_DPAPI_USER.to_string(),
                payload: "AA==".to_string(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(status(&path), SecureFileStatus::Unreadable(_)));
    }

    #[test]
    fn historical_machine_scope_is_rejected_without_decrypting_or_rewriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("machine-scope.json");
        let original = serde_json::to_vec(&ProtectedFile {
            format: FORMAT.to_string(),
            version: VERSION,
            protection: WINDOWS_DPAPI_MACHINE.to_string(),
            payload: "AA==".to_string(),
        })
        .unwrap();
        std::fs::write(&path, &original).unwrap();
        let protector = FailingCurrentUserProtector::default();

        let error = read_string_with(&path, &protector).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(protector.current_user_calls.load(Ordering::SeqCst), 0);
        assert!(matches!(status(&path), SecureFileStatus::Unreadable(_)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_write_uses_protected_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let file: ProtectedFile = serde_json::from_str(&raw).unwrap();

        assert_eq!(file.format, FORMAT);
        assert_eq!(file.version, VERSION);
        assert_eq!(file.protection, WINDOWS_DPAPI_USER);
        assert!(
            !raw.contains("secret") && !raw.contains("value"),
            "protected Windows file must not contain plaintext JSON"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_sensitive_file_and_directory_exclude_broad_write_principals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();
        write_string(&path, r#"{"secret":"replacement"}"#).unwrap();
        let backup = crate::storage::backup_path_for(&path);

        assert_eq!(
            windows_broad_write_principals(directory.path()).unwrap(),
            Vec::<&'static str>::new()
        );
        assert_eq!(
            windows_broad_write_principals(&path).unwrap(),
            Vec::<&'static str>::new()
        );
        assert_eq!(
            windows_broad_write_principals(&backup).unwrap(),
            Vec::<&'static str>::new()
        );
        assert!(!String::from_utf8_lossy(&std::fs::read(backup).unwrap()).contains("value"));
    }
}

/// Injectable current-user secret protection boundary. There is deliberately
/// no machine-scope method, so callers cannot silently weaken the boundary.
pub trait SecretProtector: Send + Sync {
    fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>>;
    fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlatformSecretProtector;
