use chrono::{DateTime, SecondsFormat, Utc};
use codexbar::build_identity::{BuildIdentity, DistributionKind};
use codexbar::install_ownership::classify_distribution_for_path;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeIdentity {
    pub build: BuildIdentity,
    pub executable_path: PathBuf,
    pub executable_sha256: String,
    pub pid: u32,
    pub process_started_at: String,
    pub distribution: DistributionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeIdentityStage {
    Locate,
    Canonicalize,
    Open,
    Read,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeIdentityError {
    stage: RuntimeIdentityStage,
    kind: io::ErrorKind,
}

impl RuntimeIdentityError {
    pub fn stage(&self) -> RuntimeIdentityStage {
        self.stage
    }

    pub fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    fn new(stage: RuntimeIdentityStage, error: io::Error) -> Self {
        Self {
            stage,
            kind: error.kind(),
        }
    }
}

impl fmt::Display for RuntimeIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "runtime executable identity failed at {:?} ({:?})",
            self.stage, self.kind
        )
    }
}

impl std::error::Error for RuntimeIdentityError {}

impl RuntimeIdentity {
    pub fn capture_current(
        process_started_at: DateTime<Utc>,
    ) -> Result<Self, RuntimeIdentityError> {
        let executable = std::env::current_exe()
            .map_err(|error| RuntimeIdentityError::new(RuntimeIdentityStage::Locate, error))?;
        Self::capture_from(executable, std::process::id(), process_started_at)
    }

    pub fn capture_from(
        executable: impl AsRef<Path>,
        pid: u32,
        process_started_at: DateTime<Utc>,
    ) -> Result<Self, RuntimeIdentityError> {
        let canonical_executable_path = fs::canonicalize(executable.as_ref()).map_err(|error| {
            RuntimeIdentityError::new(RuntimeIdentityStage::Canonicalize, error)
        })?;
        let executable_sha256 = hash_file(&canonical_executable_path)?;
        let build = BuildIdentity::compiled();
        let distribution = classify_distribution(&canonical_executable_path, &build);
        let executable_path = reported_executable_path(&canonical_executable_path);

        Ok(Self {
            build,
            executable_path,
            executable_sha256,
            pid,
            process_started_at: process_started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            distribution,
        })
    }
}

fn hash_file(path: &Path) -> Result<String, RuntimeIdentityError> {
    let mut file = File::open(path)
        .map_err(|error| RuntimeIdentityError::new(RuntimeIdentityStage::Open, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| RuntimeIdentityError::new(RuntimeIdentityStage::Read, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn classify_distribution(executable: &Path, build: &BuildIdentity) -> DistributionKind {
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    classify_distribution_with_local_app_data(executable, build, local_app_data.as_deref())
}

fn classify_distribution_with_local_app_data(
    executable: &Path,
    build: &BuildIdentity,
    local_app_data: Option<&Path>,
) -> DistributionKind {
    classify_distribution_for_path(executable, build, local_app_data.unwrap_or_else(|| Path::new("")))
}

#[cfg(target_os = "windows")]
fn reported_executable_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let verbatim_prefix = ['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16];
    if !wide.starts_with(&verbatim_prefix) {
        return path.to_path_buf();
    }

    let unc_prefix = ['U' as u16, 'N' as u16, 'C' as u16, '\\' as u16];
    let stripped = if wide[verbatim_prefix.len()..].starts_with(&unc_prefix) {
        let mut value = vec!['\\' as u16, '\\' as u16];
        value.extend_from_slice(&wide[(verbatim_prefix.len() + unc_prefix.len())..]);
        value
    } else {
        wide[verbatim_prefix.len()..].to_vec()
    };
    PathBuf::from(OsString::from_wide(&stripped))
}

#[cfg(not(target_os = "windows"))]
fn reported_executable_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn startup_failure_message(error: &RuntimeIdentityError) -> String {
    format!(
        "CodexBar could not verify the program file that is currently running.\n\nFailure stage: {:?}\nOperating-system error: {:?}\n\nClose CodexBar and reinstall it from the verified installer. The application will not continue with an unknown executable identity.",
        error.stage(),
        error.kind()
    )
}

pub fn report_fatal_startup_error(error: RuntimeIdentityError) -> ! {
    tracing::error!(
        stage = ?error.stage(),
        error_kind = ?error.kind(),
        "startup aborted because the runtime executable identity could not be captured"
    );
    let message = startup_failure_message(&error);
    report_fatal_startup_message(&message)
}

/// Display a local startup failure before Tauri and any user storage are
/// initialized. Callers must supply a message that is safe for local display
/// and must not include credentials or command-line arguments.
pub fn report_fatal_startup_message(message: &str) -> ! {
    show_startup_error(message);
    std::process::exit(1)
}

#[cfg(target_os = "windows")]
fn show_startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };

    let title = "CodexBar startup error"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let message = message
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_startup_error(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeIdentity, RuntimeIdentityStage, classify_distribution_with_local_app_data,
        startup_failure_message,
    };
    use chrono::{TimeZone, Utc};
    use codexbar::build_identity::{BuildChannel, BuildIdentity, DistributionKind};
    use std::fs;
    use std::path::Path;

    #[test]
    fn hashes_the_exact_executable_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "codexbar-runtime-identity-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("codexbar-desktop-tauri.exe");
        fs::write(&executable, b"identity-fixture").unwrap();

        let started_at = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let identity = RuntimeIdentity::capture_from(&executable, 42, started_at).unwrap();

        assert_eq!(identity.pid, 42);
        assert!(identity.executable_path.is_absolute());
        assert_eq!(
            identity.executable_sha256,
            "67724f64a48454ec0763e5771e7ed7d8bbd26770141c6f98c9a88453057b0c2a"
        );
        assert_eq!(identity.process_started_at, "2026-08-30T10:00:00.000Z");
        assert_eq!(identity.distribution, DistributionKind::Unknown);

        fs::remove_file(executable).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn classifies_known_install_portable_and_development_paths() {
        let local_app_data = Path::new(r"C:\Users\Example\AppData\Local");
        let mut build = BuildIdentity::compiled();
        build.build_channel = BuildChannel::Stable;

        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::LegacyInstalled
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"\\?\C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::LegacyInstalled
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(
                    r"C:\Users\Example\AppData\Local\Programs\CodexBar\v2\codexbar.exe"
                ),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::InstalledStable
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar-desktop.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::LegacyInstalled
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"D:\Tools\CodexBar-0.46.0-portable.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::Portable
        );

        build.build_channel = BuildChannel::Dev;
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"D:\src\CodexBar\target\debug\codexbar-desktop-tauri.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::Development
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(
                    r"D:\src\CodexBar\target\x86_64-pc-windows-msvc\release\codexbar-desktop-tauri.exe"
                ),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::Development
        );
        assert_eq!(
            classify_distribution_with_local_app_data(
                Path::new(r"D:\scratch\codexbar-desktop-tauri.exe"),
                &build,
                Some(local_app_data)
            ),
            DistributionKind::Unknown
        );
    }

    #[test]
    fn capture_errors_and_startup_message_do_not_expose_the_input_path() {
        let secret_marker = format!("private-user-folder-{}", uuid::Uuid::new_v4());
        let missing = std::env::temp_dir()
            .join(&secret_marker)
            .join("missing.exe");
        let started_at = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();

        let error = RuntimeIdentity::capture_from(&missing, 42, started_at).unwrap_err();

        assert_eq!(error.stage(), RuntimeIdentityStage::Canonicalize);
        assert!(!error.to_string().contains(&secret_marker));
        let message = startup_failure_message(&error);
        assert!(!message.contains(&secret_marker));
        assert!(message.contains("CodexBar"));
        assert!(message.contains("reinstall"));
    }
}
