//! Embed an immutable, credential-free build identity in every CodexBar binary.

mod build_identity_shared;

use build_identity_shared::{
    PACKAGE_MANIFEST_PATHS, is_full_git_commit, package_manifest_sha256, parse_toolchains_json,
    validate_stable_inputs, validate_timestamp,
};
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASE_COMMIT_ENV: &str = "CODEXBAR_RELEASE_COMMIT";
const BUILD_CHANNEL_ENV: &str = "CODEXBAR_BUILD_CHANNEL";
const BUILD_TIMESTAMP_ENV: &str = "CODEXBAR_BUILD_TIMESTAMP";
const TOOLCHAINS_ENV: &str = "CODEXBAR_TOOLCHAINS_JSON";

fn main() {
    emit_environment_rerun_rules();

    let repo_root = repo_root();
    emit_source_rerun_rules(&repo_root);

    let detected_commit = git_output(&repo_root, &["rev-parse", "HEAD"])
        .filter(|value| is_full_git_commit(value))
        .unwrap_or_else(|| "unknown".to_string());
    let release_commit = nonempty_environment(RELEASE_COMMIT_ENV);
    let source_state = tracked_source_state(&repo_root);
    let build_channel = build_channel();
    let provided_timestamp = nonempty_environment(BUILD_TIMESTAMP_ENV);
    let provided_toolchains = nonempty_environment(TOOLCHAINS_ENV)
        .map(|value| parse_toolchains_json(&value).unwrap_or_else(|error| panic!("{error}")));

    if build_channel == "stable" {
        validate_stable_inputs(
            &detected_commit,
            release_commit.as_deref(),
            &source_state,
            provided_timestamp.as_deref(),
            provided_toolchains.as_ref(),
        )
        .unwrap_or_else(|error| panic!("stable BuildIdentity validation failed: {error}"));
    }

    let git_commit = if is_full_git_commit(&detected_commit) {
        detected_commit
    } else {
        release_commit
            .filter(|value| is_full_git_commit(value))
            .unwrap_or_else(|| {
                panic!(
                    "BuildIdentity requires a full Git commit; run from a Git checkout or set {RELEASE_COMMIT_ENV}"
                )
            })
    };

    let build_timestamp =
        provided_timestamp.unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    validate_timestamp(&build_timestamp).unwrap_or_else(|error| panic!("{error}"));

    let toolchains = provided_toolchains.unwrap_or_else(detect_toolchains);
    let toolchains_json = serde_json::to_string(&toolchains)
        .expect("serializing the detected toolchain map cannot fail");
    let manifest_sha256 = package_manifest_sha256(&repo_root)
        .unwrap_or_else(|error| panic!("failed to compute BuildIdentity: {error}"));
    let build_date = DateTime::parse_from_rfc3339(&build_timestamp)
        .expect("timestamp was validated above")
        .format("%Y-%m-%d")
        .to_string();

    emit_env("CODEXBAR_GIT_COMMIT", &git_commit);
    emit_env("CODEXBAR_SOURCE_STATE", &source_state);
    emit_env("CODEXBAR_BUILD_CHANNEL", &build_channel);
    emit_env("CODEXBAR_BUILD_TIMESTAMP", &build_timestamp);
    emit_env("CODEXBAR_TOOLCHAINS_JSON", &toolchains_json);
    emit_env("CODEXBAR_PACKAGE_MANIFEST_SHA256", &manifest_sha256);

    // Keep the legacy fields until all existing surfaces use BuildIdentity.
    emit_env("GIT_COMMIT", &git_commit[..12]);
    emit_env("BUILD_DATE", &build_date);
}

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo must provide CARGO_MANIFEST_DIR to the build script"),
    );
    manifest_dir
        .parent()
        .expect("rust/Cargo.toml must have a repository parent")
        .to_path_buf()
}

fn build_channel() -> String {
    let value = nonempty_environment(BUILD_CHANNEL_ENV).unwrap_or_else(|| "dev".to_string());
    match value.to_ascii_lowercase().as_str() {
        "dev" => "dev".to_string(),
        "stable" => "stable".to_string(),
        _ => panic!("{BUILD_CHANNEL_ENV} must be either dev or stable; got {value}"),
    }
}

fn nonempty_environment(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn tracked_source_state(repo_root: &Path) -> String {
    match Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) if output.status.success() && output.stdout.is_empty() => "clean".to_string(),
        Ok(output) if output.status.success() => "dirty".to_string(),
        _ => "unknown".to_string(),
    }
}

fn git_output(repo_root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
}

fn detect_toolchains() -> BTreeMap<String, String> {
    [
        ("cargo", command_version("cargo", &["--version"])),
        ("node", command_version("node", &["--version"])),
        ("pnpm", command_version("pnpm", &["--version"])),
        ("rustc", command_version("rustc", &["--version"])),
    ]
    .into_iter()
    .map(|(name, version)| (name.to_string(), version))
    .collect()
}

fn command_version(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn emit_environment_rerun_rules() {
    for name in [
        RELEASE_COMMIT_ENV,
        BUILD_CHANNEL_ENV,
        BUILD_TIMESTAMP_ENV,
        TOOLCHAINS_ENV,
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
}

fn emit_source_rerun_rules(repo_root: &Path) {
    for relative_path in PACKAGE_MANIFEST_PATHS {
        println!(
            "cargo:rerun-if-changed={}",
            repo_root.join(relative_path).display()
        );
    }
    for relative_path in tracked_files(repo_root) {
        println!(
            "cargo:rerun-if-changed={}",
            repo_root.join(relative_path).display()
        );
    }

    let Some(git_dir) = resolve_git_dir(repo_root) else {
        return;
    };
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    emit_rerun_if_exists(&git_dir.join("index"));
    emit_rerun_if_exists(&git_dir.join("packed-refs"));
    if let Ok(head) = fs::read_to_string(&head_path)
        && let Some(reference) = head.trim().strip_prefix("ref: ")
    {
        emit_rerun_if_exists(&git_dir.join(reference.trim()));
    }
}

fn tracked_files(repo_root: &Path) -> Vec<PathBuf> {
    let Ok(output) = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repo_root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| String::from_utf8(path.to_vec()).ok())
        .map(PathBuf::from)
        .filter(|path| repo_root.join(path).exists())
        .collect()
}

fn emit_rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn resolve_git_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    let contents = fs::read_to_string(&dot_git).ok()?;
    let path = contents.trim().strip_prefix("gitdir: ")?.trim();
    let path = PathBuf::from(OsString::from(path));
    Some(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

fn emit_env(name: &str, value: &str) {
    assert!(
        !value.contains('\n') && !value.contains('\r'),
        "build identity value {name} contains a newline"
    );
    println!("cargo:rustc-env={name}={value}");
}
