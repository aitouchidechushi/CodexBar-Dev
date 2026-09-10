//! Config command implementation
//!
//! Utilities for validating and inspecting configuration.

use clap::{Parser, Subcommand};

use crate::core::{ProviderId, TokenAccountRepository, instantiate_provider};
use crate::settings::{ApiKeyRepository, ManualCookieRepository, Settings, SettingsRepository};
use crate::storage::LoadState;

/// Arguments for the config command
#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Validate configuration files
    Validate,
    /// Dump configuration to stdout
    Dump {
        /// Output format: json or toml
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// List providers and enabled state
    Providers,
    /// Enable a provider
    Enable {
        /// Provider CLI name or alias
        provider: String,
    },
    /// Disable a provider
    Disable {
        /// Provider CLI name or alias
        provider: String,
    },
    /// Store an API key for a provider
    SetApiKey {
        /// Provider CLI name or alias
        provider: String,
        /// API key to store
        #[arg(long = "api-key")]
        api_key: Option<String>,
        /// Read API key from stdin
        #[arg(long)]
        stdin: bool,
        /// Store the key without enabling the provider
        #[arg(long = "no-enable")]
        no_enable: bool,
    },
    /// Show configuration file paths
    Path,
}

/// Run the config command
pub async fn run(args: ConfigArgs) -> anyhow::Result<()> {
    let (repository, settings) = super::discover_verified_settings()?;
    run_with_settings(args, &repository, &settings).await
}

async fn run_with_repository(
    args: ConfigArgs,
    repository: &SettingsRepository,
) -> anyhow::Result<()> {
    let settings = super::load_verified_settings(repository)?;
    run_with_settings(args, repository, &settings).await
}

async fn run_with_settings(
    args: ConfigArgs,
    repository: &SettingsRepository,
    settings: &Settings,
) -> anyhow::Result<()> {
    match args.command {
        ConfigCommand::Validate => validate_config(repository).await,
        ConfigCommand::Dump { format } => dump_config(settings, &format).await,
        ConfigCommand::Providers => list_providers(settings).await,
        ConfigCommand::Enable { provider } => {
            set_provider_enabled(repository, &provider, true).await
        }
        ConfigCommand::Disable { provider } => {
            set_provider_enabled(repository, &provider, false).await
        }
        ConfigCommand::SetApiKey {
            provider,
            api_key,
            stdin,
            no_enable,
        } => set_api_key(repository, &provider, api_key.as_deref(), stdin, !no_enable).await,
        ConfigCommand::Path => show_paths(repository).await,
    }
}

/// Validate configuration files
async fn validate_config(repository: &SettingsRepository) -> anyhow::Result<()> {
    let mut report = ValidationReport::default();
    validate_settings_config(repository, &mut report);
    validate_manual_cookies_config(&mut report);
    validate_token_accounts_config(&mut report);
    report.finish()
}

#[derive(Default)]
struct ValidationReport {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl ValidationReport {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    fn finish(self) -> anyhow::Result<()> {
        print_validation_summary(&self.errors, &self.warnings)
    }
}

fn validate_settings_config(repository: &SettingsRepository, report: &mut ValidationReport) {
    print!("Checking settings.v2... ");
    if repository.path().exists() {
        println!("OK");
    } else {
        println!("NOT FOUND (using defaults)");
        report.warning("settings.v2: File does not exist, using defaults");
    }
}

fn validate_manual_cookies_config(report: &mut ValidationReport) {
    print!("Checking manual-cookies.v2... ");
    match ManualCookieRepository::discover().and_then(|repository| repository.load()) {
        Ok(LoadState::Missing) => println!("NOT FOUND (none configured)"),
        Ok(LoadState::Loaded(_)) => println!("OK"),
        Ok(state) => {
            println!("INVALID");
            report.error(format!(
                "manual-cookies.v2: storage requires recovery ({:?})",
                state.kind()
            ));
        }
        Err(error) => {
            println!("ERROR");
            report.error(format!("manual-cookies.v2: {error}"));
        }
    }
}

fn validate_token_accounts_config(report: &mut ValidationReport) {
    print!("Checking token-accounts.v2... ");
    match TokenAccountRepository::discover().and_then(|repository| repository.load()) {
        Ok(LoadState::Missing) => println!("NOT FOUND (none configured)"),
        Ok(LoadState::Loaded(_)) => println!("OK"),
        Ok(state) => {
            println!("INVALID");
            report.error(format!(
                "token-accounts.v2: storage requires recovery ({:?})",
                state.kind()
            ));
        }
        Err(error) => {
            println!("ERROR");
            report.error(format!("token-accounts.v2: {error}"));
        }
    }
}

fn print_validation_summary(errors: &[String], warnings: &[String]) -> anyhow::Result<()> {
    println!();
    if errors.is_empty() && warnings.is_empty() {
        println!("Configuration is valid.");
    } else {
        if !warnings.is_empty() {
            println!("Warnings:");
            for w in warnings {
                println!("  - {}", w);
            }
        }
        if !errors.is_empty() {
            println!("Errors:");
            for e in errors {
                println!("  - {}", e);
            }
            anyhow::bail!(
                "Configuration validation failed with {} error(s).",
                errors.len()
            );
        }
    }

    Ok(())
}

/// Dump configuration to stdout
async fn dump_config(settings: &Settings, format: &str) -> anyhow::Result<()> {
    match format.to_lowercase().as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&settings)?;
            println!("{}", json);
        }
        "toml" => {
            let toml = toml::to_string_pretty(&settings)?;
            println!("{}", toml);
        }
        _ => {
            anyhow::bail!("Unknown format '{}'. Supported formats: json, toml", format);
        }
    }

    Ok(())
}

/// List provider enabled state.
async fn list_providers(settings: &Settings) -> anyhow::Result<()> {
    for id in ProviderId::all() {
        let state = if settings.is_provider_enabled(*id) {
            "enabled"
        } else {
            "disabled"
        };
        let default_marker = if instantiate_provider(*id).metadata().default_enabled {
            " default"
        } else {
            ""
        };
        println!(
            "{}: {}{} ({})",
            id.cli_name(),
            state,
            default_marker,
            id.display_name()
        );
    }
    Ok(())
}

/// Enable or disable a provider by CLI name.
async fn set_provider_enabled(
    repository: &SettingsRepository,
    provider: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    let id = parse_provider(provider)?;
    repository.mutate(|settings| {
        if enabled {
            settings.enable_provider(id);
        } else {
            settings.disable_provider(id);
        }
        Ok(())
    })?;
    let state = if enabled { "enabled" } else { "disabled" };
    println!("Config: {state} {}", id.display_name());
    Ok(())
}

/// Store an API key and optionally enable the provider.
async fn set_api_key(
    settings_repository: &SettingsRepository,
    provider: &str,
    api_key: Option<&str>,
    read_from_stdin: bool,
    enable_provider: bool,
) -> anyhow::Result<()> {
    let id = parse_provider(provider)?;
    ensure_provider_accepts_api_key(id)?;
    let api_key = resolve_api_key_input(api_key, read_from_stdin)?;

    ApiKeyRepository::discover()?.mutate(|keys| {
        if let Some(credential_id) = keys.entries(id.cli_name()).first().map(|entry| entry.id) {
            keys.replace_secret(id.cli_name(), credential_id, &api_key)?;
        } else {
            keys.add(id.cli_name(), &api_key, None)?;
        }
        Ok(())
    })?;

    if enable_provider {
        settings_repository.mutate(|settings| {
            settings.enable_provider(id);
            Ok(())
        })?;
    }

    let suffix = if enable_provider { " and enabled" } else { "" };
    println!("Config: stored API key for {}{suffix}", id.display_name());
    Ok(())
}

fn parse_provider(raw: &str) -> anyhow::Result<ProviderId> {
    ProviderId::from_cli_name(raw).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown provider '{}'. Run `codexbar config providers` to list providers.",
            raw
        )
    })
}

fn ensure_provider_accepts_api_key(id: ProviderId) -> anyhow::Result<()> {
    if crate::settings::supports_app_managed_api_key(id) {
        return Ok(());
    }
    anyhow::bail!("{} does not support stored API keys.", id.display_name())
}

fn resolve_api_key_input(api_key: Option<&str>, read_from_stdin: bool) -> anyhow::Result<String> {
    if api_key.is_some() && read_from_stdin {
        anyhow::bail!("Use either --api-key or --stdin, not both.");
    }

    let raw = if read_from_stdin {
        let mut buffer = String::new();
        use std::io::Read;
        std::io::stdin().read_to_string(&mut buffer)?;
        Some(buffer)
    } else {
        api_key.map(ToString::to_string)
    };

    let mut value = raw
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing API key. Pass --api-key <key> or use --stdin."))?;

    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value.remove(0);
        value.pop();
    }

    let value = value.trim().to_string();
    if value.is_empty() {
        anyhow::bail!("Missing API key. Pass --api-key <key> or use --stdin.");
    }
    Ok(value)
}

/// Show configuration file paths
async fn show_paths(settings_repository: &SettingsRepository) -> anyhow::Result<()> {
    println!("Configuration paths:");

    let settings_path = settings_repository.path();
    let exists = if settings_path.exists() {
        ""
    } else {
        " (not found)"
    };
    println!("  Settings:       {}{}", settings_path.display(), exists);

    let manual_cookie_repository = ManualCookieRepository::discover()?;
    let manual_cookie_path = manual_cookie_repository.path();
    let exists = if manual_cookie_path.exists() {
        ""
    } else {
        " (not found)"
    };
    println!(
        "  Manual cookies: {}{}",
        manual_cookie_path.display(),
        exists
    );

    let token_repository = TokenAccountRepository::discover()?;
    let token_path = token_repository.path();
    let exists = if token_path.exists() {
        ""
    } else {
        " (not found)"
    };
    println!("  Token accounts: {}{}", token_path.display(), exists);

    // Show config directory
    if let Some(config_dir) = dirs::config_dir() {
        let codexbar_dir = config_dir.join("CodexBar");
        println!();
        println!("Config directory: {}", codexbar_dir.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_file::PlatformSecretProtector;
    use crate::storage::{Protection, StoragePaths, UserScopeId};
    use std::sync::Arc;

    fn invalid_settings_repository() -> (std::path::PathBuf, SettingsRepository, Vec<u8>) {
        let root = std::env::temp_dir().join(format!(
            "codexbar-cli-config-invalid-settings-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = SettingsRepository::with_protection(
            paths,
            UserScopeId::from_stable_identifier(b"cli-config-test-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        );
        std::fs::create_dir_all(repository.path().parent().unwrap()).unwrap();
        let original = b"not valid settings json".to_vec();
        std::fs::write(repository.path(), &original).unwrap();
        (root, repository, original)
    }

    #[tokio::test]
    async fn corrupt_settings_block_config_mutation_without_changing_the_store() {
        let (root, repository, original) = invalid_settings_repository();

        let result = run_with_repository(
            ConfigArgs {
                command: ConfigCommand::Enable {
                    provider: "codex".to_string(),
                },
            },
            &repository,
        )
        .await;

        let message = result.unwrap_err().to_string();
        assert!(message.contains("Configuration recovery required"));
        assert_eq!(std::fs::read(repository.path()).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }
}
