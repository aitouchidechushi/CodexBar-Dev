use chrono::DateTime;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const PACKAGE_MANIFEST_PATHS: [&str; 6] = [
    "Cargo.lock",
    "apps/desktop-tauri/package.json",
    "apps/desktop-tauri/pnpm-lock.yaml",
    "apps/desktop-tauri/src-tauri/Cargo.toml",
    "apps/desktop-tauri/src-tauri/tauri.conf.json",
    "rust/Cargo.toml",
];

pub fn is_full_git_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn package_manifest_sha256(repo_root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();

    for relative_path in PACKAGE_MANIFEST_PATHS {
        let absolute_path = repo_root.join(relative_path);
        let bytes = fs::read(&absolute_path).map_err(|error| {
            format!(
                "failed to read package manifest {}: {error}",
                absolute_path.display()
            )
        })?;
        hasher.update(relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn parse_toolchains_json(value: &str) -> Result<BTreeMap<String, String>, String> {
    let toolchains: BTreeMap<String, String> = serde_json::from_str(value)
        .map_err(|error| format!("CODEXBAR_TOOLCHAINS_JSON must be an object: {error}"))?;
    if toolchains.is_empty() {
        return Err("CODEXBAR_TOOLCHAINS_JSON must not be empty".to_string());
    }
    if toolchains
        .iter()
        .any(|(name, version)| name.trim().is_empty() || version.trim().is_empty())
    {
        return Err(
            "CODEXBAR_TOOLCHAINS_JSON contains an empty toolchain name or version".to_string(),
        );
    }
    Ok(toolchains)
}

pub fn validate_timestamp(value: &str) -> Result<(), String> {
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|error| format!("CODEXBAR_BUILD_TIMESTAMP must be RFC 3339: {error}"))
}

pub fn validate_stable_inputs(
    detected_commit: &str,
    release_commit: Option<&str>,
    source_state: &str,
    timestamp: Option<&str>,
    toolchains: Option<&BTreeMap<String, String>>,
) -> Result<(), String> {
    if !is_full_git_commit(detected_commit) {
        return Err("stable build could not determine the full Git commit".to_string());
    }

    let release_commit = release_commit
        .ok_or_else(|| "stable build requires CODEXBAR_RELEASE_COMMIT".to_string())?;
    if !is_full_git_commit(release_commit) {
        return Err(
            "CODEXBAR_RELEASE_COMMIT must be a 40-character hexadecimal commit".to_string(),
        );
    }
    if !release_commit.eq_ignore_ascii_case(detected_commit) {
        return Err(format!(
            "CODEXBAR_RELEASE_COMMIT does not match checked-out commit: expected {detected_commit}, got {release_commit}"
        ));
    }
    if source_state != "clean" {
        return Err(format!(
            "stable build requires a clean tracked source tree; state is {source_state}"
        ));
    }

    let timestamp =
        timestamp.ok_or_else(|| "stable build requires CODEXBAR_BUILD_TIMESTAMP".to_string())?;
    validate_timestamp(timestamp)?;

    let toolchains =
        toolchains.ok_or_else(|| "stable build requires CODEXBAR_TOOLCHAINS_JSON".to_string())?;
    if toolchains.is_empty() {
        return Err("stable build requires at least one toolchain".to_string());
    }
    for (name, version) in toolchains {
        if name.trim().is_empty() {
            return Err("stable build contains an empty toolchain name".to_string());
        }
        let version = version.trim();
        if version.is_empty() || version.eq_ignore_ascii_case("unknown") {
            return Err(format!(
                "stable build toolchain {name} must have a fixed, known version"
            ));
        }
    }

    Ok(())
}
