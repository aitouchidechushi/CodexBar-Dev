use crate::runtime_identity::RuntimeIdentity;
use codexbar::build_identity::{BuildChannel, BuildIdentity, DistributionKind};
use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "windows")]
use std::collections::BTreeSet;
#[cfg(target_os = "windows")]
use std::fs::{self, File};
#[cfg(target_os = "windows")]
use std::io::Read;
#[cfg(target_os = "windows")]
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupContext {
    pub current_pid: u32,
    pub current_user_sid_hash: String,
    pub build: BuildIdentity,
    pub distribution: DistributionKind,
    pub executable_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEvidence {
    /// The executable was independently identified as CodexBar.
    VerifiedCodexBar,
    /// A same-name process was inspected and proved to belong to another app.
    VerifiedOtherProduct,
    /// A candidate name was seen, but its path, hash, or product identity could
    /// not be read safely.
    UnverifiableCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub user_sid_hash: String,
    pub executable_path: PathBuf,
    pub executable_sha256: String,
    pub build: Option<BuildIdentity>,
    pub distribution: DistributionKind,
    pub evidence: ProcessEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeProcessConflictAction {
    CloseManually,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeProcessConflict {
    pub pid: u32,
    pub executable_path: PathBuf,
    pub executable_sha256: String,
    pub action: SafeProcessConflictAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupDecision {
    Continue,
    HandOffToSameOrNewer { pid: u32 },
    RequestVerifiedOldExit { pid: u32 },
    BlockUnknownConflict { conflicts: Vec<SafeProcessConflict> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPreflightErrorKind {
    CurrentUserIdentityUnavailable,
    ProcessSnapshotUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupPreflightError {
    kind: StartupPreflightErrorKind,
}

impl StartupPreflightError {
    pub fn kind(&self) -> StartupPreflightErrorKind {
        self.kind
    }

    fn new(kind: StartupPreflightErrorKind) -> Self {
        Self { kind }
    }
}

impl fmt::Display for StartupPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            StartupPreflightErrorKind::CurrentUserIdentityUnavailable => {
                "the current Windows user identity could not be inspected"
            }
            StartupPreflightErrorKind::ProcessSnapshotUnavailable => {
                "the running-process snapshot could not be inspected"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for StartupPreflightError {}

/// Inspect likely CodexBar processes before any shared storage is opened. The
/// Windows implementation treats incomplete evidence as a conflict, rather
/// than guessing which process can be terminated. Non-Windows builds do not
/// share the Windows installation or Run-key ownership model.
pub fn preflight_current_process(
    runtime_identity: &RuntimeIdentity,
) -> Result<StartupDecision, StartupPreflightError> {
    #[cfg(target_os = "windows")]
    {
        let current_user_sid_hash = current_user_sid_hash().ok_or_else(|| {
            StartupPreflightError::new(StartupPreflightErrorKind::CurrentUserIdentityUnavailable)
        })?;
        let candidates = inspect_candidate_processes(&current_user_sid_hash, runtime_identity)?;
        return Ok(decide_for_runtime_identity(
            runtime_identity,
            current_user_sid_hash,
            &candidates,
        ));
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = runtime_identity;
        Ok(StartupDecision::Continue)
    }
}

fn decide_for_runtime_identity(
    runtime_identity: &RuntimeIdentity,
    current_user_sid_hash: String,
    candidates: &[ProcessIdentity],
) -> StartupDecision {
    let current = StartupContext {
        current_pid: runtime_identity.pid,
        current_user_sid_hash,
        build: runtime_identity.build.clone(),
        distribution: runtime_identity.distribution,
        executable_sha256: runtime_identity.executable_sha256.clone(),
    };
    decide_startup_ownership(&current, candidates)
}

/// Abort before opening any shared storage when Windows process inspection
/// itself cannot establish a safe answer.
pub fn report_preflight_failure(error: StartupPreflightError) -> ! {
    tracing::error!(
        error_kind = ?error.kind(),
        "startup ownership preflight could not inspect the process boundary"
    );
    crate::runtime_identity::report_fatal_startup_message(
        "CodexBar could not safely inspect other running programs.\n\nTo protect saved settings and API keys, it did not start. Close any existing CodexBar process and try again. If the problem persists, reinstall from the verified installer.",
    )
}

/// Abort before storage initialization when an old or unverifiable CodexBar
/// candidate could otherwise share the same files or single-instance domain.
pub fn report_startup_conflict(decision: &StartupDecision) -> ! {
    let message = match decision {
        StartupDecision::BlockUnknownConflict { conflicts } => startup_conflict_message(conflicts),
        StartupDecision::RequestVerifiedOldExit { pid } => format!(
            "An earlier verified CodexBar process (PID {pid}) is still running.\n\nTo protect saved settings and API keys, this copy did not start. Close that earlier CodexBar process, then start this copy again. No process was stopped automatically."
        ),
        StartupDecision::Continue | StartupDecision::HandOffToSameOrNewer { .. } => {
            "CodexBar startup ownership was not available. The application did not start."
                .to_string()
        }
    };
    tracing::warn!(
        decision = ?decision,
        "startup ownership prevented shared storage initialization"
    );
    crate::runtime_identity::report_fatal_startup_message(&message)
}

fn startup_conflict_message(conflicts: &[SafeProcessConflict]) -> String {
    let details = conflicts
        .iter()
        .take(5)
        .map(|conflict| {
            let hash = if is_sha256_hex(&conflict.executable_sha256) {
                conflict.executable_sha256.as_str()
            } else {
                "unavailable"
            };
            format!(
                "PID: {}\nPath: {}\nFile SHA-256: {}\nRequired action: close it manually",
                conflict.pid,
                conflict.executable_path.display(),
                hash,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let additional = conflicts.len().saturating_sub(5);
    let additional = if additional == 0 {
        String::new()
    } else {
        format!("\n\n{additional} additional candidate process(es) were not listed.")
    };

    format!(
        "CodexBar found an earlier process whose identity could not be proven safe for automatic replacement.\n\nTo protect saved settings and API keys, this copy did not start. No process was stopped automatically. Close the listed earlier process(es), then start CodexBar again.\n\n{details}{additional}"
    )
}

/// Decide startup ownership from a fully captured candidate list. This pure
/// function never touches storage or processes; the Windows coordinator is
/// responsible for obtaining evidence and carrying out only the selected
/// action.
pub fn decide_startup_ownership(
    current: &StartupContext,
    candidates: &[ProcessIdentity],
) -> StartupDecision {
    let mut conflicts = Vec::new();
    let mut same_or_newer = Vec::new();
    let mut verified_old = Vec::new();

    for candidate in candidates {
        if candidate.pid == current.current_pid
            || candidate.user_sid_hash != current.current_user_sid_hash
        {
            continue;
        }
        match candidate.evidence {
            ProcessEvidence::VerifiedOtherProduct => continue,
            ProcessEvidence::UnverifiableCandidate => {
                conflicts.push(safe_conflict(candidate));
                continue;
            }
            ProcessEvidence::VerifiedCodexBar => {}
        }

        // A second copy of the exact registered v2 launcher may not expose an
        // inspectable BuildIdentity record. Its canonical distribution and
        // matching SHA-256 are sufficient for a safe single-instance handoff;
        // do not initialize shared storage in that process.
        if is_verified_same_canonical_binary(current, candidate) {
            same_or_newer.push(candidate.pid);
            continue;
        }

        let Some(build) = candidate.build.as_ref() else {
            conflicts.push(safe_conflict(candidate));
            continue;
        };
        if !build.product_name.eq_ignore_ascii_case("CodexBar")
            || !is_sha256_hex(&candidate.executable_sha256)
        {
            conflicts.push(safe_conflict(candidate));
            continue;
        }

        // The Windows inspector supplies our complete build identity only for
        // the same canonical path and bytes owning the single-instance window.
        // Distribution/channel is not authority to mutate anything here: this
        // decision merely lets the plugin forward the launch before storage opens.
        if build == &current.build
            && is_sha256_hex(&current.executable_sha256)
            && candidate
                .executable_sha256
                .eq_ignore_ascii_case(&current.executable_sha256)
        {
            same_or_newer.push(candidate.pid);
            continue;
        }

        if current.distribution != DistributionKind::InstalledStable
            || current.build.build_channel != BuildChannel::Stable
            || !is_sha256_hex(&current.executable_sha256)
        {
            conflicts.push(safe_conflict(candidate));
            continue;
        }

        match candidate.distribution {
            DistributionKind::InstalledStable => {
                let Some(version_ordering) = compare_stable_versions(
                    &build.semantic_version,
                    &current.build.semantic_version,
                ) else {
                    conflicts.push(safe_conflict(candidate));
                    continue;
                };
                match version_ordering {
                    Ordering::Greater => same_or_newer.push(candidate.pid),
                    Ordering::Equal
                        if build
                            .git_commit
                            .eq_ignore_ascii_case(&current.build.git_commit)
                            && candidate
                                .executable_sha256
                                .eq_ignore_ascii_case(&current.executable_sha256) =>
                    {
                        same_or_newer.push(candidate.pid);
                    }
                    Ordering::Equal => conflicts.push(safe_conflict(candidate)),
                    Ordering::Less => verified_old.push(candidate.pid),
                }
            }
            DistributionKind::LegacyInstalled | DistributionKind::Portable
                if build.build_channel == BuildChannel::Stable =>
            {
                verified_old.push(candidate.pid);
            }
            DistributionKind::Development | DistributionKind::Unknown => {
                conflicts.push(safe_conflict(candidate));
            }
            DistributionKind::LegacyInstalled | DistributionKind::Portable => {
                conflicts.push(safe_conflict(candidate));
            }
        }
    }

    if !conflicts.is_empty() {
        conflicts.sort_by_key(|conflict| conflict.pid);
        return StartupDecision::BlockUnknownConflict { conflicts };
    }
    if let Some(pid) = same_or_newer.into_iter().min() {
        return StartupDecision::HandOffToSameOrNewer { pid };
    }
    if let Some(pid) = verified_old.into_iter().min() {
        return StartupDecision::RequestVerifiedOldExit { pid };
    }
    StartupDecision::Continue
}

fn safe_conflict(candidate: &ProcessIdentity) -> SafeProcessConflict {
    SafeProcessConflict {
        pid: candidate.pid,
        executable_path: candidate.executable_path.clone(),
        executable_sha256: candidate.executable_sha256.clone(),
        action: SafeProcessConflictAction::CloseManually,
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_verified_same_canonical_binary(
    current: &StartupContext,
    candidate: &ProcessIdentity,
) -> bool {
    current.distribution == DistributionKind::InstalledStable
        && current.build.build_channel == BuildChannel::Stable
        && candidate.distribution == DistributionKind::InstalledStable
        && is_sha256_hex(&current.executable_sha256)
        && is_sha256_hex(&candidate.executable_sha256)
        && candidate
            .executable_sha256
            .eq_ignore_ascii_case(&current.executable_sha256)
}

fn compare_stable_versions(left: &str, right: &str) -> Option<Ordering> {
    fn parse(value: &str) -> Option<[u32; 3]> {
        let mut segments = value.split('.');
        let parsed = [
            segments.next()?.parse().ok()?,
            segments.next()?.parse().ok()?,
            segments.next()?.parse().ok()?,
        ];
        if segments.next().is_some() {
            return None;
        }
        Some(parsed)
    }

    Some(parse(left)?.cmp(&parse(right)?))
}

#[cfg(target_os = "windows")]
const CANDIDATE_EXECUTABLE_NAMES: &[&str] = &[
    "codexbar.exe",
    "codexbar-desktop.exe",
    "codexbar-desktop-tauri.exe",
];

#[cfg(target_os = "windows")]
const SINGLE_INSTANCE_IDENTIFIER: &str = "com.codexbar.desktop";

#[cfg(target_os = "windows")]
fn current_user_sid_hash() -> Option<String> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // GetCurrentProcess returns a pseudo-handle that must not be closed.
    sid_hash_for_process(unsafe { GetCurrentProcess() })
}

#[cfg(target_os = "windows")]
fn inspect_candidate_processes(
    current_user_sid_hash: &str,
    runtime_identity: &RuntimeIdentity,
) -> Result<Vec<ProcessIdentity>, StartupPreflightError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(StartupPreflightError::new(
            StartupPreflightErrorKind::ProcessSnapshotUnavailable,
        ));
    }

    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut candidate_pids = BTreeSet::new();
    if let Some(pid) = single_instance_target_process_id() {
        candidate_pids.insert(pid);
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;

    while has_entry {
        let pid = entry.th32ProcessID;
        let executable_name = process_entry_name(&entry);
        if is_candidate_executable_name(&executable_name) {
            candidate_pids.insert(pid);
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }

    unsafe {
        CloseHandle(snapshot);
    }

    let candidates = candidate_pids
        .into_iter()
        .filter(|pid| *pid != runtime_identity.pid)
        .filter_map(|pid| {
            inspect_candidate_process(
                pid,
                current_user_sid_hash,
                runtime_identity,
                &local_app_data,
            )
        })
        .collect();
    Ok(candidates)
}

#[cfg(target_os = "windows")]
fn process_entry_name(
    entry: &windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W,
) -> String {
    let length = entry
        .szExeFile
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..length])
}

#[cfg(target_os = "windows")]
fn is_candidate_executable_name(name: &str) -> bool {
    CANDIDATE_EXECUTABLE_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

#[cfg(target_os = "windows")]
fn single_instance_target_process_id() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};

    let class_name = wide_null_terminated(&format!("{SINGLE_INSTANCE_IDENTIFIER}-sic"));
    let window_name = wide_null_terminated(&format!("{SINGLE_INSTANCE_IDENTIFIER}-siw"));
    let window = unsafe { FindWindowW(class_name.as_ptr(), window_name.as_ptr()) };
    if window.is_null() {
        return None;
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, &mut pid);
    }
    (pid != 0).then_some(pid)
}

#[cfg(target_os = "windows")]
fn wide_null_terminated(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn inspect_candidate_process(
    pid: u32,
    current_user_sid_hash: &str,
    runtime_identity: &RuntimeIdentity,
    local_app_data: &Path,
) -> Option<ProcessIdentity> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Some(unverifiable_candidate(
            pid,
            current_user_sid_hash,
            PathBuf::from("<process unavailable>"),
        ));
    }
    let result = match sid_hash_for_process(process) {
        Some(candidate_sid_hash) if candidate_sid_hash == current_user_sid_hash => {
            Some(inspect_open_candidate_process(
                process,
                pid,
                current_user_sid_hash,
                runtime_identity,
                local_app_data,
            ))
        }
        Some(_) => None,
        None => Some(unverifiable_candidate(
            pid,
            current_user_sid_hash,
            PathBuf::from("<process owner unavailable>"),
        )),
    };
    unsafe {
        CloseHandle(process);
    }
    result
}

#[cfg(target_os = "windows")]
fn inspect_open_candidate_process(
    process: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
    current_user_sid_hash: &str,
    runtime_identity: &RuntimeIdentity,
    local_app_data: &Path,
) -> ProcessIdentity {
    let Some(reported_path) = query_full_process_image_name(process) else {
        return unverifiable_candidate(
            pid,
            current_user_sid_hash,
            PathBuf::from("<executable path unavailable>"),
        );
    };
    let Ok(executable_path) = fs::canonicalize(&reported_path) else {
        return unverifiable_candidate(pid, current_user_sid_hash, reported_path);
    };
    let Ok(executable_sha256) = hash_file(&executable_path) else {
        return unverifiable_candidate(pid, current_user_sid_hash, executable_path);
    };

    candidate_from_inspected_image(
        pid,
        current_user_sid_hash,
        runtime_identity,
        executable_path,
        executable_sha256,
        local_app_data,
        single_instance_target_process_id(),
    )
}

#[cfg(target_os = "windows")]
fn candidate_from_inspected_image(
    pid: u32,
    current_user_sid_hash: &str,
    runtime_identity: &RuntimeIdentity,
    executable_path: PathBuf,
    executable_sha256: String,
    local_app_data: &Path,
    single_instance_pid: Option<u32>,
) -> ProcessIdentity {
    let distribution = codexbar::install_ownership::classify_distribution_for_path(
        &executable_path,
        &runtime_identity.build,
        local_app_data,
    );
    let same_binary = paths_equal_for_startup(&executable_path, &runtime_identity.executable_path)
        && is_sha256_hex(&runtime_identity.executable_sha256)
        && is_sha256_hex(&executable_sha256)
        && executable_sha256.eq_ignore_ascii_case(&runtime_identity.executable_sha256);
    let same_instance = same_binary && single_instance_pid == Some(pid);
    let same_canonical_install = same_binary
        && runtime_identity.distribution == DistributionKind::InstalledStable
        && distribution == DistributionKind::InstalledStable;
    let evidence = if same_instance || same_canonical_install {
        ProcessEvidence::VerifiedCodexBar
    } else {
        ProcessEvidence::UnverifiableCandidate
    };

    ProcessIdentity {
        pid,
        user_sid_hash: current_user_sid_hash.to_string(),
        executable_path,
        executable_sha256,
        build: same_instance.then(|| runtime_identity.build.clone()),
        distribution,
        evidence,
    }
}

#[cfg(target_os = "windows")]
fn unverifiable_candidate(
    pid: u32,
    current_user_sid_hash: &str,
    executable_path: PathBuf,
) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        user_sid_hash: current_user_sid_hash.to_string(),
        executable_path,
        executable_sha256: String::new(),
        build: None,
        distribution: DistributionKind::Unknown,
        evidence: ProcessEvidence::UnverifiableCandidate,
    }
}

#[cfg(target_os = "windows")]
fn query_full_process_image_name(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

#[cfg(target_os = "windows")]
fn sid_hash_for_process(process: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle, Security::TOKEN_QUERY, System::Threading::OpenProcessToken,
    };

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let result = sid_hash_for_token(token);
    unsafe {
        CloseHandle(token);
    }
    result
}

#[cfg(target_os = "windows")]
fn sid_hash_for_token(token: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Security::{GetLengthSid, GetTokenInformation, TOKEN_USER, TokenUser};

    let mut required_length = 0;
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut required_length,
        );
    }
    if required_length < std::mem::size_of::<TOKEN_USER>() as u32 {
        return None;
    }

    let mut buffer = vec![0_u8; required_length as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required_length,
            &mut required_length,
        )
    } == 0
    {
        return None;
    }

    let token_user = buffer.as_ptr().cast::<TOKEN_USER>();
    let sid = unsafe { (*token_user).User.Sid };
    if sid.is_null() {
        return None;
    }
    let sid_length = unsafe { GetLengthSid(sid) } as usize;
    if sid_length == 0 || sid_length > buffer.len() {
        return None;
    }
    let sid_bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), sid_length) };
    let mut hasher = Sha256::new();
    hasher.update(sid_bytes);
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "windows")]
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "windows")]
fn paths_equal_for_startup(left: &Path, right: &Path) -> bool {
    fn normalize(path: &Path) -> String {
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

    normalize(left) == normalize(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar::build_identity::{BuildChannel, BuildIdentity, DistributionKind};
    use std::path::PathBuf;

    fn stable_build(version: &str, commit_digit: char) -> BuildIdentity {
        let mut build = BuildIdentity::compiled();
        build.build_channel = BuildChannel::Stable;
        build.semantic_version = version.to_string();
        build.git_commit = commit_digit.to_string().repeat(40);
        build.short_commit = commit_digit.to_string().repeat(12);
        build
    }

    #[test]
    fn a_verified_old_install_is_selected_for_controlled_exit() {
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".to_string(),
            build: stable_build("0.46.0", 'b'),
            distribution: DistributionKind::InstalledStable,
            executable_sha256: "b".repeat(64),
        };
        let old_process = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".to_string(),
            executable_path: PathBuf::from(
                r"C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar.exe",
            ),
            executable_sha256: "a".repeat(64),
            build: Some(stable_build("0.45.2", 'a')),
            distribution: DistributionKind::LegacyInstalled,
            evidence: ProcessEvidence::VerifiedCodexBar,
        };

        assert_eq!(
            decide_startup_ownership(&current, &[old_process]),
            StartupDecision::RequestVerifiedOldExit { pid: 101 },
        );
    }

    #[test]
    fn an_identical_canonical_binary_can_handoff_without_embedded_metadata() {
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".to_string(),
            build: stable_build("0.46.0", 'b'),
            distribution: DistributionKind::InstalledStable,
            executable_sha256: "b".repeat(64),
        };
        // Existing legacy binaries cannot expose the new embedded identity
        // record. A candidate independently proven to be the same canonical
        // v2 bytes is nevertheless safe to hand off to; it must not be
        // mistaken for an unknown process merely because it lacks that record.
        let current_instance = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".to_string(),
            executable_path: PathBuf::from(
                r"C:\Users\Example\AppData\Local\Programs\CodexBar\v2\codexbar.exe",
            ),
            executable_sha256: "b".repeat(64),
            build: None,
            distribution: DistributionKind::InstalledStable,
            evidence: ProcessEvidence::VerifiedCodexBar,
        };

        assert_eq!(
            decide_startup_ownership(&current, &[current_instance]),
            StartupDecision::HandOffToSameOrNewer { pid: 101 },
        );
    }

    #[test]
    fn an_unverified_same_hash_never_authorizes_a_handoff() {
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".to_string(),
            build: stable_build("0.46.0", 'b'),
            distribution: DistributionKind::InstalledStable,
            executable_sha256: "b".repeat(64),
        };
        let candidate = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".to_string(),
            executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
            executable_sha256: "b".repeat(64),
            build: None,
            distribution: DistributionKind::InstalledStable,
            evidence: ProcessEvidence::UnverifiableCandidate,
        };

        assert_eq!(
            decide_startup_ownership(&current, &[candidate]),
            StartupDecision::BlockUnknownConflict {
                conflicts: vec![SafeProcessConflict {
                    pid: 101,
                    executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
                    executable_sha256: "b".repeat(64),
                    action: SafeProcessConflictAction::CloseManually,
                }],
            },
        );
    }

    #[test]
    fn an_identical_verified_portable_dev_instance_hands_off() {
        let mut build = stable_build("0.47.0", 'b');
        build.build_channel = BuildChannel::Dev;
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".into(),
            build: build.clone(),
            distribution: DistributionKind::Unknown,
            executable_sha256: "b".repeat(64),
        };
        let candidate = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".into(),
            executable_path: PathBuf::from(r"C:\Tools\CodexBar-0.47.0-portable.exe"),
            executable_sha256: "b".repeat(64),
            build: Some(build),
            distribution: DistributionKind::Unknown,
            evidence: ProcessEvidence::VerifiedCodexBar,
        };
        assert_eq!(
            decide_startup_ownership(&current, &[candidate]),
            StartupDecision::HandOffToSameOrNewer { pid: 101 }
        );
    }

    #[cfg(target_os = "windows")]
    fn portable_runtime() -> RuntimeIdentity {
        let mut build = stable_build("0.47.0", 'b');
        build.build_channel = BuildChannel::Dev;
        RuntimeIdentity {
            build,
            executable_path: PathBuf::from(r"C:\Tools\CodexBar-0.47.0-portable.exe"),
            executable_sha256: "b".repeat(64),
            pid: 100,
            process_started_at: "2026-09-17T00:00:00Z".into(),
            distribution: DistributionKind::Unknown,
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn inspected_portable_handoff_requires_matching_path_hash_and_receiver() {
        let runtime = portable_runtime();
        // Paths are already canonicalized at the OS boundary. The extended
        // Windows spelling must compare equal to RuntimeIdentity's display path.
        let cases = [
            (
                r"C:\Tools\CodexBar-0.47.0-portable.exe",
                "b".repeat(64),
                Some(101),
                true,
            ),
            (
                r"\\?\C:\TOOLS\CodexBar-0.47.0-portable.exe",
                "B".repeat(64),
                Some(101),
                true,
            ),
            (
                r"C:\Other\CodexBar-0.47.0-portable.exe",
                "b".repeat(64),
                Some(101),
                false,
            ),
            (
                r"C:\Tools\CodexBar-0.47.0-portable.exe",
                "a".repeat(64),
                Some(101),
                false,
            ),
            (
                r"C:\Tools\CodexBar-0.47.0-portable.exe",
                String::new(),
                Some(101),
                false,
            ),
            (
                r"C:\Tools\CodexBar-0.47.0-portable.exe",
                "b".repeat(64),
                None,
                false,
            ),
            (
                r"C:\Tools\CodexBar-0.47.0-portable.exe",
                "b".repeat(64),
                Some(102),
                false,
            ),
        ];
        for (path, hash, receiver, allowed) in cases {
            let candidate = candidate_from_inspected_image(
                101,
                "current-user",
                &runtime,
                PathBuf::from(path),
                hash,
                Path::new(r"C:\Users\Example\AppData\Local"),
                receiver,
            );
            let decision =
                decide_for_runtime_identity(&runtime, "current-user".into(), &[candidate]);
            if allowed {
                assert_eq!(decision, StartupDecision::HandOffToSameOrNewer { pid: 101 });
            } else {
                assert!(
                    matches!(decision, StartupDecision::BlockUnknownConflict { .. }),
                    "{path}, {receiver:?}: {decision:?}"
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn matching_portable_hash_cannot_bypass_build_identity_or_other_conflicts() {
        let runtime = portable_runtime();
        let matching = candidate_from_inspected_image(
            101,
            "current-user",
            &runtime,
            runtime.executable_path.clone(),
            "b".repeat(64),
            Path::new(r"C:\Users\Example\AppData\Local"),
            Some(101),
        );
        for mutation in 0..4 {
            let mut candidate = matching.clone();
            match mutation {
                0 => candidate.build.as_mut().unwrap().git_commit = "a".repeat(40),
                1 => candidate.build = None,
                2 => candidate.executable_sha256 = "a".repeat(64),
                _ => candidate.evidence = ProcessEvidence::UnverifiableCandidate,
            }
            assert!(matches!(
                decide_for_runtime_identity(&runtime, "current-user".into(), &[candidate]),
                StartupDecision::BlockUnknownConflict { .. }
            ));
        }
        let foreign =
            unverifiable_candidate(102, "current-user", PathBuf::from(r"C:\Other\codexbar.exe"));
        assert!(matches!(
            decide_for_runtime_identity(&runtime, "current-user".into(), &[matching, foreign]),
            StartupDecision::BlockUnknownConflict { .. }
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn singleton_probe_identifier_tracks_tauri_configuration() {
        let configuration: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();

        assert_eq!(
            configuration
                .get("identifier")
                .and_then(serde_json::Value::as_str),
            Some(SINGLE_INSTANCE_IDENTIFIER),
        );
    }

    #[test]
    fn an_unverifiable_same_user_candidate_blocks_startup_without_targeting_it() {
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".to_string(),
            build: stable_build("0.46.0", 'b'),
            distribution: DistributionKind::InstalledStable,
            executable_sha256: "b".repeat(64),
        };
        let candidate = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".to_string(),
            executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
            executable_sha256: String::new(),
            build: None,
            distribution: DistributionKind::Unknown,
            evidence: ProcessEvidence::UnverifiableCandidate,
        };

        assert_eq!(
            decide_startup_ownership(&current, &[candidate]),
            StartupDecision::BlockUnknownConflict {
                conflicts: vec![SafeProcessConflict {
                    pid: 101,
                    executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
                    executable_sha256: String::new(),
                    action: SafeProcessConflictAction::CloseManually,
                }],
            },
        );
    }

    #[test]
    fn a_proven_non_codexbar_or_different_user_process_is_ignored() {
        let current = StartupContext {
            current_pid: 100,
            current_user_sid_hash: "current-user".to_string(),
            build: stable_build("0.46.0", 'b'),
            distribution: DistributionKind::InstalledStable,
            executable_sha256: "b".repeat(64),
        };
        let other_product = ProcessIdentity {
            pid: 101,
            user_sid_hash: "current-user".to_string(),
            executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
            executable_sha256: "a".repeat(64),
            build: None,
            distribution: DistributionKind::Unknown,
            evidence: ProcessEvidence::VerifiedOtherProduct,
        };
        let other_user = ProcessIdentity {
            pid: 102,
            user_sid_hash: "different-user".to_string(),
            executable_path: PathBuf::from(r"C:\Tools\codexbar.exe"),
            executable_sha256: String::new(),
            build: None,
            distribution: DistributionKind::Unknown,
            evidence: ProcessEvidence::UnverifiableCandidate,
        };

        assert_eq!(
            decide_startup_ownership(&current, &[other_product, other_user]),
            StartupDecision::Continue,
        );
    }
}
