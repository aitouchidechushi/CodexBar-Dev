use crate::secure_file::restrict_storage_directory;
use crate::storage::{StoreError, StoreFailure, StoreKind, StoreOperation};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePaths {
    roaming_root: PathBuf,
    local_root: PathBuf,
    roaming_v2: PathBuf,
    local_v2: PathBuf,
    settings: PathBuf,
    api_keys: PathBuf,
    manual_cookies: PathBuf,
    token_accounts: PathBuf,
    kimi_accounts: PathBuf,
    hooks: PathBuf,
    window_geometry: PathBuf,
    migration_journal: PathBuf,
    migration_backups: PathBuf,
    provider_last_good: PathBuf,
    logs: PathBuf,
    diagnostics: PathBuf,
}

impl StoragePaths {
    pub fn discover() -> Result<Self, StoreError> {
        let roaming = dirs::config_dir().ok_or_else(|| StoreError::Io {
            store: StoreKind::Unspecified,
            operation: StoreOperation::Load,
            reason: StoreFailure::new(crate::storage::StoreFailureKind::NotFound, None),
        })?;
        let local = dirs::data_local_dir().ok_or_else(|| StoreError::Io {
            store: StoreKind::Unspecified,
            operation: StoreOperation::Load,
            reason: StoreFailure::new(crate::storage::StoreFailureKind::NotFound, None),
        })?;
        Ok(Self::from_roots(roaming, local))
    }

    pub fn from_roots(
        roaming_config_root: impl Into<PathBuf>,
        local_data_root: impl Into<PathBuf>,
    ) -> Self {
        let roaming_root = roaming_config_root.into();
        let local_root = local_data_root.into();
        let roaming_v2 = roaming_root.join("CodexBar").join("v2");
        let local_v2 = local_root.join("CodexBar").join("v2");

        Self {
            settings: roaming_v2.join("settings.json"),
            api_keys: roaming_v2.join("api-keys.json"),
            manual_cookies: roaming_v2.join("manual-cookies.json"),
            token_accounts: roaming_v2.join("token-accounts.json"),
            kimi_accounts: roaming_v2.join("kimi-accounts.json"),
            // Hook rules are a documented user-authored JSON interface, not a
            // credential store. Keep their canonical path stable and readable
            // while the repository supplies locking and atomic writes.
            hooks: roaming_root.join("CodexBar").join("hooks.json"),
            window_geometry: roaming_v2.join("window-geometry.json"),
            migration_journal: roaming_v2.join("migration-journal.json"),
            migration_backups: roaming_v2.join("migration-backups"),
            provider_last_good: local_v2.join("provider-last-good.json"),
            logs: local_v2.join("logs"),
            diagnostics: local_v2.join("diagnostics"),
            roaming_root,
            local_root,
            roaming_v2,
            local_v2,
        }
    }

    pub fn ensure_directories(&self) -> Result<(), StoreError> {
        for path in [
            &self.roaming_v2,
            &self.local_v2,
            &self.migration_backups,
            &self.logs,
            &self.diagnostics,
        ] {
            std::fs::create_dir_all(path)
                .and_then(|()| restrict_storage_directory(path))
                .map_err(|source| StoreError::Io {
                    store: StoreKind::Unspecified,
                    operation: StoreOperation::ApplyPermissions,
                    reason: StoreFailure::from_io(&source),
                })?;
        }
        Ok(())
    }

    pub fn settings(&self) -> &Path {
        &self.settings
    }

    pub fn api_keys(&self) -> &Path {
        &self.api_keys
    }

    pub fn manual_cookies(&self) -> &Path {
        &self.manual_cookies
    }

    pub fn token_accounts(&self) -> &Path {
        &self.token_accounts
    }

    pub fn kimi_accounts(&self) -> &Path {
        &self.kimi_accounts
    }

    pub fn hooks(&self) -> &Path {
        &self.hooks
    }

    pub fn window_geometry(&self) -> &Path {
        &self.window_geometry
    }

    pub fn migration_journal(&self) -> &Path {
        &self.migration_journal
    }

    pub fn migration_backups(&self) -> &Path {
        &self.migration_backups
    }

    pub fn provider_last_good(&self) -> &Path {
        &self.provider_last_good
    }

    pub fn logs(&self) -> &Path {
        &self.logs
    }

    pub fn diagnostics(&self) -> &Path {
        &self.diagnostics
    }

    pub fn legacy_api_keys(&self) -> PathBuf {
        self.roaming_root.join("CodexBar").join("api_keys.json")
    }

    pub fn legacy_settings(&self) -> PathBuf {
        self.roaming_root.join("CodexBar").join("settings.json")
    }

    pub fn legacy_manual_cookies(&self) -> PathBuf {
        self.roaming_root
            .join("CodexBar")
            .join("manual_cookies.json")
    }

    pub fn legacy_token_accounts(&self) -> PathBuf {
        self.roaming_root
            .join("CodexBar")
            .join("token-accounts.json")
    }

    pub fn legacy_kimi_accounts(&self) -> PathBuf {
        self.roaming_root
            .join("CodexBar")
            .join("kimi_accounts.json")
    }

    /// Transitional location used only by an unreleased encrypted Hooks V2
    /// implementation. A public `hooks.json` always wins when both exist.
    pub fn previous_v2_hooks(&self) -> PathBuf {
        self.roaming_v2.join("hooks.json")
    }

    pub fn legacy_window_geometry(&self) -> PathBuf {
        self.roaming_root
            .join("CodexBar")
            .join("window_geometry.json")
    }

    pub fn store_path(&self, store: StoreKind) -> Option<&Path> {
        match store {
            StoreKind::Settings => Some(self.settings()),
            StoreKind::ApiKeys => Some(self.api_keys()),
            StoreKind::ManualCookies => Some(self.manual_cookies()),
            StoreKind::TokenAccounts => Some(self.token_accounts()),
            StoreKind::KimiAccounts => Some(self.kimi_accounts()),
            StoreKind::Hooks => Some(self.hooks()),
            StoreKind::WindowGeometry => Some(self.window_geometry()),
            StoreKind::ProviderLastGood => Some(self.provider_last_good()),
            StoreKind::MigrationJournal => Some(self.migration_journal()),
            StoreKind::Unspecified => None,
        }
    }

    pub fn diagnostic_name_for(&self, path: &Path) -> Option<&'static str> {
        if path == self.settings() {
            Some("roaming/settings.json")
        } else if path == self.api_keys() {
            Some("roaming/api-keys.json")
        } else if path == self.manual_cookies() {
            Some("roaming/manual-cookies.json")
        } else if path == self.token_accounts() {
            Some("roaming/token-accounts.json")
        } else if path == self.kimi_accounts() {
            Some("roaming/kimi-accounts.json")
        } else if path == self.hooks() {
            Some("roaming/hooks.json")
        } else if path == self.window_geometry() {
            Some("roaming/window-geometry.json")
        } else if path == self.migration_journal() {
            Some("roaming/migration-journal.json")
        } else if path == self.migration_backups() {
            Some("roaming/migration-backups")
        } else if path == self.provider_last_good() {
            Some("local/provider-last-good.json")
        } else if path == self.logs() {
            Some("local/logs")
        } else if path == self.diagnostics() {
            Some("local/diagnostics")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoreKind;
    use std::path::Path;

    #[test]
    fn injected_roots_produce_the_fixed_roaming_and_local_v2_layout() {
        let roaming = Path::new("fixture-roaming-root");
        let local = Path::new("fixture-local-root");
        let paths = StoragePaths::from_roots(roaming, local);
        let roaming_v2 = roaming.join("CodexBar").join("v2");
        let local_v2 = local.join("CodexBar").join("v2");

        assert_eq!(paths.settings(), roaming_v2.join("settings.json"));
        assert_eq!(paths.api_keys(), roaming_v2.join("api-keys.json"));
        assert_eq!(
            paths.manual_cookies(),
            roaming_v2.join("manual-cookies.json")
        );
        assert_eq!(
            paths.token_accounts(),
            roaming_v2.join("token-accounts.json")
        );
        assert_eq!(paths.kimi_accounts(), roaming_v2.join("kimi-accounts.json"));
        assert_eq!(
            paths.hooks(),
            roaming.join("CodexBar").join("hooks.json")
        );
        assert_eq!(
            paths.window_geometry(),
            roaming_v2.join("window-geometry.json")
        );
        assert_eq!(
            paths.migration_journal(),
            roaming_v2.join("migration-journal.json")
        );
        assert_eq!(
            paths.migration_backups(),
            roaming_v2.join("migration-backups")
        );
        assert_eq!(
            paths.provider_last_good(),
            local_v2.join("provider-last-good.json")
        );
        assert_eq!(paths.logs(), local_v2.join("logs"));
        assert_eq!(paths.diagnostics(), local_v2.join("diagnostics"));
    }

    #[test]
    fn v2_api_key_path_never_aliases_the_legacy_writer_path() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.legacy_api_keys(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("api_keys.json")
        );
        assert_ne!(paths.api_keys(), paths.legacy_api_keys());
        assert!(!paths.api_keys().starts_with(paths.legacy_api_keys()));
    }

    #[test]
    fn v2_settings_path_never_aliases_the_legacy_writer_path() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.legacy_settings(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("settings.json")
        );
        assert_ne!(paths.settings(), paths.legacy_settings());
        assert!(!paths.settings().starts_with(paths.legacy_settings()));
    }

    #[test]
    fn credential_v2_paths_never_alias_the_legacy_writer_paths() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.legacy_manual_cookies(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("manual_cookies.json")
        );
        assert_eq!(
            paths.legacy_token_accounts(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("token-accounts.json")
        );
        assert_ne!(paths.manual_cookies(), paths.legacy_manual_cookies());
        assert_ne!(paths.token_accounts(), paths.legacy_token_accounts());
    }

    #[test]
    fn kimi_v2_path_never_aliases_the_legacy_writer_path() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.legacy_kimi_accounts(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("kimi_accounts.json")
        );
        assert_ne!(paths.kimi_accounts(), paths.legacy_kimi_accounts());
    }

    #[test]
    fn geometry_v2_path_never_aliases_the_legacy_writer_path() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.legacy_window_geometry(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("window_geometry.json")
        );
        assert_ne!(paths.window_geometry(), paths.legacy_window_geometry());
        assert!(
            !paths
                .window_geometry()
                .starts_with(paths.legacy_window_geometry())
        );
    }

    #[test]
    fn hooks_keep_the_documented_public_path_and_isolate_only_the_transitional_v2_file() {
        let paths = StoragePaths::from_roots(
            Path::new("fixture-roaming-root"),
            Path::new("fixture-local-root"),
        );

        assert_eq!(
            paths.hooks(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("hooks.json")
        );
        assert_eq!(
            paths.previous_v2_hooks(),
            Path::new("fixture-roaming-root")
                .join("CodexBar")
                .join("v2")
                .join("hooks.json")
        );
        assert_ne!(paths.hooks(), paths.previous_v2_hooks());
    }

    #[test]
    fn diagnostics_receive_only_fixed_logical_names() {
        let paths = StoragePaths::from_roots(
            Path::new("C:/Users/fixture-sensitive/AppData/Roaming"),
            Path::new("C:/Users/fixture-sensitive/AppData/Local"),
        );

        assert_eq!(
            paths.diagnostic_name_for(paths.api_keys()),
            Some("roaming/api-keys.json")
        );
        assert_eq!(
            paths.diagnostic_name_for(paths.provider_last_good()),
            Some("local/provider-last-good.json")
        );
        assert_eq!(paths.store_path(StoreKind::ApiKeys), Some(paths.api_keys()));
        assert!(
            !paths
                .diagnostic_name_for(paths.api_keys())
                .unwrap()
                .contains("fixture-sensitive")
        );
    }
}
