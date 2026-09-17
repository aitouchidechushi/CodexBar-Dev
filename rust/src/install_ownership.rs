use crate::build_identity::{BuildChannel, BuildIdentity, DistributionKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const CANONICAL_INSTALL_DIRECTORY: &str = "Programs\\CodexBar\\v2";
pub const CANONICAL_LAUNCHER_FILE_NAME: &str = "codexbar.exe";
pub const CANONICAL_APP_ID: &str = "{6F73B2E3-8E5B-4A2D-9E7B-0C46A0B2F001}";
pub const CANONICAL_RUN_VALUE: &str = "CodexBarStableV2";
pub const LEGACY_RUN_VALUE: &str = "CodexBar";
pub const INSTALL_REGISTRATION_SUBKEY: &str = r"Software\CodexBar\InstallV2";
pub const INSTALL_REGISTRATION_SCHEMA_VALUE: &str = "SchemaVersion";
pub const INSTALL_REGISTRATION_ROOT_VALUE: &str = "InstallRoot";
pub const INSTALL_REGISTRATION_LAUNCHER_VALUE: &str = "LauncherPath";
pub const INSTALL_REGISTRATION_APP_ID_VALUE: &str = "AppId";
pub const INSTALL_REGISTRATION_VERSION_VALUE: &str = "InstalledVersion";
pub const INSTALL_REGISTRATION_COMMIT_VALUE: &str = "ExpectedCommit";
pub const INSTALL_REGISTRATION_MANIFEST_VALUE: &str = "ExpectedManifestSha256";
pub const INSTALL_REGISTRATION_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRegistration {
    /// The original installer-reported path is retained for safe diagnostics.
    pub install_root: PathBuf,
    pub launcher_path: PathBuf,
    pub app_id: String,
    pub installed_version: String,
    pub expected_commit: String,
    pub expected_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInstallRegistration {
    registration: InstallRegistration,
    launcher_path: PathBuf,
}

impl VerifiedInstallRegistration {
    pub fn registration(&self) -> &InstallRegistration {
        &self.registration
    }

    pub fn launcher_path(&self) -> &Path {
        &self.launcher_path
    }

    pub fn startup_command(&self) -> String {
        format!("\"{}\" --startup", self.launcher_path.display())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOwnershipErrorKind {
    RegistrationMissing,
    RegistrationUnreadable,
    RegistrationSchemaUnsupported,
    BuildIsNotStable,
    RegistrationIdentityMismatch,
    RegistrationPathInvalid,
    CanonicalInstallUnavailable,
    CanonicalLauncherUnavailable,
    CurrentExecutableUnavailable,
    CurrentExecutableIsNotCanonical,
    RunValueUnreadable,
    RunValueWriteFailed,
    ForeignRunOwnership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallOwnershipError {
    kind: InstallOwnershipErrorKind,
}

impl InstallOwnershipError {
    pub fn kind(&self) -> InstallOwnershipErrorKind {
        self.kind
    }

    fn new(kind: InstallOwnershipErrorKind) -> Self {
        Self { kind }
    }
}

impl fmt::Display for InstallOwnershipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self.kind {
            InstallOwnershipErrorKind::RegistrationMissing => {
                "the CodexBar install registration is missing"
            }
            InstallOwnershipErrorKind::RegistrationUnreadable => {
                "the CodexBar install registration cannot be read safely"
            }
            InstallOwnershipErrorKind::RegistrationSchemaUnsupported => {
                "the CodexBar install registration has an unsupported schema"
            }
            InstallOwnershipErrorKind::BuildIsNotStable => {
                "the running build is not a stable installed build"
            }
            InstallOwnershipErrorKind::RegistrationIdentityMismatch => {
                "the install registration does not match the running build identity"
            }
            InstallOwnershipErrorKind::RegistrationPathInvalid => {
                "the install registration does not describe the canonical launcher"
            }
            InstallOwnershipErrorKind::CanonicalInstallUnavailable => {
                "the canonical install directory is unavailable"
            }
            InstallOwnershipErrorKind::CanonicalLauncherUnavailable => {
                "the canonical launcher is unavailable"
            }
            InstallOwnershipErrorKind::CurrentExecutableUnavailable => {
                "the running executable cannot be canonicalized"
            }
            InstallOwnershipErrorKind::CurrentExecutableIsNotCanonical => {
                "the running executable is not the canonical launcher"
            }
            InstallOwnershipErrorKind::RunValueUnreadable => {
                "the Windows startup entry cannot be read safely"
            }
            InstallOwnershipErrorKind::RunValueWriteFailed => {
                "the Windows startup entry could not be updated"
            }
            InstallOwnershipErrorKind::ForeignRunOwnership => {
                "a startup entry is owned by another program"
            }
        };
        formatter.write_str(description)
    }
}

impl std::error::Error for InstallOwnershipError {}

/// Read-only boundary around the install registration written by the Inno
/// installer. It permits in-memory tests without allowing arbitrary paths to
/// impersonate an installed application.
pub trait InstallRegistrationStore {
    fn read_registration_value(
        &self,
        name: &str,
    ) -> Result<Option<String>, InstallOwnershipError>;
}

pub fn load_install_registration(
    registry: &impl InstallRegistrationStore,
) -> Result<InstallRegistration, InstallOwnershipError> {
    let schema_version = required_registration_value(registry, INSTALL_REGISTRATION_SCHEMA_VALUE)?;
    if schema_version != INSTALL_REGISTRATION_SCHEMA_VERSION {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::RegistrationSchemaUnsupported,
        ));
    }

    Ok(InstallRegistration {
        install_root: PathBuf::from(required_registration_value(
            registry,
            INSTALL_REGISTRATION_ROOT_VALUE,
        )?),
        launcher_path: PathBuf::from(required_registration_value(
            registry,
            INSTALL_REGISTRATION_LAUNCHER_VALUE,
        )?),
        app_id: required_registration_value(registry, INSTALL_REGISTRATION_APP_ID_VALUE)?,
        installed_version: required_registration_value(
            registry,
            INSTALL_REGISTRATION_VERSION_VALUE,
        )?,
        expected_commit: required_registration_value(
            registry,
            INSTALL_REGISTRATION_COMMIT_VALUE,
        )?,
        expected_manifest_sha256: required_registration_value(
            registry,
            INSTALL_REGISTRATION_MANIFEST_VALUE,
        )?,
    })
}

fn required_registration_value(
    registry: &impl InstallRegistrationStore,
    name: &str,
) -> Result<String, InstallOwnershipError> {
    registry
        .read_registration_value(name)
        .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RegistrationUnreadable))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| InstallOwnershipError::new(InstallOwnershipErrorKind::RegistrationMissing))
}

#[cfg(target_os = "windows")]
pub struct WindowsInstallRegistrationStore {
    key: winreg::RegKey,
}

#[cfg(target_os = "windows")]
impl WindowsInstallRegistrationStore {
    pub fn open_current_user() -> Result<Self, InstallOwnershipError> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu
            .open_subkey_with_flags(INSTALL_REGISTRATION_SUBKEY, KEY_READ)
            .map_err(|error| {
                InstallOwnershipError::new(if error.kind() == std::io::ErrorKind::NotFound {
                    InstallOwnershipErrorKind::RegistrationMissing
                } else {
                    InstallOwnershipErrorKind::RegistrationUnreadable
                })
            })?;
        Ok(Self { key })
    }
}

#[cfg(target_os = "windows")]
impl InstallRegistrationStore for WindowsInstallRegistrationStore {
    fn read_registration_value(
        &self,
        name: &str,
    ) -> Result<Option<String>, InstallOwnershipError> {
        match self.key.get_value::<String, _>(name) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(InstallOwnershipError::new(
                InstallOwnershipErrorKind::RegistrationUnreadable,
            )),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn read_windows_install_registration() -> Result<InstallRegistration, InstallOwnershipError> {
    let registry = WindowsInstallRegistrationStore::open_current_user()?;
    load_install_registration(&registry)
}

/// Minimal boundary around the HKCU Run key. The production Windows adapter and
/// tests both use this boundary, so the ownership decision remains independent
/// from the host registry.
pub trait RunValueStore {
    fn read_value(&self, name: &str) -> Result<Option<String>, InstallOwnershipError>;
    fn write_value(&mut self, name: &str, value: &str) -> Result<(), InstallOwnershipError>;
    fn delete_value(&mut self, name: &str) -> Result<(), InstallOwnershipError>;
}

#[cfg(target_os = "windows")]
pub struct WindowsRunValueStore {
    key: Option<winreg::RegKey>,
}

#[cfg(target_os = "windows")]
impl WindowsRunValueStore {
    pub fn open_current_user_read_only() -> Result<Self, InstallOwnershipError> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        let key = match winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_READ) {
            Ok(key) => Some(key),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueUnreadable)),
        };
        Ok(Self { key })
    }

    pub fn open_current_user() -> Result<Self, InstallOwnershipError> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey_with_flags(
                r"Software\Microsoft\Windows\CurrentVersion\Run",
                KEY_READ | KEY_WRITE,
            )
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueUnreadable))?;
        Ok(Self { key: Some(key) })
    }
}

#[cfg(target_os = "windows")]
impl RunValueStore for WindowsRunValueStore {
    fn read_value(&self, name: &str) -> Result<Option<String>, InstallOwnershipError> {
        let Some(key) = &self.key else { return Ok(None); };
        match key.get_value::<String, _>(name) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(InstallOwnershipError::new(
                InstallOwnershipErrorKind::RunValueUnreadable,
            )),
        }
    }

    fn write_value(&mut self, name: &str, value: &str) -> Result<(), InstallOwnershipError> {
        self.key.as_ref()
            .ok_or_else(|| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))?
            .set_value(name, &value)
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))
    }

    fn delete_value(&mut self, name: &str) -> Result<(), InstallOwnershipError> {
        let Some(key) = &self.key else { return Ok(()); };
        match key.delete_value(name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(InstallOwnershipError::new(
                InstallOwnershipErrorKind::RunValueWriteFailed,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOwnershipState {
    Disabled,
    OwnedCanonical,
    StaleCodexBar { target: PathBuf },
    Foreign { command_hash: String },
    Unreadable,
}

#[cfg(target_os = "windows")]
pub fn verify_running_install() -> anyhow::Result<VerifiedInstallRegistration> {
    let registration = read_windows_install_registration()?;
    let local = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("Windows local application directory is unavailable"))?;
    Ok(verify_current_install(
        &registration,
        &local,
        &std::env::current_exe()?,
        &BuildIdentity::compiled(),
    )?)
}

/// The registry is authoritative; the historical settings boolean is not an
/// observation of Windows startup state and must never be presented as one.
pub fn current_start_at_login_state() -> anyhow::Result<RunOwnershipState> {
    #[cfg(target_os = "windows")]
    {
        let registration = read_windows_install_registration()?;
        let local = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("Windows local application directory is unavailable"))?;
        let registry = WindowsRunValueStore::open_current_user_read_only()?;
        Ok(read_start_at_login_state_for_executable(
            &registration,
            &local,
            &std::env::current_exe()?,
            &BuildIdentity::compiled(),
            &registry,
        )?)
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!("Auto-start is only supported on Windows")
    }
}

#[cfg(any(target_os = "windows", test))]
fn read_start_at_login_state_for_executable(
    registration: &InstallRegistration,
    local_app_data: &Path,
    current_executable: &Path,
    build: &BuildIdentity,
    registry: &impl RunValueStore,
) -> Result<RunOwnershipState, InstallOwnershipError> {
    let verified = match verify_current_install(
        registration, local_app_data, current_executable, build,
    ) {
        Ok(verified) => verified,
        Err(error) if error.kind() == InstallOwnershipErrorKind::CurrentExecutableIsNotCanonical => {
            // A matching installed CLI may observe startup state, but must not
            // receive the ownership token used by mutation APIs.
            let expected_root = canonicalize(
                &local_app_data.join("Programs").join("CodexBar").join("v2"),
                InstallOwnershipErrorKind::CanonicalInstallUnavailable,
            )?;
            let current = canonicalize(
                current_executable,
                InstallOwnershipErrorKind::CurrentExecutableUnavailable,
            )?;
            if !paths_equal(&current, &expected_root.join("codexbar-cli.exe")) {
                return Err(error);
            }
            verify_current_install(registration, local_app_data, &registration.launcher_path, build)?
        }
        Err(error) => return Err(error),
    };
    Ok(read_start_at_login_state(&verified, registry))
}

pub fn current_start_at_login_enabled() -> anyhow::Result<bool> {
    match current_start_at_login_state()? {
        RunOwnershipState::OwnedCanonical => Ok(true),
        RunOwnershipState::Disabled => Ok(false),
        RunOwnershipState::StaleCodexBar { .. } => anyhow::bail!("A legacy CodexBar startup entry needs repair"),
        RunOwnershipState::Foreign { .. } => anyhow::bail!("A startup entry is owned by another program"),
        RunOwnershipState::Unreadable => anyhow::bail!("The Windows startup entries cannot be read"),
    }
}

enum RunValueClassification {
    Missing,
    Canonical,
    Legacy(PathBuf),
    Foreign(String),
    Unreadable,
}

/// Read the current ownership state without changing any system setting.
pub fn read_start_at_login_state(
    registration: &VerifiedInstallRegistration,
    registry: &impl RunValueStore,
) -> RunOwnershipState {
    let canonical = classify_run_value(
        registry.read_value(CANONICAL_RUN_VALUE),
        registration,
    );
    let legacy = classify_run_value(registry.read_value(LEGACY_RUN_VALUE), registration);

    // Both entries are acted on by Windows. A valid v2 entry must not hide an
    // unreadable, foreign, or stale legacy entry that can start another copy.
    match (canonical, legacy) {
        (RunValueClassification::Unreadable, _) | (_, RunValueClassification::Unreadable) => {
            RunOwnershipState::Unreadable
        }
        (RunValueClassification::Foreign(command_hash), _)
        | (_, RunValueClassification::Foreign(command_hash)) => {
            RunOwnershipState::Foreign { command_hash }
        }
        (RunValueClassification::Legacy(target), _)
        | (_, RunValueClassification::Legacy(target)) => RunOwnershipState::StaleCodexBar { target },
        (RunValueClassification::Canonical, _) | (_, RunValueClassification::Canonical) => {
            RunOwnershipState::OwnedCanonical
        }
        (RunValueClassification::Missing, RunValueClassification::Missing) => {
            RunOwnershipState::Disabled
        }
    }
}

/// Explicitly change startup ownership after a process has passed
/// verify_current_install. Portable, development, and unverified processes
/// cannot construct a VerifiedInstallRegistration, so cannot reach this
/// mutation boundary.
pub fn set_start_at_login(
    registration: &VerifiedInstallRegistration,
    enabled: bool,
    registry: &mut impl RunValueStore,
) -> Result<RunOwnershipState, InstallOwnershipError> {
    let canonical = classify_run_value(
        registry
            .read_value(CANONICAL_RUN_VALUE)
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueUnreadable)),
        registration,
    );
    let legacy = classify_run_value(
        registry
            .read_value(LEGACY_RUN_VALUE)
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueUnreadable)),
        registration,
    );

    if matches!(&canonical, RunValueClassification::Unreadable)
        || matches!(&legacy, RunValueClassification::Unreadable)
    {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::RunValueUnreadable,
        ));
    }
    if matches!(&canonical, RunValueClassification::Foreign(_))
        || matches!(&legacy, RunValueClassification::Foreign(_))
    {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::ForeignRunOwnership,
        ));
    }

    if enabled {
        registry
            .write_value(CANONICAL_RUN_VALUE, &registration.startup_command())
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))?;
        if matches!(
            &legacy,
            RunValueClassification::Canonical | RunValueClassification::Legacy(_)
        ) {
            registry
                .delete_value(LEGACY_RUN_VALUE)
                .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))?;
        }
        return Ok(RunOwnershipState::OwnedCanonical);
    }

    if matches!(
        &canonical,
        RunValueClassification::Canonical | RunValueClassification::Legacy(_)
    ) {
        registry
            .delete_value(CANONICAL_RUN_VALUE)
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))?;
    }
    if matches!(
        &legacy,
        RunValueClassification::Canonical | RunValueClassification::Legacy(_)
    ) {
        registry
            .delete_value(LEGACY_RUN_VALUE)
            .map_err(|_| InstallOwnershipError::new(InstallOwnershipErrorKind::RunValueWriteFailed))?;
    }
    Ok(RunOwnershipState::Disabled)
}

fn classify_run_value(
    value: Result<Option<String>, InstallOwnershipError>,
    registration: &VerifiedInstallRegistration,
) -> RunValueClassification {
    let Ok(value) = value else {
        return RunValueClassification::Unreadable;
    };
    let Some(value) = value else {
        return RunValueClassification::Missing;
    };
    let Some(target) = command_target(&value) else {
        return RunValueClassification::Foreign(command_hash(&value));
    };
    if target_matches_launcher(&target, registration.launcher_path()) {
        return RunValueClassification::Canonical;
    }
    if is_known_legacy_target(&target, registration) {
        return RunValueClassification::Legacy(target);
    }
    RunValueClassification::Foreign(command_hash(&value))
}

fn command_target(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if let Some(quoted) = command.strip_prefix('"') {
        let closing_quote = quoted.find('"')?;
        let target = &quoted[..closing_quote];
        return (!target.is_empty()).then(|| PathBuf::from(target));
    }
    command
        .split_ascii_whitespace()
        .next()
        .filter(|target| !target.is_empty())
        .map(PathBuf::from)
}

fn target_matches_launcher(target: &Path, launcher: &Path) -> bool {
    let target = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    paths_equal(&target, launcher)
}

fn is_known_legacy_target(target: &Path, registration: &VerifiedInstallRegistration) -> bool {
    let Some(canonical_root) = registration.launcher_path.parent() else {
        return false;
    };
    let Some(legacy_root) = canonical_root.parent() else {
        return false;
    };
    let target = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let target = normalized_path(&target);
    let legacy_root = normalized_path(legacy_root);
    // Only the historical flat installation is known to belong to the old
    // installer. Backups, portable copies, and future version directories must
    // not acquire cleanup eligibility merely by sharing its parent directory.
    ["codexbar.exe", "codexbar-desktop.exe", "codexbar-desktop-tauri.exe"]
        .iter()
        .any(|name| target == format!("{legacy_root}\\{name}"))
}

fn command_hash(command: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(command.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Verify that the process asking to own an installed resource is the exact
/// canonical launcher described by a matching stable registration. Both the
/// registered and current paths are canonicalized before comparison so a link
/// or case-only alias cannot obtain ownership.
pub fn verify_current_install(
    registration: &InstallRegistration,
    local_app_data: &Path,
    current_executable: &Path,
    build: &BuildIdentity,
) -> Result<VerifiedInstallRegistration, InstallOwnershipError> {
    if build.build_channel != BuildChannel::Stable {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::BuildIsNotStable,
        ));
    }
    if registration.app_id != CANONICAL_APP_ID
        || registration.installed_version != build.semantic_version
        || !registration
            .expected_commit
            .eq_ignore_ascii_case(&build.git_commit)
        || !registration
            .expected_manifest_sha256
            .eq_ignore_ascii_case(&build.package_manifest_sha256)
    {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::RegistrationIdentityMismatch,
        ));
    }

    let expected_root = canonicalize(
        &local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2"),
        InstallOwnershipErrorKind::CanonicalInstallUnavailable,
    )?;
    let registered_root = canonicalize(
        &registration.install_root,
        InstallOwnershipErrorKind::RegistrationPathInvalid,
    )?;
    if !paths_equal(&expected_root, &registered_root) {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::RegistrationPathInvalid,
        ));
    }

    let expected_launcher = canonicalize(
        &expected_root.join(CANONICAL_LAUNCHER_FILE_NAME),
        InstallOwnershipErrorKind::CanonicalLauncherUnavailable,
    )?;
    let registered_launcher = canonicalize(
        &registration.launcher_path,
        InstallOwnershipErrorKind::RegistrationPathInvalid,
    )?;
    if !paths_equal(&expected_launcher, &registered_launcher) {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::RegistrationPathInvalid,
        ));
    }

    let current_launcher = canonicalize(
        current_executable,
        InstallOwnershipErrorKind::CurrentExecutableUnavailable,
    )?;
    if !paths_equal(&expected_launcher, &current_launcher) {
        return Err(InstallOwnershipError::new(
            InstallOwnershipErrorKind::CurrentExecutableIsNotCanonical,
        ));
    }

    Ok(VerifiedInstallRegistration {
        registration: registration.clone(),
        launcher_path: expected_launcher,
    })
}

fn canonicalize(
    path: &Path,
    error_kind: InstallOwnershipErrorKind,
) -> Result<PathBuf, InstallOwnershipError> {
    fs::canonicalize(path).map_err(|_| InstallOwnershipError::new(error_kind))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalized_path(left) == normalized_path(right)
}

/// Classify a candidate executable path before it is allowed to take ownership
/// of the installed application. This is intentionally a lexical classifier:
/// callers that are about to mutate a shared resource must canonicalize and
/// verify the file separately, preserving the original path for diagnostics.
pub fn classify_distribution_for_path(
    executable: &Path,
    build: &BuildIdentity,
    local_app_data: &Path,
) -> DistributionKind {
    if !build.product_name.eq_ignore_ascii_case("CodexBar") {
        return DistributionKind::Unknown;
    }

    let executable = normalized_path(executable);
    if is_development_path(&executable) {
        return DistributionKind::Development;
    }

    let canonical_root = normalized_path(
        &local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2"),
    );
    let canonical_launcher = format!("{canonical_root}\\{CANONICAL_LAUNCHER_FILE_NAME}");
    if executable == canonical_launcher {
        return if build.build_channel == BuildChannel::Stable {
            DistributionKind::InstalledStable
        } else {
            DistributionKind::Unknown
        };
    }

    let legacy_root = normalized_path(&local_app_data.join("Programs").join("CodexBar"));
    if is_within(&executable, &legacy_root)
        && build.build_channel == BuildChannel::Stable
        && matches!(
            executable.rsplit('\\').next(),
            Some("codexbar.exe" | "codexbar-desktop.exe" | "codexbar-desktop-tauri.exe")
        )
    {
        return DistributionKind::LegacyInstalled;
    }

    if build.build_channel == BuildChannel::Stable {
        return DistributionKind::Portable;
    }

    DistributionKind::Unknown
}

fn is_development_path(path: &str) -> bool {
    let has_profile = path.contains("\\debug\\") || path.contains("\\release\\");
    let has_target_directory = path.split('\\').any(|component| {
        component == "target" || component.starts_with("target-") || component.ends_with("-target")
    });
    has_profile && has_target_directory
}

fn is_within(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&(root.to_owned() + "\\"))
}

fn normalized_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if let Some(rest) = normalized.strip_prefix("\\\\?\\unc\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = normalized.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_identity::{BuildChannel, BuildIdentity, DistributionKind};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn stable_build() -> BuildIdentity {
        let mut build = BuildIdentity::compiled();
        build.build_channel = BuildChannel::Stable;
        build
    }

    #[test]
    fn only_the_v2_launcher_is_classified_as_the_stable_install() {
        let local_app_data = Path::new(r"C:\Users\Example\AppData\Local");
        let build = stable_build();

        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"c:\users\example\appdata\local\PROGRAMS\CODEXBAR\v2\CODEXBAR.EXE"),
                &build,
                local_app_data,
            ),
            DistributionKind::InstalledStable,
        );
        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar.exe"),
                &build,
                local_app_data,
            ),
            DistributionKind::LegacyInstalled,
        );
        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar-desktop.exe"),
                &build,
                local_app_data,
            ),
            DistributionKind::LegacyInstalled,
        );
        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"D:\Downloads\CodexBar-0.46.0-portable.exe"),
                &build,
                local_app_data,
            ),
            DistributionKind::Portable,
        );
        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"D:\src\CodexBar\target\debug\codexbar.exe"),
                &build,
                local_app_data,
            ),
            DistributionKind::Development,
        );

        let mut development_build = build.clone();
        development_build.build_channel = BuildChannel::Dev;
        assert_eq!(
            classify_distribution_for_path(
                Path::new(r"D:\scratch\codexbar.exe"),
                &development_build,
                local_app_data,
            ),
            DistributionKind::Unknown,
        );
    }

    #[test]
    fn only_a_matching_canonical_launcher_can_verify_install_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let local_app_data = directory.path().join("Local");
        let install_root = local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2");
        std::fs::create_dir_all(&install_root).unwrap();
        let launcher = install_root.join(CANONICAL_LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, b"installed-launcher").unwrap();

        let build = stable_build();
        let registration = InstallRegistration {
            install_root: install_root.clone(),
            launcher_path: launcher.clone(),
            app_id: CANONICAL_APP_ID.to_string(),
            installed_version: build.semantic_version.clone(),
            expected_commit: build.git_commit.clone(),
            expected_manifest_sha256: build.package_manifest_sha256.clone(),
        };

        let verified =
            verify_current_install(&registration, &local_app_data, &launcher, &build).unwrap();
        assert_eq!(verified.launcher_path(), launcher.canonicalize().unwrap());
        assert_eq!(
            verified.startup_command(),
            format!("\"{}\" --startup", launcher.canonicalize().unwrap().display())
        );

        let portable = directory.path().join("portable").join("codexbar.exe");
        std::fs::create_dir_all(portable.parent().unwrap()).unwrap();
        std::fs::write(&portable, b"portable-copy").unwrap();
        assert_eq!(
            verify_current_install(&registration, &local_app_data, &portable, &build)
                .unwrap_err()
                .kind(),
            InstallOwnershipErrorKind::CurrentExecutableIsNotCanonical,
        );
    }

    #[derive(Default)]
    struct FakeRunValueStore {
        values: BTreeMap<String, String>,
    }

    #[test]
    fn installed_cli_can_read_startup_state_without_acquiring_write_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("Local");
        let root = local.join("Programs").join("CodexBar").join("v2");
        fs::create_dir_all(&root).unwrap();
        let launcher = root.join(CANONICAL_LAUNCHER_FILE_NAME);
        let cli = root.join("codexbar-cli.exe");
        fs::write(&launcher, b"launcher").unwrap();
        fs::write(&cli, b"cli").unwrap();
        let build = stable_build();
        let registration = InstallRegistration {
            install_root: root,
            launcher_path: launcher.clone(),
            app_id: CANONICAL_APP_ID.into(),
            installed_version: build.semantic_version.clone(),
            expected_commit: build.git_commit.clone(),
            expected_manifest_sha256: build.package_manifest_sha256.clone(),
        };
        let mut registry = FakeRunValueStore::default();
        assert_eq!(read_start_at_login_state_for_executable(
            &registration, &local, &cli, &build, &registry,
        ).unwrap(), RunOwnershipState::Disabled);
        let verified = verify_current_install(&registration, &local, &launcher, &build).unwrap();
        registry.values.insert(CANONICAL_RUN_VALUE.into(), verified.startup_command());
        let before = registry.values.clone();
        assert_eq!(read_start_at_login_state_for_executable(
            &registration, &local, &cli, &build, &registry,
        ).unwrap(), RunOwnershipState::OwnedCanonical);
        assert_eq!(registry.values, before);
        assert_eq!(verify_current_install(&registration, &local, &cli, &build)
            .unwrap_err().kind(), InstallOwnershipErrorKind::CurrentExecutableIsNotCanonical);
        let portable = directory.path().join("codexbar-cli.exe");
        fs::write(&portable, b"portable").unwrap();
        assert!(read_start_at_login_state_for_executable(
            &registration, &local, &portable, &build, &registry,
        ).is_err());
        let mut wrong_build = build.clone();
        wrong_build.git_commit = "different-build".into();
        assert!(read_start_at_login_state_for_executable(
            &registration, &local, &cli, &wrong_build, &registry,
        ).is_err());
    }

    impl RunValueStore for FakeRunValueStore {
        fn read_value(&self, name: &str) -> Result<Option<String>, InstallOwnershipError> {
            Ok(self.values.get(name).cloned())
        }

        fn write_value(
            &mut self,
            name: &str,
            value: &str,
        ) -> Result<(), InstallOwnershipError> {
            self.values.insert(name.to_string(), value.to_string());
            Ok(())
        }

        fn delete_value(&mut self, name: &str) -> Result<(), InstallOwnershipError> {
            self.values.remove(name);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeInstallRegistrationStore {
        values: BTreeMap<String, String>,
    }

    impl InstallRegistrationStore for FakeInstallRegistrationStore {
        fn read_registration_value(
            &self,
            name: &str,
        ) -> Result<Option<String>, InstallOwnershipError> {
            Ok(self.values.get(name).cloned())
        }
    }

    #[test]
    fn enabling_autostart_writes_only_the_verified_canonical_command() {
        let directory = tempfile::tempdir().unwrap();
        let local_app_data = directory.path().join("Local");
        let install_root = local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2");
        std::fs::create_dir_all(&install_root).unwrap();
        let launcher = install_root.join(CANONICAL_LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, b"installed-launcher").unwrap();

        let legacy_launcher = local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("codexbar.exe");
        std::fs::write(&legacy_launcher, b"legacy-launcher").unwrap();

        let build = stable_build();
        let registration = InstallRegistration {
            install_root,
            launcher_path: launcher.clone(),
            app_id: CANONICAL_APP_ID.to_string(),
            installed_version: build.semantic_version.clone(),
            expected_commit: build.git_commit.clone(),
            expected_manifest_sha256: build.package_manifest_sha256.clone(),
        };
        let verified =
            verify_current_install(&registration, &local_app_data, &launcher, &build).unwrap();
        let mut run_values = FakeRunValueStore::default();
        run_values.values.insert(
            LEGACY_RUN_VALUE.to_string(),
            format!("\"{}\"", legacy_launcher.display()),
        );
        assert!(
            is_known_legacy_target(&legacy_launcher, &verified),
            "the old flat launcher must be eligible for targeted cleanup",
        );

        let state = set_start_at_login(&verified, true, &mut run_values).unwrap();

        assert_eq!(state, RunOwnershipState::OwnedCanonical);
        assert_eq!(
            run_values.values.get(CANONICAL_RUN_VALUE),
            Some(&verified.startup_command())
        );
        assert!(!run_values.values.contains_key(LEGACY_RUN_VALUE));
    }

    #[test]
    fn a_foreign_run_value_is_never_replaced_or_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let local_app_data = directory.path().join("Local");
        let install_root = local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2");
        std::fs::create_dir_all(&install_root).unwrap();
        let launcher = install_root.join(CANONICAL_LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, b"installed-launcher").unwrap();
        let build = stable_build();
        let registration = InstallRegistration {
            install_root,
            launcher_path: launcher.clone(),
            app_id: CANONICAL_APP_ID.to_string(),
            installed_version: build.semantic_version.clone(),
            expected_commit: build.git_commit.clone(),
            expected_manifest_sha256: build.package_manifest_sha256.clone(),
        };
        let verified =
            verify_current_install(&registration, &local_app_data, &launcher, &build).unwrap();
        let foreign_command = "\"D:\\OtherProduct\\other.exe\" --startup".to_string();
        let mut run_values = FakeRunValueStore::default();
        run_values
            .values
            .insert(CANONICAL_RUN_VALUE.to_string(), foreign_command.clone());

        assert_eq!(
            set_start_at_login(&verified, true, &mut run_values)
                .unwrap_err()
                .kind(),
            InstallOwnershipErrorKind::ForeignRunOwnership,
        );
        assert_eq!(
            run_values.values.get(CANONICAL_RUN_VALUE),
            Some(&foreign_command)
        );
    }

    #[test]
    fn disabling_autostart_removes_the_canonical_launcher_from_the_legacy_value_name() {
        let directory = tempfile::tempdir().unwrap();
        let local_app_data = directory.path().join("Local");
        let install_root = local_app_data
            .join("Programs")
            .join("CodexBar")
            .join("v2");
        std::fs::create_dir_all(&install_root).unwrap();
        let launcher = install_root.join(CANONICAL_LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, b"installed-launcher").unwrap();
        let build = stable_build();
        let registration = InstallRegistration {
            install_root,
            launcher_path: launcher.clone(),
            app_id: CANONICAL_APP_ID.to_string(),
            installed_version: build.semantic_version.clone(),
            expected_commit: build.git_commit.clone(),
            expected_manifest_sha256: build.package_manifest_sha256.clone(),
        };
        let verified =
            verify_current_install(&registration, &local_app_data, &launcher, &build).unwrap();
        let mut run_values = FakeRunValueStore::default();
        run_values
            .values
            .insert(LEGACY_RUN_VALUE.to_string(), verified.startup_command());

        let state = set_start_at_login(&verified, false, &mut run_values).unwrap();

        assert_eq!(state, RunOwnershipState::Disabled);
        assert!(!run_values.values.contains_key(LEGACY_RUN_VALUE));
    }

    #[test]
    fn a_future_install_registration_schema_is_rejected_before_path_use() {
        let mut registration = FakeInstallRegistrationStore::default();
        registration
            .values
            .insert(INSTALL_REGISTRATION_SCHEMA_VALUE.to_string(), "2".to_string());
        registration.values.insert(
            INSTALL_REGISTRATION_ROOT_VALUE.to_string(),
            r"C:\Users\Example\AppData\Local\Programs\CodexBar\v2".to_string(),
        );
        registration.values.insert(
            INSTALL_REGISTRATION_LAUNCHER_VALUE.to_string(),
            r"C:\Users\Example\AppData\Local\Programs\CodexBar\v2\codexbar.exe".to_string(),
        );
        registration.values.insert(
            INSTALL_REGISTRATION_APP_ID_VALUE.to_string(),
            CANONICAL_APP_ID.to_string(),
        );
        registration.values.insert(
            INSTALL_REGISTRATION_VERSION_VALUE.to_string(),
            "0.46.0".to_string(),
        );
        registration.values.insert(
            INSTALL_REGISTRATION_COMMIT_VALUE.to_string(),
            "a".repeat(40),
        );
        registration.values.insert(
            INSTALL_REGISTRATION_MANIFEST_VALUE.to_string(),
            "b".repeat(64),
        );

        assert_eq!(
            load_install_registration(&registration).unwrap_err().kind(),
            InstallOwnershipErrorKind::RegistrationSchemaUnsupported,
        );
    }

    fn synthetic_verified_registration() -> VerifiedInstallRegistration {
        VerifiedInstallRegistration {
            registration: InstallRegistration {
                install_root: PathBuf::from(r"C:\Local\Programs\CodexBar\v2"),
                launcher_path: PathBuf::from(r"C:\Local\Programs\CodexBar\v2\codexbar.exe"),
                app_id: CANONICAL_APP_ID.into(),
                installed_version: "0.46.0".into(),
                expected_commit: "a".repeat(40),
                expected_manifest_sha256: "b".repeat(64),
            },
            launcher_path: PathBuf::from(r"C:\Local\Programs\CodexBar\v2\codexbar.exe"),
        }
    }

    #[test]
    fn installer_registration_matches_the_runtime_contract() {
        let installer = include_str!("../installer/codexbar.iss");
        assert!(installer.contains(&format!("AppId={{{CANONICAL_APP_ID}")));
        assert!(installer.contains(&format!("DefaultDirName={{localappdata}}\\{CANONICAL_INSTALL_DIRECTORY}")));
        for field in [
            INSTALL_REGISTRATION_SCHEMA_VALUE,
            INSTALL_REGISTRATION_ROOT_VALUE,
            INSTALL_REGISTRATION_LAUNCHER_VALUE,
            INSTALL_REGISTRATION_APP_ID_VALUE,
            INSTALL_REGISTRATION_VERSION_VALUE,
            INSTALL_REGISTRATION_COMMIT_VALUE,
            INSTALL_REGISTRATION_MANIFEST_VALUE,
        ] {
            assert!(installer.lines().any(|line| {
                line.contains(&format!("Subkey: \"{INSTALL_REGISTRATION_SUBKEY}\""))
                    && line.contains(&format!("ValueName: \"{field}\""))
                    && line.contains("ValueType: string")
            }), "missing or incompatible install registration field: {field}");
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn an_absent_read_only_run_key_reports_disabled_without_creating_it() {
        let registry = WindowsRunValueStore { key: None };
        assert_eq!(
            read_start_at_login_state(&synthetic_verified_registration(), &registry),
            RunOwnershipState::Disabled,
        );
    }

    #[test]
    fn a_nested_copy_is_not_treated_as_the_legacy_install() {
        let registration = synthetic_verified_registration();
        for target in [
            r"C:\Local\Programs\CodexBar\backup\codexbar.exe",
            r"C:\Local\Programs\CodexBar\v3\codexbar.exe",
        ] {
            let command = format!("\"{target}\" --startup");
            let mut registry = FakeRunValueStore::default();
            registry.values.insert(LEGACY_RUN_VALUE.into(), command.clone());
            for enabled in [true, false] {
                assert_eq!(
                    set_start_at_login(&registration, enabled, &mut registry)
                        .unwrap_err().kind(),
                    InstallOwnershipErrorKind::ForeignRunOwnership,
                );
                assert_eq!(registry.values.get(LEGACY_RUN_VALUE), Some(&command));
                assert!(!registry.values.contains_key(CANONICAL_RUN_VALUE));
            }
        }
    }

    #[test]
    fn an_owned_v2_entry_does_not_hide_a_conflicting_legacy_entry() {
        let registration = synthetic_verified_registration();
        let mut registry = FakeRunValueStore::default();
        registry.values.insert(CANONICAL_RUN_VALUE.into(), registration.startup_command());
        registry.values.insert(LEGACY_RUN_VALUE.into(), r#""D:\Other\other.exe""#.into());

        assert!(matches!(
            read_start_at_login_state(&registration, &registry),
            RunOwnershipState::Foreign { .. }
        ));

        registry.values.insert(
            LEGACY_RUN_VALUE.into(),
            r#""C:\Local\Programs\CodexBar\codexbar.exe""#.into(),
        );
        assert!(matches!(
            read_start_at_login_state(&registration, &registry),
            RunOwnershipState::StaleCodexBar { .. }
        ));
    }
}
