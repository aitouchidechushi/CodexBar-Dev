use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildIdentity {
    pub product_name: String,
    pub semantic_version: String,
    pub git_commit: String,
    pub short_commit: String,
    pub build_channel: BuildChannel,
    pub source_state: SourceState,
    pub build_timestamp: String,
    pub toolchains: BTreeMap<String, String>,
    pub package_manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuildChannel {
    Dev,
    Stable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceState {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DistributionKind {
    InstalledStable,
    LegacyInstalled,
    Portable,
    Development,
    Unknown,
}

impl BuildIdentity {
    pub fn compiled() -> Self {
        Self::try_compiled()
            .unwrap_or_else(|error| panic!("invalid embedded BuildIdentity: {error}"))
    }

    fn try_compiled() -> Result<Self, String> {
        let semantic_version = env!("CARGO_PKG_VERSION").to_string();
        if semantic_version.trim().is_empty() {
            return Err("semantic version is empty".to_string());
        }

        let git_commit = env!("CODEXBAR_GIT_COMMIT").to_ascii_lowercase();
        if git_commit.len() != 40 || !git_commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Git commit must contain 40 hexadecimal characters".to_string());
        }

        let build_channel = match env!("CODEXBAR_BUILD_CHANNEL") {
            "dev" => BuildChannel::Dev,
            "stable" => BuildChannel::Stable,
            value => return Err(format!("unsupported build channel {value}")),
        };
        let source_state = match env!("CODEXBAR_SOURCE_STATE") {
            "clean" => SourceState::Clean,
            "dirty" => SourceState::Dirty,
            "unknown" => SourceState::Unknown,
            value => return Err(format!("unsupported source state {value}")),
        };

        let build_timestamp = env!("CODEXBAR_BUILD_TIMESTAMP").to_string();
        DateTime::parse_from_rfc3339(&build_timestamp)
            .map_err(|error| format!("build timestamp must be RFC 3339: {error}"))?;

        let toolchains: BTreeMap<String, String> =
            serde_json::from_str(env!("CODEXBAR_TOOLCHAINS_JSON"))
                .map_err(|error| format!("toolchains must be a JSON object: {error}"))?;
        if toolchains.is_empty() {
            return Err("toolchains must not be empty".to_string());
        }
        if toolchains
            .iter()
            .any(|(name, version)| name.trim().is_empty() || version.trim().is_empty())
        {
            return Err("toolchain names and versions must not be empty".to_string());
        }

        let package_manifest_sha256 = env!("CODEXBAR_PACKAGE_MANIFEST_SHA256").to_ascii_lowercase();
        if package_manifest_sha256.len() != 64
            || !package_manifest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("package manifest hash must contain 64 hexadecimal characters".to_string());
        }

        if build_channel == BuildChannel::Stable {
            if source_state != SourceState::Clean {
                return Err("stable build must embed a clean source state".to_string());
            }
            if toolchains
                .values()
                .any(|value| value.trim().eq_ignore_ascii_case("unknown"))
            {
                return Err("stable build must not embed unknown toolchains".to_string());
            }
        }

        Ok(Self {
            product_name: "CodexBar".to_string(),
            semantic_version,
            short_commit: git_commit[..12].to_string(),
            git_commit,
            build_channel,
            source_state,
            build_timestamp,
            toolchains,
            package_manifest_sha256,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_identity_shared::{
        PACKAGE_MANIFEST_PATHS, package_manifest_sha256, parse_toolchains_json,
        validate_stable_inputs,
    };
    use std::fs;

    #[test]
    fn compiled_identity_is_complete_and_non_secret() {
        let identity = BuildIdentity::compiled();
        assert_eq!(identity.product_name, "CodexBar");
        assert!(!identity.semantic_version.is_empty());
        assert_eq!(identity.git_commit.len(), 40);
        assert_eq!(identity.short_commit, identity.git_commit[..12]);
        assert!(!identity.build_timestamp.is_empty());
        assert!(!identity.toolchains.is_empty());
        assert_eq!(identity.package_manifest_sha256.len(), 64);
        assert!(
            identity
                .package_manifest_sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );

        let json = serde_json::to_string(&identity).unwrap();
        assert!(json.contains("semanticVersion"));
        assert!(json.contains("sourceState"));
        for forbidden in ["apiKey", "cookie", "token", "secret"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn enum_serialization_uses_fixed_wire_values() {
        assert_eq!(
            serde_json::to_string(&BuildChannel::Dev).unwrap(),
            "\"dev\""
        );
        assert_eq!(
            serde_json::to_string(&SourceState::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&DistributionKind::InstalledStable).unwrap(),
            "\"installed-stable\""
        );
    }

    #[test]
    fn toolchain_json_must_be_a_nonempty_named_map() {
        let parsed = parse_toolchains_json(r#"{"rustc":"rustc 1.91.0"}"#).unwrap();
        assert_eq!(parsed.get("rustc").unwrap(), "rustc 1.91.0");
        assert!(parse_toolchains_json("[]").is_err());
        assert!(parse_toolchains_json("{}").is_err());
        assert!(parse_toolchains_json(r#"{"":"rustc 1.91.0"}"#).is_err());
        assert!(parse_toolchains_json(r#"{"rustc":""}"#).is_err());
    }

    #[test]
    fn embedded_manifest_hash_matches_the_documented_algorithm() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        assert_eq!(
            BuildIdentity::compiled().package_manifest_sha256,
            package_manifest_sha256(repo_root).unwrap()
        );
    }

    #[test]
    fn changing_any_package_manifest_changes_the_hash() {
        let fixture = tempfile::tempdir().unwrap();
        for relative_path in PACKAGE_MANIFEST_PATHS {
            let path = fixture.path().join(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("fixture:{relative_path}")).unwrap();
        }
        let baseline = package_manifest_sha256(fixture.path()).unwrap();

        for relative_path in PACKAGE_MANIFEST_PATHS {
            let path = fixture.path().join(relative_path);
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"changed").unwrap();
            assert_ne!(
                package_manifest_sha256(fixture.path()).unwrap(),
                baseline,
                "changing {relative_path} must change the package manifest hash"
            );
            fs::write(path, original).unwrap();
        }
    }

    #[test]
    fn stable_validation_requires_reproducible_inputs() {
        let commit = "a".repeat(40);
        let other_commit = "b".repeat(40);
        let toolchains = BTreeMap::from([
            ("node".to_string(), "v25.8.1".to_string()),
            ("rustc".to_string(), "rustc 1.91.0".to_string()),
        ]);
        let timestamp = "2026-08-30T10:00:00Z";

        assert!(
            validate_stable_inputs(
                &commit,
                Some(&commit),
                "clean",
                Some(timestamp),
                Some(&toolchains)
            )
            .is_ok()
        );
        assert!(
            validate_stable_inputs(&commit, Some(&commit), "clean", None, Some(&toolchains))
                .unwrap_err()
                .contains("CODEXBAR_BUILD_TIMESTAMP")
        );
        assert!(
            validate_stable_inputs(
                &commit,
                Some(&commit),
                "dirty",
                Some(timestamp),
                Some(&toolchains)
            )
            .unwrap_err()
            .contains("clean tracked source tree")
        );
        assert!(
            validate_stable_inputs(
                &commit,
                Some(&other_commit),
                "clean",
                Some(timestamp),
                Some(&toolchains)
            )
            .unwrap_err()
            .contains("does not match")
        );

        let unknown_toolchains = BTreeMap::from([("rustc".to_string(), "unknown".to_string())]);
        assert!(
            validate_stable_inputs(
                &commit,
                Some(&commit),
                "clean",
                Some(timestamp),
                Some(&unknown_toolchains)
            )
            .unwrap_err()
            .contains("fixed, known version")
        );
    }
}
