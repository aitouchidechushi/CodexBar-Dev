//! `codexbar hooks` — list / enable / disable / test external hook rules.

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::core::{
    HookEvent, HookEventType, HookRunner, HooksConfig, HooksConfigRepository, ProviderId,
};
use crate::settings::{Settings, SettingsRepository};
use crate::storage::StoreKind;

#[derive(Args, Debug, Clone)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub command: HooksCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HooksCommand {
    /// Print configured hook rules
    List(HooksListArgs),
    /// Enable hooks in hooks.json (master switch)
    Enable(HooksToggleArgs),
    /// Disable hooks in hooks.json (master switch)
    Disable(HooksToggleArgs),
    /// Run matching rules for a sample event
    Test(HooksTestArgs),
}

#[derive(Args, Debug, Clone)]
pub struct HooksListArgs {
    /// Emit JSON
    #[arg(long)]
    pub json: bool,
    /// Pretty-print JSON
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Args, Debug, Clone)]
pub struct HooksToggleArgs {
    /// Emit JSON
    #[arg(long)]
    pub json: bool,
    /// Pretty-print JSON
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Args, Debug, Clone)]
pub struct HooksTestArgs {
    /// Event name (quota_low, quota_reached, quota_reset, provider_unavailable, provider_recovered, refresh_failed)
    pub event: String,
    /// Provider CLI name
    #[arg(long)]
    pub provider: String,
    /// Emit JSON
    #[arg(long)]
    pub json: bool,
    /// Pretty-print JSON
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Debug, Serialize)]
struct HooksListJson {
    enabled: bool,
    settings_hooks_enabled: bool,
    path: Option<String>,
    rules: Vec<HookRuleListItem>,
}

#[derive(Debug, Serialize)]
struct HookRuleListItem {
    enabled: bool,
    event: Option<String>,
    events: Vec<String>,
    provider: Option<String>,
    executable: String,
    arguments: Vec<String>,
    timeout_secs: u64,
}

#[derive(Debug, Serialize)]
struct HookTestResult {
    executable: String,
    event: String,
    provider: String,
    ok: bool,
    error: Option<String>,
}

pub async fn run(args: HooksArgs) -> anyhow::Result<()> {
    let (_, settings) = super::discover_verified_settings()?;
    run_with_settings(args, &settings)
}

fn run_with_repository(args: HooksArgs, repository: &SettingsRepository) -> anyhow::Result<()> {
    super::with_verified_settings(repository, |settings| run_with_settings(args, settings))
}

fn run_with_settings(args: HooksArgs, settings: &Settings) -> anyhow::Result<()> {
    match args.command {
        HooksCommand::List(a) => run_list(a, settings),
        HooksCommand::Enable(a) => run_set_enabled(true, a),
        HooksCommand::Disable(a) => run_set_enabled(false, a),
        HooksCommand::Test(a) => run_test(a, settings),
    }
}

fn run_list(args: HooksListArgs, settings: &Settings) -> anyhow::Result<()> {
    let repository = HooksConfigRepository::discover()?;
    let config = load_hooks_config(&repository)?;
    let path = Some(repository.path().display().to_string());

    if args.json {
        let payload = HooksListJson {
            enabled: config.enabled,
            settings_hooks_enabled: settings.hooks_enabled,
            path,
            rules: config
                .events
                .iter()
                .map(|r| HookRuleListItem {
                    enabled: r.enabled,
                    event: r.event.map(|e| e.as_str().to_string()),
                    events: r.events.iter().map(|e| e.as_str().to_string()).collect(),
                    provider: r.provider.clone(),
                    executable: r.executable.display().to_string(),
                    arguments: r.arguments.clone(),
                    timeout_secs: r.timeout_secs,
                })
                .collect(),
        };
        print_json(&payload, args.pretty)?;
        return Ok(());
    }

    println!(
        "Hooks: {} (settings toggle: {})",
        if config.enabled {
            "enabled"
        } else {
            "disabled"
        },
        if settings.hooks_enabled { "on" } else { "off" }
    );
    if let Some(path) = path {
        println!("Config: {path}");
    }
    if config.events.is_empty() {
        println!("No rules configured.");
        return Ok(());
    }
    for rule in &config.events {
        let state = if rule.enabled { "on" } else { "off" };
        let event = rule
            .event
            .map(|e| e.as_str().to_string())
            .or_else(|| {
                (!rule.events.is_empty()).then(|| {
                    rule.events
                        .iter()
                        .map(|e| e.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
            })
            .unwrap_or_else(|| "any".into());
        let provider = rule.provider.as_deref().unwrap_or("any");
        let command = std::iter::once(rule.executable.display().to_string())
            .chain(rule.arguments.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        println!("[{state}] {event} provider={provider}: {command}");
    }
    Ok(())
}

fn run_set_enabled(enabled: bool, args: HooksToggleArgs) -> anyhow::Result<()> {
    let repository = HooksConfigRepository::discover()?;
    run_set_enabled_with_repository(enabled, args, &repository)
}

fn run_set_enabled_with_repository(
    enabled: bool,
    args: HooksToggleArgs,
    repository: &HooksConfigRepository,
) -> anyhow::Result<()> {
    let config = repository.mutate(|config| {
        config.enabled = enabled;
        config.clone()
    })?;
    let path = repository.path();
    if args.json {
        print_json(
            &serde_json::json!({
                "enabled": config.enabled,
                "path": path.display().to_string(),
            }),
            args.pretty,
        )?;
    } else {
        println!(
            "Hooks: {} ({})",
            if enabled { "enabled" } else { "disabled" },
            path.display()
        );
    }
    Ok(())
}

fn run_test(args: HooksTestArgs, settings: &Settings) -> anyhow::Result<()> {
    let event_type = parse_event(&args.event)?;
    let provider = ProviderId::from_cli_name(&args.provider).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown provider '{}'. Use a CLI name such as codex or claude.",
            args.provider
        )
    })?;

    let event = sample_event(event_type, provider.cli_name());
    let repository = HooksConfigRepository::discover()?;
    let config = load_hooks_config(&repository)?;
    if !config.enabled {
        anyhow::bail!("Hooks are disabled. Run `codexbar hooks enable` first.");
    }
    if !settings.hooks_enabled {
        anyhow::bail!(
            "Hooks are disabled in Settings (hooks_enabled=false). Enable them in Advanced."
        );
    }

    let rules = config.matching_rules(&event);
    if rules.is_empty() {
        anyhow::bail!(
            "No hook rule matches {} for {}.",
            event_type.as_str(),
            provider.cli_name()
        );
    }

    let base_env = std::env::vars().collect();
    let mut results = Vec::new();
    for rule in rules {
        match HookRunner::run(rule, &event, &base_env) {
            Ok(()) => results.push(HookTestResult {
                executable: rule.executable.display().to_string(),
                event: event_type.as_str().into(),
                provider: provider.cli_name().into(),
                ok: true,
                error: None,
            }),
            Err(err) => results.push(HookTestResult {
                executable: rule.executable.display().to_string(),
                event: event_type.as_str().into(),
                provider: provider.cli_name().into(),
                ok: false,
                error: Some(err),
            }),
        }
    }

    if args.json {
        print_json(&results, args.pretty)?;
    } else {
        for r in &results {
            if r.ok {
                println!("ok  {} ({})", r.executable, r.event);
            } else {
                println!(
                    "err {} ({}) — {}",
                    r.executable,
                    r.event,
                    r.error.as_deref().unwrap_or("error")
                );
            }
        }
    }

    if results.iter().any(|r| !r.ok) {
        anyhow::bail!("one or more hooks failed");
    }
    Ok(())
}

fn load_hooks_config(repository: &HooksConfigRepository) -> anyhow::Result<HooksConfig> {
    repository
        .load()?
        .into_writable_for(StoreKind::Hooks)
        .map_err(anyhow::Error::from)
}

fn sample_event(event: HookEventType, provider: &str) -> HookEvent {
    let mut e = HookEvent::new(event, provider).with_window("session");
    match event {
        HookEventType::QuotaLow => e = e.with_used_percent(85.0),
        HookEventType::QuotaReached => e = e.with_used_percent(100.0),
        HookEventType::QuotaReset => e = e.with_used_percent(5.0),
        HookEventType::ProviderUnavailable => e = e.with_status("unavailable"),
        HookEventType::ProviderRecovered => e = e.with_status("ok"),
        HookEventType::RefreshFailed => e = e.with_status("refresh_failed"),
    }
    e
}

fn parse_event(raw: &str) -> anyhow::Result<HookEventType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "quota_low" => Ok(HookEventType::QuotaLow),
        "quota_reached" => Ok(HookEventType::QuotaReached),
        "quota_reset" => Ok(HookEventType::QuotaReset),
        "provider_unavailable" => Ok(HookEventType::ProviderUnavailable),
        "provider_recovered" => Ok(HookEventType::ProviderRecovered),
        "refresh_failed" => Ok(HookEventType::RefreshFailed),
        other => anyhow::bail!(
            "Unknown event '{other}'. Use one of: quota_low, quota_reached, quota_reset, provider_unavailable, provider_recovered, refresh_failed."
        ),
    }
}

fn print_json<T: Serialize>(value: &T, pretty: bool) -> anyhow::Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
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
            "codexbar-cli-hooks-invalid-settings-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = SettingsRepository::with_protection(
            paths,
            UserScopeId::from_stable_identifier(b"cli-hooks-test-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        );
        std::fs::create_dir_all(repository.path().parent().unwrap()).unwrap();
        let original = b"not valid settings json".to_vec();
        std::fs::write(repository.path(), &original).unwrap();
        (root, repository, original)
    }

    fn invalid_hooks_repository() -> (
        std::path::PathBuf,
        StoragePaths,
        HooksConfigRepository,
        Vec<u8>,
    ) {
        let root = std::env::temp_dir().join(format!(
            "codexbar-cli-hooks-invalid-hooks-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = HooksConfigRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"cli-hooks-config-test-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        );
        std::fs::create_dir_all(paths.hooks().parent().unwrap()).unwrap();
        let original = b"not valid hooks json".to_vec();
        std::fs::write(paths.hooks(), &original).unwrap();
        (root, paths, repository, original)
    }

    #[test]
    fn parses_event_names() {
        assert!(matches!(
            parse_event("quota_low").unwrap(),
            HookEventType::QuotaLow
        ));
        assert!(parse_event("nope").is_err());
    }

    #[test]
    fn sample_quota_low_has_remaining() {
        let e = sample_event(HookEventType::QuotaLow, "claude");
        assert!(e.remaining_percent.unwrap() < 20.0);
        assert_eq!(e.provider, "claude");
        assert!(e.environment_variables().contains_key("CODEXBAR_PROVIDER"));
    }

    #[test]
    fn corrupt_settings_stop_hook_commands_before_hook_config_access() {
        let (root, repository, original) = invalid_settings_repository();

        let result = run_with_repository(
            HooksArgs {
                command: HooksCommand::List(HooksListArgs {
                    json: false,
                    pretty: false,
                }),
            },
            &repository,
        );

        let message = result.unwrap_err().to_string();
        assert!(message.contains("Configuration recovery required"));
        assert_eq!(std::fs::read(repository.path()).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_hook_config_blocks_cli_toggle_without_overwriting_it() {
        let (root, paths, repository, original) = invalid_hooks_repository();

        let result = run_set_enabled_with_repository(
            true,
            HooksToggleArgs {
                json: false,
                pretty: false,
            },
            &repository,
        );

        assert!(result.unwrap_err().to_string().contains("hooks"));
        assert_eq!(std::fs::read(paths.hooks()).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }
}
