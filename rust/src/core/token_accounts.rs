//! Token Account Multi-Support
//!
//! Store and manage multiple accounts/tokens per provider.
//! Supports parallel fetching and account switching.

use crate::core::ProviderId;
use crate::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, default_protection,
    protected_file_bytes_with, restrict_storage_file,
};
use crate::storage::{
    CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest, Protection,
    RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
    StoreMutationFailureKind, StoreOperation, UserScopeId, write_bytes_verified_for,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

pub const TOKEN_ACCOUNTS_STORE_VERSION: u32 = 2;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";
static TOKEN_ACCOUNTS_MUTATION_LOCK: Mutex<()> = Mutex::new(());

/// How to inject a token into a fetch request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TokenInjection {
    /// Inject as Cookie header value
    CookieHeader,
    /// Inject as environment variable
    Environment { key: String },
}

/// Support definition for a provider's token accounts
#[derive(Debug, Clone)]
pub struct TokenAccountSupport {
    /// Display title for the UI
    pub title: &'static str,
    /// Subtitle/description for the UI
    pub subtitle: &'static str,
    /// Placeholder text for input field
    pub placeholder: &'static str,
    /// How tokens are injected
    pub injection: TokenInjection,
    /// Whether manual cookie source is required
    pub requires_manual_cookie_source: bool,
    /// Cookie name to use when normalizing (e.g., "sessionKey")
    pub cookie_name: Option<&'static str>,
}

impl TokenAccountSupport {
    /// Get token account support for a provider
    pub fn for_provider(provider: ProviderId) -> Option<Self> {
        match provider {
            ProviderId::Claude => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store Claude sessionKey cookies for settings-page usage. OAuth tokens are kept as a legacy fallback.",
                placeholder: "Paste sessionKey value or Cookie: sessionKey=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("sessionKey"),
            }),
            ProviderId::Zai => Some(TokenAccountSupport {
                title: "API tokens",
                subtitle: "Stored locally in token-accounts.json. Team usage can use workspace_id as organization|project.",
                placeholder: "Paste token...",
                injection: TokenInjection::Environment {
                    key: "Z_AI_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::Cursor => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Cursor Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::OpenCode => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple OpenCode Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Factory => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Factory Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Alibaba => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Alibaba Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::AlibabaTokenPlan => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Alibaba Token Plan Cookie headers.",
                placeholder: "Cookie: cna=...; login_aliyunid_csrf=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::MiniMax => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple MiniMax Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Augment => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Augment Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Amp => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Amp Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Ollama => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Ollama Cookie headers or __Secure-session values.",
                placeholder: "__Secure-session value or Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("__Secure-session"),
            }),
            ProviderId::T3Chat => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple T3 Chat Cookie headers or full browser cURL captures.",
                placeholder: "Cookie: ... or curl ... -H 'Cookie: ...'",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Mistral => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Mistral Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Manus => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Manus session_id values.",
                placeholder: "session_id value or Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("session_id"),
            }),
            ProviderId::MiMo => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Xiaomi MiMo Cookie headers.",
                placeholder: "Cookie: api-platform_serviceToken=...; userId=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::CommandCode => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Command Code Cookie headers or Better Auth values.",
                placeholder: "Cookie: __Secure-commandcode_prod_.session_token=... or better-auth value",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("__Secure-better-auth.session_token"),
            }),
            ProviderId::Qoder => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Qoder Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Sakana => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Sakana Console Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Sub2Api => Some(TokenAccountSupport {
                title: "Group API keys",
                subtitle: "Store multiple sub2api group API keys with labels such as Claude, Codex, or Gemini.",
                placeholder: "sk-...",
                injection: TokenInjection::Environment {
                    key: "SUB2API_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::DeepInfra => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple DeepInfra API keys.",
                placeholder: "API key from deepinfra.com/dash",
                injection: TokenInjection::Environment {
                    key: "DEEPINFRA_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::AiAnd => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple ai& API keys.",
                placeholder: "API key from console.aiand.com",
                injection: TokenInjection::Environment {
                    key: "AIAND_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::ZenMux => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple ZenMux Management API keys.",
                placeholder: "Management API key",
                injection: TokenInjection::Environment {
                    key: "ZENMUX_MANAGEMENT_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::ClinePass => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple ClinePass API keys.",
                placeholder: "API key",
                injection: TokenInjection::Environment {
                    key: "CLINEPASS_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::Neuralwatt => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple Neuralwatt API keys.",
                placeholder: "API key",
                injection: TokenInjection::Environment {
                    key: "NEURALWATT_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            // Upstream 0.45 #2271: labeled OpenRouter API keys via token accounts.
            ProviderId::OpenRouter => Some(TokenAccountSupport {
                title: "API keys",
                subtitle: "Store multiple OpenRouter API keys.",
                placeholder: "sk-or-v1-...",
                injection: TokenInjection::Environment {
                    key: "OPENROUTER_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::Copilot => Some(TokenAccountSupport {
                title: "GitHub accounts",
                subtitle: "Store GitHub OAuth tokens for Copilot plan usage.",
                placeholder: "Sign in with GitHub or paste a GitHub OAuth token...",
                injection: TokenInjection::Environment {
                    key: "GITHUB_TOKEN".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            // These providers don't support token accounts
            ProviderId::Codex
            | ProviderId::Gemini
            | ProviderId::Antigravity
            | ProviderId::Kiro
            | ProviderId::VertexAI
            | ProviderId::Kimi
            | ProviderId::KimiK2
            | ProviderId::JetBrains
            | ProviderId::Warp
            | ProviderId::AzureOpenAI
            | ProviderId::NanoGPT
            | ProviderId::Infini
            | ProviderId::Perplexity
            | ProviderId::Abacus
            | ProviderId::OpenCodeGo
            | ProviderId::Kilo
            | ProviderId::Bedrock
            | ProviderId::Codebuff
            | ProviderId::DeepSeek
            | ProviderId::Windsurf
            | ProviderId::Doubao
            | ProviderId::Crof
            | ProviderId::StepFun
            | ProviderId::Venice
            | ProviderId::OpenAIApi
            | ProviderId::Grok
            | ProviderId::ElevenLabs
            | ProviderId::Deepgram
            | ProviderId::Groq
            | ProviderId::LLMProxy
            | ProviderId::Chutes
            | ProviderId::LiteLLM
            | ProviderId::Poe
            | ProviderId::Devin
            | ProviderId::Zed
            | ProviderId::CrossModel
            | ProviderId::LongCat
            | ProviderId::Wayfinder => None,
        }
    }

    /// Check if a provider supports token accounts
    pub fn is_supported(provider: ProviderId) -> bool {
        Self::for_provider(provider).is_some()
    }

    /// Get environment override for a token
    pub fn env_override(provider: ProviderId, token: &str) -> Option<HashMap<String, String>> {
        let support = Self::for_provider(provider)?;
        match &support.injection {
            TokenInjection::Environment { key } => {
                let mut map = HashMap::new();
                map.insert(key.clone(), token.to_string());
                Some(map)
            }
            TokenInjection::CookieHeader => {
                // Check for Claude OAuth token
                if provider == ProviderId::Claude
                    && let Some(normalized) = Self::normalized_claude_oauth_token(token)
                    && Self::is_claude_oauth_token(&normalized)
                {
                    let mut map = HashMap::new();
                    map.insert("CODEXBAR_CLAUDE_OAUTH_TOKEN".to_string(), normalized);
                    return Some(map);
                }
                None
            }
        }
    }

    /// Normalize a cookie header for a provider
    pub fn normalized_cookie_header(provider: ProviderId, token: &str) -> String {
        let trimmed = token.trim();
        let Some(support) = Self::for_provider(provider) else {
            return trimmed.to_string();
        };

        let Some(cookie_name) = support.cookie_name else {
            return trimmed.to_string();
        };

        let lower = trimmed.to_lowercase();
        if lower.contains("cookie:") || trimmed.contains('=') {
            return trimmed.to_string();
        }

        format!("{}={}", cookie_name, trimmed)
    }

    /// Check if a token is a Claude OAuth token
    pub fn is_claude_oauth_token(token: &str) -> bool {
        let Some(trimmed) = Self::normalized_claude_oauth_token(token) else {
            return false;
        };
        let lower = trimmed.to_lowercase();
        if lower.contains("cookie:") || trimmed.contains('=') {
            return false;
        }
        lower.starts_with("sk-ant-oat")
    }

    /// Normalize a Claude OAuth token
    fn normalized_claude_oauth_token(token: &str) -> Option<String> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("bearer ") {
            Some(trimmed[7..].trim().to_string())
        } else {
            Some(trimmed.to_string())
        }
    }
}

/// A single token account for a provider
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenAccount {
    /// Unique identifier
    pub id: Uuid,
    /// User-provided label
    pub label: String,
    /// The token/cookie value
    pub token: String,
    /// When this account was added (Unix timestamp in seconds)
    pub added_at: i64,
    /// When this account was last used (Unix timestamp in seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
}

impl TokenAccount {
    /// Create a new token account
    pub fn new(label: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
            token: token.into(),
            added_at: Utc::now().timestamp(),
            last_used: None,
        }
    }

    /// Mark this account as used
    pub fn mark_used(&mut self) {
        self.last_used = Some(Utc::now().timestamp());
    }

    /// Get display name
    pub fn display_name(&self) -> &str {
        &self.label
    }

    /// Get added_at as DateTime
    pub fn added_at_datetime(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.added_at, 0).unwrap_or_else(Utc::now)
    }

    /// Get last_used as DateTime
    pub fn last_used_datetime(&self) -> Option<DateTime<Utc>> {
        self.last_used
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
    }
}

/// Account data for a provider
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccountData {
    /// File format version
    #[serde(default = "default_version")]
    pub version: u32,
    /// List of accounts
    pub accounts: Vec<TokenAccount>,
    /// Index of the active account
    #[serde(default)]
    pub active_index: usize,
}

fn default_version() -> u32 {
    1
}

impl Default for ProviderAccountData {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAccountData {
    /// Create new empty account data
    pub fn new() -> Self {
        Self {
            version: 1,
            accounts: Vec::new(),
            active_index: 0,
        }
    }

    /// Get the clamped active index
    pub fn clamped_active_index(&self) -> usize {
        if self.accounts.is_empty() {
            return 0;
        }
        self.active_index.min(self.accounts.len() - 1)
    }

    /// Get the active account
    pub fn active_account(&self) -> Option<&TokenAccount> {
        self.accounts.get(self.clamped_active_index())
    }

    /// Get the active account mutably
    pub fn active_account_mut(&mut self) -> Option<&mut TokenAccount> {
        let idx = self.clamped_active_index();
        self.accounts.get_mut(idx)
    }

    /// Add a new account
    pub fn add_account(&mut self, account: TokenAccount) {
        self.accounts.push(account);
    }

    /// Remove an account by ID
    pub fn remove_account(&mut self, id: Uuid) -> Option<TokenAccount> {
        let pos = self.accounts.iter().position(|a| a.id == id)?;
        let removed = self.accounts.remove(pos);
        // Adjust active index if needed
        if self.active_index >= self.accounts.len() && !self.accounts.is_empty() {
            self.active_index = self.accounts.len() - 1;
        }
        Some(removed)
    }

    /// Set the active account by index
    pub fn set_active(&mut self, index: usize) {
        self.active_index = index.min(self.accounts.len().saturating_sub(1));
    }

    /// Set the active account by ID
    pub fn set_active_by_id(&mut self, id: Uuid) -> bool {
        if let Some(pos) = self.accounts.iter().position(|a| a.id == id) {
            self.active_index = pos;
            true
        } else {
            false
        }
    }

    /// Check if this provider has multiple accounts
    pub fn has_multiple(&self) -> bool {
        self.accounts.len() > 1
    }

    /// Get account count
    pub fn count(&self) -> usize {
        self.accounts.len()
    }
}

pub type TokenAccounts = HashMap<ProviderId, ProviderAccountData>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenAccountsFile {
    version: u32,
    providers: HashMap<String, ProviderAccountData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAccountMutationError {
    Duplicate,
    NotFound,
    InvalidValue,
}

impl std::fmt::Display for TokenAccountMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate => formatter.write_str("the token account already exists"),
            Self::NotFound => formatter.write_str("the token account was not found"),
            Self::InvalidValue => formatter.write_str("the token account value is invalid"),
        }
    }
}

impl std::error::Error for TokenAccountMutationError {}

#[derive(Clone)]
pub struct TokenAccountRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for TokenAccountRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenAccountRepository")
            .field("store", &StoreKind::TokenAccounts)
            .field("protection", &self.protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl TokenAccountRepository {
    pub fn discover() -> Result<Self, StoreError> {
        Ok(Self::new(
            StoragePaths::discover()?,
            UserScopeId::current()?,
        ))
    }

    pub fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self::with_protection(
            paths,
            user_scope,
            default_protection(),
            Arc::new(PlatformSecretProtector),
        )
    }

    pub fn with_protection(
        paths: StoragePaths,
        user_scope: UserScopeId,
        protection: Protection,
        protector: Arc<dyn SecretProtector>,
    ) -> Self {
        Self {
            paths,
            user_scope,
            protection,
            protector,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.token_accounts()
    }

    pub fn load(&self) -> Result<LoadState<TokenAccounts>, StoreError> {
        let _process_guard = TOKEN_ACCOUNTS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    pub fn load_provider(&self, provider: ProviderId) -> Result<ProviderAccountData, StoreError> {
        Ok(self
            .load()?
            .into_writable_for(StoreKind::TokenAccounts)?
            .get(&provider)
            .cloned()
            .unwrap_or_default())
    }

    pub fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut TokenAccounts) -> Result<R, TokenAccountMutationError>,
    ) -> Result<R, StoreError> {
        let _process_guard = TOKEN_ACCOUNTS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;
        self.load_or_migrate_unlocked()?
            .into_writable_for(StoreKind::TokenAccounts)?;

        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::TokenAccounts,
            &self.user_scope,
            self.paths.token_accounts(),
            self.lock_timeout,
        )?;
        let mut accounts = self
            .load_v2_unlocked()
            .into_writable_for(StoreKind::TokenAccounts)?;
        let result = mutation(&mut accounts).map_err(map_token_account_mutation_error)?;
        validate_token_accounts(&accounts)?;
        self.write_verified(&accounts)?;

        match self.load_v2_unlocked() {
            LoadState::Loaded(read_back) if read_back == accounts => Ok(result),
            LoadState::Loaded(_) => Err(token_account_invalid_payload()),
            state => Err(token_account_load_state_error(state)),
        }
    }

    pub fn mutate_provider<R>(
        &self,
        provider: ProviderId,
        mutation: impl FnOnce(&mut ProviderAccountData) -> Result<R, TokenAccountMutationError>,
    ) -> Result<R, StoreError> {
        self.mutate(|accounts| {
            let provider_data = accounts.entry(provider).or_default();
            mutation(provider_data)
        })
    }

    pub fn remove_provider(&self, provider: ProviderId) -> Result<bool, StoreError> {
        self.mutate(|accounts| Ok(accounts.remove(&provider).is_some()))
    }

    pub fn ensure_exists(&self) -> Result<PathBuf, StoreError> {
        self.mutate(|_| Ok(()))?;
        Ok(self.paths.token_accounts().to_path_buf())
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<TokenAccounts>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.token_accounts().exists() {
            return Ok(self.load_v2_unlocked());
        }

        let source = self.paths.legacy_token_accounts();
        if !source.exists() {
            return Ok(LoadState::Missing);
        }
        let raw = match fs::read(&source) {
            Ok(raw) => raw,
            Err(source) => {
                return Ok(LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                });
            }
        };
        let accounts = match self.decode_legacy_bytes(&raw) {
            Ok(accounts) => accounts,
            Err(error) => return Ok(token_account_load_state_from_error(error)),
        };
        validate_token_accounts(&accounts)?;

        let request = MigrationRequest {
            store: StoreKind::TokenAccounts,
            source_path: source,
            source_version: 1,
            destination_path: self.paths.token_accounts().to_path_buf(),
            destination_bytes: self.protected_bytes(&accounts)?,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_v2_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_v2_unlocked())
    }

    fn load_v2_unlocked(&self) -> LoadState<TokenAccounts> {
        let raw = match fs::read(self.paths.token_accounts()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        match self.decode_v2_bytes(&raw) {
            Ok(accounts) => LoadState::Loaded(accounts),
            Err(error) => token_account_load_state_from_error(error),
        }
    }

    fn decode_v2_bytes(&self, raw: &[u8]) -> Result<TokenAccounts, StoreError> {
        let payload = self.decode_payload(raw)?;
        let file: TokenAccountsFile = serde_json::from_str(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| token_account_invalid_payload())?;
        if file.version != TOKEN_ACCOUNTS_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::TokenAccounts,
                found: file.version,
                supported: TOKEN_ACCOUNTS_STORE_VERSION,
            });
        }
        let accounts = decode_provider_accounts(file.providers, false)?;
        validate_token_accounts(&accounts)?;
        Ok(accounts)
    }

    fn decode_legacy_bytes(&self, raw: &[u8]) -> Result<TokenAccounts, StoreError> {
        let payload = self.decode_payload(raw)?;
        let file: TokenAccountsFile = serde_json::from_str(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| token_account_invalid_payload())?;
        if file.version != 1 {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::TokenAccounts,
                found: file.version,
                supported: 1,
            });
        }
        decode_provider_accounts(file.providers, true)
    }

    fn decode_payload(&self, raw: &[u8]) -> Result<String, StoreError> {
        let secure_envelope = token_account_raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::TokenAccounts,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::TokenAccounts,
                        reason,
                    }
                }
            } else {
                token_account_invalid_payload()
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::TokenAccounts,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(payload)
    }

    fn protected_bytes(&self, accounts: &TokenAccounts) -> Result<Vec<u8>, StoreError> {
        let providers = accounts
            .iter()
            .map(|(provider, data)| (provider.cli_name().to_string(), data.clone()))
            .collect();
        let file = TokenAccountsFile {
            version: TOKEN_ACCOUNTS_STORE_VERSION,
            providers,
        };
        let json = serde_json::to_string(&file).map_err(|_| StoreError::Io {
            store: StoreKind::TokenAccounts,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        protected_file_bytes_with(&json, self.protection, self.protector.as_ref()).map_err(
            |source| StoreError::Io {
                store: StoreKind::TokenAccounts,
                operation: StoreOperation::Protect,
                reason: StoreFailure::from_io(&source),
            },
        )
    }

    fn write_verified(&self, accounts: &TokenAccounts) -> Result<(), StoreError> {
        let protected = self.protected_bytes(accounts)?;
        write_bytes_verified_for(
            StoreKind::TokenAccounts,
            self.paths.token_accounts(),
            &protected,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.token_accounts()).map_err(|source| StoreError::Io {
            store: StoreKind::TokenAccounts,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn decode_provider_accounts(
    providers: HashMap<String, ProviderAccountData>,
    allow_legacy_default_version: bool,
) -> Result<TokenAccounts, StoreError> {
    let mut accounts = HashMap::with_capacity(providers.len());
    for (name, mut data) in providers {
        if allow_legacy_default_version && data.version == 0 {
            data.version = 1;
        }
        if data.version != 1 {
            return Err(token_account_invalid_payload());
        }
        let provider =
            ProviderId::from_cli_name(&name).ok_or_else(token_account_invalid_payload)?;
        if accounts.insert(provider, data).is_some() {
            return Err(token_account_invalid_payload());
        }
    }
    Ok(accounts)
}

fn validate_token_accounts(accounts: &TokenAccounts) -> Result<(), StoreError> {
    for data in accounts.values() {
        if data.version != 1 {
            return Err(token_account_invalid_payload());
        }
        let mut ids = std::collections::HashSet::with_capacity(data.accounts.len());
        if data.accounts.iter().any(|account| !ids.insert(account.id)) {
            return Err(token_account_invalid_payload());
        }
    }
    Ok(())
}

fn token_account_raw_is_secure_envelope(raw: &[u8]) -> bool {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|format| format == SECURE_FILE_FORMAT)
}

fn token_account_load_state_from_error(error: StoreError) -> LoadState<TokenAccounts> {
    match error {
        StoreError::CorruptEnvelope { reason, .. } => LoadState::CorruptEnvelope { reason },
        StoreError::DecryptFailed { reason, .. } => LoadState::DecryptFailed { reason },
        StoreError::InvalidPayload { reason, .. } => LoadState::InvalidPayload { reason },
        StoreError::UnsupportedVersion {
            found, supported, ..
        } => LoadState::UnsupportedVersion { found, supported },
        StoreError::LockTimeout { reason, .. } | StoreError::Locked { reason, .. } => {
            LoadState::Locked { reason }
        }
        StoreError::Io {
            operation, reason, ..
        } => LoadState::IoError { operation, reason },
        StoreError::MigrationRequired { .. } | StoreError::MutationRejected { .. } => {
            LoadState::InvalidPayload {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            }
        }
    }
}

fn token_account_load_state_error(state: LoadState<TokenAccounts>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::TokenAccounts,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => token_account_invalid_payload(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::TokenAccounts,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::TokenAccounts,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::TokenAccounts,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::TokenAccounts,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::TokenAccounts,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::TokenAccounts,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::TokenAccounts,
            operation,
            reason,
        },
    }
}

fn map_token_account_mutation_error(error: TokenAccountMutationError) -> StoreError {
    let reason = match error {
        TokenAccountMutationError::Duplicate => StoreMutationFailureKind::Duplicate,
        TokenAccountMutationError::NotFound => StoreMutationFailureKind::NotFound,
        TokenAccountMutationError::InvalidValue => StoreMutationFailureKind::InvalidValue,
    };
    StoreError::MutationRejected {
        store: StoreKind::TokenAccounts,
        reason,
        limit: None,
    }
}

fn token_account_invalid_payload() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::TokenAccounts,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

/// Override for temporarily using a different token during fetch
#[derive(Debug, Clone)]
pub struct TokenAccountOverride {
    /// The provider being overridden
    pub provider: ProviderId,
    /// The account being used
    pub account: TokenAccount,
    /// Environment variables to set
    pub env_override: Option<HashMap<String, String>>,
    /// Cookie header to use
    pub cookie_header: Option<String>,
}

impl TokenAccountOverride {
    /// Create an override from an account
    pub fn from_account(provider: ProviderId, account: TokenAccount) -> Self {
        let env_override = TokenAccountSupport::env_override(provider, &account.token);
        let cookie_header = if env_override.is_none() {
            Some(TokenAccountSupport::normalized_cookie_header(
                provider,
                &account.token,
            ))
        } else {
            None
        };

        Self {
            provider,
            account,
            env_override,
            cookie_header,
        }
    }
}

/// Maximum number of accounts to fetch per provider
pub const MAX_ACCOUNTS_PER_FETCH: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_file::PlatformSecretProtector;
    use crate::storage::{LoadState, Protection, StoragePaths, UserScopeId};
    use std::sync::{Arc, Barrier};

    fn repository_fixture() -> (PathBuf, StoragePaths, TokenAccountRepository) {
        let root = std::env::temp_dir().join(format!(
            "codexbar-token-account-repository-test-{}",
            Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = TokenAccountRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"token-account-test-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        );
        (root, paths, repository)
    }

    fn cleanup_repository_fixture(root: &std::path::Path) {
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn legacy_token_accounts_migrate_with_ids_labels_order_and_active_index() {
        let (root, paths, repository) = repository_fixture();
        let legacy = paths.legacy_token_accounts();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let legacy_default_id = Uuid::new_v4();
        let legacy_json = serde_json::json!({
            "version": 1,
            "providers": {
                "claude": {
                    "version": 1,
                    "accounts": [
                        {
                            "id": first_id,
                            "label": "Personal",
                            "token": "fixture-personal",
                            "added_at": 11
                        },
                        {
                            "id": second_id,
                            "label": "Work",
                            "token": "fixture-work",
                            "added_at": 22,
                            "last_used": 33
                        }
                    ],
                    "active_index": 1
                },
                "cursor": {
                    "version": 0,
                    "accounts": [
                        {
                            "id": legacy_default_id,
                            "label": "Legacy default",
                            "token": "fixture-legacy-default",
                            "added_at": 44
                        }
                    ],
                    "active_index": 0
                }
            }
        });
        fs::write(&legacy, serde_json::to_vec_pretty(&legacy_json).unwrap()).unwrap();

        let LoadState::Loaded(accounts) = repository.load().unwrap() else {
            panic!("legacy token accounts must migrate to a loaded v2 store")
        };
        let claude = accounts.get(&ProviderId::Claude).unwrap();
        assert_eq!(claude.active_index, 1);
        assert_eq!(claude.accounts.len(), 2);
        assert_eq!(claude.accounts[0].id, first_id);
        assert_eq!(claude.accounts[0].label, "Personal");
        assert_eq!(claude.accounts[1].id, second_id);
        assert_eq!(claude.accounts[1].label, "Work");
        assert_eq!(claude.accounts[1].last_used, Some(33));
        let cursor = accounts.get(&ProviderId::Cursor).unwrap();
        assert_eq!(cursor.version, 1);
        assert_eq!(cursor.accounts[0].id, legacy_default_id);
        assert_eq!(cursor.accounts[0].label, "Legacy default");
        assert!(paths.token_accounts().exists());
        assert!(legacy.exists());

        fs::write(
            &legacy,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "providers": {}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(repository.load().unwrap(), LoadState::Loaded(accounts));

        cleanup_repository_fixture(&root);
    }

    #[test]
    fn corrupt_token_account_store_is_not_empty_and_cannot_be_overwritten() {
        let (root, paths, repository) = repository_fixture();
        fs::create_dir_all(paths.token_accounts().parent().unwrap()).unwrap();
        let corrupt = b"{not-valid-json";
        fs::write(paths.token_accounts(), corrupt).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        assert!(
            repository
                .mutate_provider(ProviderId::Claude, |provider| {
                    provider.add_account(TokenAccount::new("Replacement", "fixture"));
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(fs::read(paths.token_accounts()).unwrap(), corrupt);

        cleanup_repository_fixture(&root);
    }

    #[test]
    fn concurrent_provider_transactions_preserve_both_providers() {
        let (root, _paths, repository) = repository_fixture();
        let barrier = Arc::new(Barrier::new(2));
        let first_repository = repository.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_repository
                .mutate_provider(ProviderId::Claude, |provider| {
                    provider.add_account(TokenAccount::new("Claude", "fixture-claude"));
                    Ok(())
                })
                .unwrap();
        });
        let second_repository = repository.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_repository
                .mutate_provider(ProviderId::Cursor, |provider| {
                    provider.add_account(TokenAccount::new("Cursor", "fixture-cursor"));
                    Ok(())
                })
                .unwrap();
        });

        first.join().unwrap();
        second.join().unwrap();
        let LoadState::Loaded(accounts) = repository.load().unwrap() else {
            panic!("concurrent transactions must leave a readable store")
        };
        assert_eq!(accounts[&ProviderId::Claude].accounts[0].label, "Claude");
        assert_eq!(accounts[&ProviderId::Cursor].accounts[0].label, "Cursor");

        cleanup_repository_fixture(&root);
    }

    #[test]
    fn future_token_account_version_cannot_be_overwritten_by_this_build() {
        let (root, paths, repository) = repository_fixture();
        fs::create_dir_all(paths.token_accounts().parent().unwrap()).unwrap();
        let future = br#"{"version":99,"providers":{}}"#;
        fs::write(paths.token_accounts(), future).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::UnsupportedVersion {
                found: 99,
                supported: TOKEN_ACCOUNTS_STORE_VERSION
            }
        ));
        assert!(
            repository
                .mutate_provider(ProviderId::Claude, |provider| {
                    provider.add_account(TokenAccount::new("Replacement", "fixture"));
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(fs::read(paths.token_accounts()).unwrap(), future);

        cleanup_repository_fixture(&root);
    }

    #[test]
    fn test_token_account_support() {
        assert!(TokenAccountSupport::is_supported(ProviderId::Claude));
        assert!(TokenAccountSupport::is_supported(ProviderId::Cursor));
        assert!(TokenAccountSupport::is_supported(ProviderId::Copilot));
        assert!(TokenAccountSupport::is_supported(ProviderId::OpenRouter));
        assert!(!TokenAccountSupport::is_supported(ProviderId::Codex));
        assert!(!TokenAccountSupport::is_supported(ProviderId::Gemini));
    }

    #[test]
    fn openrouter_token_accounts_inject_api_key_env() {
        let support = TokenAccountSupport::for_provider(ProviderId::OpenRouter).unwrap();
        assert_eq!(support.title, "API keys");
        assert_eq!(support.placeholder, "sk-or-v1-...");
        assert!(!support.requires_manual_cookie_source);
        match &support.injection {
            TokenInjection::Environment { key } => assert_eq!(key, "OPENROUTER_API_KEY"),
            other => panic!("expected environment injection, got {other:?}"),
        }

        let mut data = ProviderAccountData::new();
        data.add_account(TokenAccount::new("Personal", "sk-or-v1-personal"));
        data.add_account(TokenAccount::new("Work", "sk-or-v1-work"));
        data.set_active(1);

        let active = data.active_account().unwrap();
        assert_eq!(active.label, "Work");
        let env = TokenAccountSupport::env_override(ProviderId::OpenRouter, &active.token).unwrap();
        assert_eq!(
            env.get("OPENROUTER_API_KEY").map(String::as_str),
            Some("sk-or-v1-work")
        );

        let override_data =
            TokenAccountOverride::from_account(ProviderId::OpenRouter, active.clone());
        assert_eq!(
            override_data
                .env_override
                .as_ref()
                .and_then(|m| m.get("OPENROUTER_API_KEY"))
                .map(String::as_str),
            Some("sk-or-v1-work")
        );
        assert!(override_data.cookie_header.is_none());
    }

    #[test]
    fn test_claude_oauth_detection() {
        assert!(TokenAccountSupport::is_claude_oauth_token(
            "sk-ant-oat01-abc123"
        ));
        assert!(TokenAccountSupport::is_claude_oauth_token(
            "Bearer sk-ant-oat01-abc123"
        ));
        assert!(!TokenAccountSupport::is_claude_oauth_token(
            "sessionKey=abc123"
        ));
        assert!(!TokenAccountSupport::is_claude_oauth_token(
            "Cookie: foo=bar"
        ));
    }

    #[test]
    fn test_normalize_cookie_header() {
        let header =
            TokenAccountSupport::normalized_cookie_header(ProviderId::Claude, "abc123token");
        assert_eq!(header, "sessionKey=abc123token");

        let header = TokenAccountSupport::normalized_cookie_header(
            ProviderId::Claude,
            "sessionKey=already_formatted",
        );
        assert_eq!(header, "sessionKey=already_formatted");

        let header = TokenAccountSupport::normalized_cookie_header(ProviderId::Ollama, "abc123");
        assert_eq!(header, "__Secure-session=abc123");
    }

    #[test]
    fn test_provider_account_data() {
        assert_eq!(ProviderAccountData::default().version, 1);
        let mut data = ProviderAccountData::new();
        assert_eq!(data.clamped_active_index(), 0);
        assert!(data.active_account().is_none());

        let account = TokenAccount::new("Test", "token123");
        let id = account.id;
        data.add_account(account);

        assert_eq!(data.count(), 1);
        assert!(data.active_account().is_some());
        assert_eq!(data.active_account().unwrap().label, "Test");

        data.remove_account(id);
        assert_eq!(data.count(), 0);
    }

    #[test]
    fn test_multiple_accounts() {
        let mut data = ProviderAccountData::new();
        data.add_account(TokenAccount::new("Account 1", "token1"));
        data.add_account(TokenAccount::new("Account 2", "token2"));

        assert!(data.has_multiple());
        assert_eq!(data.active_account().unwrap().label, "Account 1");

        data.set_active(1);
        assert_eq!(data.active_account().unwrap().label, "Account 2");
    }
}
