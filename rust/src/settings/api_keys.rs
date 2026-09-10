use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use uuid::Uuid;

mod migration;
pub use migration::ApiKeyRepository;

pub const API_KEYS_STORE_VERSION: u32 = 2;
pub const MAX_ACTIVE_API_KEYS_PER_PROVIDER: usize = 64;
static API_KEYS_MUTATION_LOCK: Mutex<()> = Mutex::new(());

/// Versioned API-key storage for providers that need app-managed tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeys {
    pub version: u32,
    /// Provider ID -> ordered API-key entries and provider-local ordinal state.
    pub providers: HashMap<String, ApiKeyProviderEntries>,
}

impl Default for ApiKeys {
    fn default() -> Self {
        Self {
            version: API_KEYS_STORE_VERSION,
            providers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyProviderEntries {
    pub next_ordinal: u64,
    pub entries: Vec<ApiKeyEntry>,
}

impl Default for ApiKeyProviderEntries {
    fn default() -> Self {
        Self {
            next_ordinal: 1,
            entries: Vec::new(),
        }
    }
}

/// A single API key entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyEntry {
    pub id: Uuid,
    pub secret: String,
    pub saved_at: String,
    pub label: Option<String>,
    pub ordinal: u64,
    pub active: bool,
    #[serde(
        default,
        rename = "inactiveReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub inactive_reason: Option<ApiKeyInactiveReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiKeyInactiveReason {
    OverLimitMigration,
    UserDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyMutationError {
    DuplicateSecret,
    NotFound,
    InvalidOrder,
    OrdinalExhausted,
    ActiveLimitReached { limit: usize },
}

impl std::fmt::Display for ApiKeyMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::DuplicateSecret => "An identical API key already exists for this provider",
            Self::NotFound => "API key credential was not found",
            Self::InvalidOrder => "API key order must include every credential exactly once",
            Self::OrdinalExhausted => "API key display ordinal is unavailable",
            Self::ActiveLimitReached { .. } => {
                "The active API key limit has been reached for this provider"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ApiKeyMutationError {}

impl ApiKeys {
    /// Return the formal v2 store path for diagnostics without loading data.
    pub fn keys_path() -> Option<PathBuf> {
        crate::storage::StoragePaths::discover()
            .ok()
            .map(|paths| paths.api_keys().to_path_buf())
    }

    /// Compatibility entrypoint with an explicit state boundary. Callers must
    /// distinguish a missing store from recovery-required failures.
    pub fn load() -> Result<crate::storage::LoadState<Self>, crate::storage::StoreError> {
        ApiKeyRepository::discover()?.load()
    }

    pub fn entries(&self, provider_id: &str) -> &[ApiKeyEntry] {
        self.providers
            .get(provider_id)
            .map(|provider| provider.entries.as_slice())
            .unwrap_or_default()
    }

    pub fn active_entries(&self, provider_id: &str) -> impl Iterator<Item = &ApiKeyEntry> {
        self.entries(provider_id)
            .iter()
            .filter(|entry| entry.active)
    }

    /// Compatibility helper for provider-scoped callers not yet credential-aware.
    pub fn get(&self, provider_id: &str) -> Option<&str> {
        self.active_entries(provider_id)
            .next()
            .map(|entry| entry.secret.as_str())
    }

    pub fn add(
        &mut self,
        provider_id: &str,
        secret: &str,
        label: Option<&str>,
    ) -> Result<Uuid, ApiKeyMutationError> {
        if self
            .entries(provider_id)
            .iter()
            .any(|entry| entry.secret == secret)
        {
            return Err(ApiKeyMutationError::DuplicateSecret);
        }
        if self.active_entries(provider_id).count() >= MAX_ACTIVE_API_KEYS_PER_PROVIDER {
            return Err(ApiKeyMutationError::ActiveLimitReached {
                limit: MAX_ACTIVE_API_KEYS_PER_PROVIDER,
            });
        }
        let provider = self.providers.entry(provider_id.to_string()).or_default();
        let ordinal = provider.next_ordinal.max(1);
        provider.next_ordinal = ordinal
            .checked_add(1)
            .ok_or(ApiKeyMutationError::OrdinalExhausted)?;
        let id = Uuid::new_v4();
        provider.entries.push(ApiKeyEntry {
            id,
            secret: secret.to_string(),
            saved_at: current_saved_at(),
            label: normalized_label(label),
            ordinal,
            active: true,
            inactive_reason: None,
        });
        Ok(id)
    }

    pub fn update_label(&mut self, provider_id: &str, id: Uuid, label: Option<&str>) -> bool {
        let Some(entry) = self
            .providers
            .get_mut(provider_id)
            .and_then(|provider| provider.entries.iter_mut().find(|entry| entry.id == id))
        else {
            return false;
        };
        entry.label = normalized_label(label);
        entry.saved_at = current_saved_at();
        true
    }

    pub fn replace_secret(
        &mut self,
        provider_id: &str,
        id: Uuid,
        secret: &str,
    ) -> Result<(), ApiKeyMutationError> {
        let Some(provider) = self.providers.get_mut(provider_id) else {
            return Err(ApiKeyMutationError::NotFound);
        };
        if provider
            .entries
            .iter()
            .any(|entry| entry.id != id && entry.secret == secret)
        {
            return Err(ApiKeyMutationError::DuplicateSecret);
        }
        let Some(entry) = provider.entries.iter_mut().find(|entry| entry.id == id) else {
            return Err(ApiKeyMutationError::NotFound);
        };
        entry.secret = secret.to_string();
        entry.saved_at = current_saved_at();
        Ok(())
    }

    pub fn delete(&mut self, provider_id: &str, id: Uuid) -> bool {
        let Some(provider) = self.providers.get_mut(provider_id) else {
            return false;
        };
        let previous_len = provider.entries.len();
        provider.entries.retain(|entry| entry.id != id);
        provider.entries.len() != previous_len
    }

    pub fn set_active(
        &mut self,
        provider_id: &str,
        id: Uuid,
        active: bool,
    ) -> Result<(), ApiKeyMutationError> {
        let currently_active = self
            .providers
            .get(provider_id)
            .and_then(|provider| provider.entries.iter().find(|entry| entry.id == id))
            .map(|entry| entry.active)
            .ok_or(ApiKeyMutationError::NotFound)?;
        if active
            && !currently_active
            && self.active_entries(provider_id).count() >= MAX_ACTIVE_API_KEYS_PER_PROVIDER
        {
            return Err(ApiKeyMutationError::ActiveLimitReached {
                limit: MAX_ACTIVE_API_KEYS_PER_PROVIDER,
            });
        }

        let entry = self
            .providers
            .get_mut(provider_id)
            .and_then(|provider| provider.entries.iter_mut().find(|entry| entry.id == id))
            .ok_or(ApiKeyMutationError::NotFound)?;
        entry.active = active;
        entry.inactive_reason = (!active).then_some(ApiKeyInactiveReason::UserDisabled);
        entry.saved_at = current_saved_at();
        Ok(())
    }

    pub fn reorder(
        &mut self,
        provider_id: &str,
        ordered_ids: &[Uuid],
    ) -> Result<(), ApiKeyMutationError> {
        let provider = self
            .providers
            .get_mut(provider_id)
            .ok_or(ApiKeyMutationError::NotFound)?;
        let expected_ids = provider
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<HashSet<_>>();
        let supplied_ids = ordered_ids.iter().copied().collect::<HashSet<_>>();
        if ordered_ids.len() != provider.entries.len()
            || supplied_ids.len() != ordered_ids.len()
            || supplied_ids != expected_ids
        {
            return Err(ApiKeyMutationError::InvalidOrder);
        }

        let positions = ordered_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect::<HashMap<_, _>>();
        provider.entries.sort_by_key(|entry| positions[&entry.id]);
        for (ordinal, entry) in (1_u64..).zip(provider.entries.iter_mut()) {
            entry.ordinal = ordinal;
        }
        Ok(())
    }

    /// Compatibility helper for callers that still configure one provider key.
    pub fn set(&mut self, provider_id: &str, api_key: &str, label: Option<&str>) {
        if let Some(id) = self.entries(provider_id).first().map(|entry| entry.id) {
            if self.replace_secret(provider_id, id, api_key).is_err() {
                return;
            }
            self.update_label(provider_id, id, label);
        } else {
            let _ = self.add(provider_id, api_key, label);
        }
    }

    /// Compatibility helper that revokes every app-managed key for a provider.
    pub fn remove(&mut self, provider_id: &str) {
        self.providers.remove(provider_id);
    }

    /// Check if a provider has an API key configured
    pub fn has_key(&self, provider_id: &str) -> bool {
        self.active_entries(provider_id)
            .any(|entry| !entry.secret.is_empty())
    }

    /// Get all saved API keys for UI display without exposing secret material.
    pub fn get_all_for_display(&self) -> Vec<SavedApiKeyInfo> {
        let mut display = self
            .providers
            .iter()
            .flat_map(|(provider_id, provider)| {
                let provider_name = ProviderId::from_cli_name(provider_id)
                    .map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| provider_id.clone());
                provider.entries.iter().map(move |entry| SavedApiKeyInfo {
                    credential_id: entry.id,
                    provider_id: provider_id.clone(),
                    provider: provider_name.clone(),
                    label: entry
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("Key {}", entry.ordinal)),
                    custom_label: entry.label.clone(),
                    display_ordinal: entry.ordinal,
                    saved_at: entry.saved_at.clone(),
                    active: entry.active,
                    inactive_reason: entry.inactive_reason,
                })
            })
            .collect::<Vec<_>>();
        display.sort_by(|left, right| {
            left.provider_id
                .cmp(&right.provider_id)
                .then(left.display_ordinal.cmp(&right.display_ordinal))
        });
        display
    }
}

fn normalized_label(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
}

fn current_saved_at() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string()
}

/// Info about a saved API key for UI display
#[derive(Debug, Clone, Serialize)]
pub struct SavedApiKeyInfo {
    pub credential_id: Uuid,
    pub provider_id: String,
    pub provider: String,
    pub label: String,
    pub custom_label: Option<String>,
    pub display_ordinal: u64,
    pub saved_at: String,
    pub active: bool,
    pub inactive_reason: Option<ApiKeyInactiveReason>,
}

/// Provider configuration info
#[derive(Debug, Clone)]
pub struct ProviderConfigInfo {
    pub id: ProviderId,
    pub name: &'static str,
    pub requires_api_key: bool,
    pub api_key_env_var: Option<&'static str>,
    pub api_key_help: Option<&'static str>,
    pub config_file_path: Option<&'static str>,
    pub dashboard_url: Option<&'static str>,
}

/// Get configuration info for providers that need API keys
pub fn get_api_key_providers() -> Vec<ProviderConfigInfo> {
    vec![
        ProviderConfigInfo {
            id: ProviderId::Alibaba,
            name: "Alibaba Coding Plan",
            requires_api_key: true,
            api_key_env_var: Some("ALIBABA_CODING_PLAN_API_KEY"),
            api_key_help: Some("Get your Coding Plan API key from Alibaba Model Studio / Bailian"),
            config_file_path: Some("~/.codexbar/config.json"),
            dashboard_url: Some(
                "https://modelstudio.console.alibabacloud.com/ap-southeast-1/?tab=coding-plan#/efm/detail",
            ),
        },
        ProviderConfigInfo {
            id: ProviderId::Amp,
            name: "Amp (Sourcegraph)",
            requires_api_key: true,
            api_key_env_var: Some("SRC_ACCESS_TOKEN"),
            api_key_help: Some("Get your token from Sourcegraph → Settings → Access Tokens"),
            config_file_path: Some("~/.amp/config.json"),
            dashboard_url: Some("https://sourcegraph.com/cody/manage"),
        },
        ProviderConfigInfo {
            id: ProviderId::Copilot,
            name: "GitHub Copilot (legacy token)",
            requires_api_key: true,
            api_key_env_var: Some("GITHUB_TOKEN"),
            api_key_help: Some(
                "Optional fallback. Prefer Providers → Copilot → Sign in with GitHub.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://github.com/settings/copilot"),
        },
        ProviderConfigInfo {
            id: ProviderId::Zai,
            name: ProviderId::Zai.display_name(),
            requires_api_key: true,
            api_key_env_var: Some("Z_AI_API_KEY or ZAI_API_TOKEN"),
            api_key_help: Some(
                "Get your API token from z.ai Dashboard. BigModel team usage can set Z_AI_BIGMODEL_ORGANIZATION + Z_AI_BIGMODEL_PROJECT, or provider workspace_id as organization|project.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://z.ai/manage-apikey/coding-plan/personal/my-plan"),
        },
        ProviderConfigInfo {
            id: ProviderId::Warp,
            name: "Warp",
            requires_api_key: true,
            api_key_env_var: Some("WARP_API_KEY"),
            api_key_help: Some(
                "Get your API key from Warp → Settings → API Keys (docs.warp.dev/reference/cli/api-keys)",
            ),
            config_file_path: None,
            dashboard_url: Some("https://docs.warp.dev/reference/cli/api-keys"),
        },
        ProviderConfigInfo {
            id: ProviderId::Ollama,
            name: "Ollama",
            requires_api_key: false,
            api_key_env_var: Some("OLLAMA_API_KEY / OLLAMA_KEY"),
            api_key_help: Some(
                "Optional: use an Ollama API key for Cloud validation, or browser cookies for usage.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://ollama.com/settings"),
        },
        ProviderConfigInfo {
            id: ProviderId::AzureOpenAI,
            name: "Azure OpenAI",
            requires_api_key: true,
            api_key_env_var: Some(
                "AZURE_OPENAI_API_KEY + AZURE_OPENAI_ENDPOINT + AZURE_OPENAI_DEPLOYMENT",
            ),
            api_key_help: Some(
                "Use env vars, or save JSON {api_key, endpoint, deployment, api_version}; composite api_key|endpoint|deployment[|api_version] also works.",
            ),
            config_file_path: Some("~/.codexbar/api_keys.json"),
            dashboard_url: Some("https://ai.azure.com"),
        },
        ProviderConfigInfo {
            id: ProviderId::OpenRouter,
            name: "OpenRouter",
            requires_api_key: true,
            api_key_env_var: Some("OPENROUTER_API_KEY"),
            api_key_help: Some("Get your API key from openrouter.ai/settings/keys"),
            config_file_path: None,
            dashboard_url: Some("https://openrouter.ai/settings/credits"),
        },
        ProviderConfigInfo {
            id: ProviderId::NanoGPT,
            name: "NanoGPT",
            requires_api_key: true,
            api_key_env_var: Some("NANOGPT_API_KEY"),
            api_key_help: Some("Get your API key from nano-gpt.com/api"),
            config_file_path: None,
            dashboard_url: Some("https://nano-gpt.com/api"),
        },
        ProviderConfigInfo {
            id: ProviderId::Infini,
            name: "Infini AI",
            requires_api_key: true,
            api_key_env_var: Some("INFINI_API_KEY"),
            api_key_help: Some("Get your API key from Infini Cloud → Settings → API Keys"),
            config_file_path: None,
            dashboard_url: Some("https://cloud.infini-ai.com"),
        },
        ProviderConfigInfo {
            id: ProviderId::Kimi,
            name: "Kimi Code API",
            requires_api_key: true,
            api_key_env_var: Some("KIMI_CODE_API_KEY"),
            api_key_help: Some(
                "Get your Kimi Code API key from Kimi. Optional HTTPS proxy base URL: KIMI_CODE_BASE_URL.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://platform.moonshot.cn/console/api-keys"),
        },
        ProviderConfigInfo {
            id: ProviderId::MiniMax,
            name: "MiniMax Token Plan",
            requires_api_key: true,
            api_key_env_var: Some("MINIMAX_API_KEY"),
            api_key_help: Some(
                "Add the Token Plan Key from the MiniMax Coding Plan console. The default region is China mainland.",
            ),
            config_file_path: None,
            dashboard_url: Some(
                "https://platform.minimaxi.com/user-center/payment/coding-plan?cycle_type=3",
            ),
        },
        ProviderConfigInfo {
            id: ProviderId::Kilo,
            name: "Kilo",
            requires_api_key: true,
            api_key_env_var: Some("KILO_API_KEY"),
            api_key_help: Some("Get your API key from Kilo, or sign in with Kilo CLI."),
            config_file_path: Some("~/.local/share/kilo/auth.json"),
            dashboard_url: Some("https://app.kilo.ai/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::Bedrock,
            name: "AWS Bedrock",
            requires_api_key: true,
            api_key_env_var: Some(
                "AWS_ACCESS_KEY_ID:AWS_SECRET_ACCESS_KEY[:AWS_SESSION_TOKEN] or AWS_PROFILE",
            ),
            api_key_help: Some(
                "Paste access_key:secret_key[:session_token], JSON credentials, profile:name, or use AWS env vars/AWS CLI profiles.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://console.aws.amazon.com/bedrock"),
        },
        ProviderConfigInfo {
            id: ProviderId::Codebuff,
            name: "Codebuff",
            requires_api_key: true,
            api_key_env_var: Some("CODEBUFF_API_KEY"),
            api_key_help: Some(
                "Get your API key from Codebuff, or sign in with Codebuff/Manicode.",
            ),
            config_file_path: Some("~/.config/manicode/credentials.json"),
            dashboard_url: Some("https://www.codebuff.com/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::DeepSeek,
            name: "DeepSeek",
            requires_api_key: true,
            api_key_env_var: Some("DEEPSEEK_API_KEY"),
            api_key_help: Some("Get your API key from platform.deepseek.com."),
            config_file_path: None,
            dashboard_url: Some("https://platform.deepseek.com/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::DeepInfra,
            name: "DeepInfra",
            requires_api_key: true,
            api_key_env_var: Some("DEEPINFRA_API_KEY"),
            api_key_help: Some(
                "Get your API key from deepinfra.com/dash. Also accepts DEEPINFRA_TOKEN.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://deepinfra.com/dash"),
        },
        ProviderConfigInfo {
            id: ProviderId::AiAnd,
            name: "ai&",
            requires_api_key: true,
            api_key_env_var: Some("AIAND_API_KEY"),
            api_key_help: Some("Get your API key from console.aiand.com."),
            config_file_path: None,
            dashboard_url: Some("https://console.aiand.com"),
        },
        ProviderConfigInfo {
            id: ProviderId::ZenMux,
            name: "ZenMux",
            requires_api_key: true,
            api_key_env_var: Some("ZENMUX_MANAGEMENT_API_KEY"),
            api_key_help: Some(
                "Use a ZenMux Management API key (not an inference key). Also accepts ZENMUX_API_KEY.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://zenmux.ai/platform/management"),
        },
        ProviderConfigInfo {
            id: ProviderId::ClinePass,
            name: "ClinePass",
            requires_api_key: true,
            api_key_env_var: Some("CLINEPASS_API_KEY"),
            api_key_help: Some(
                "Get your API key from Cline / ClinePass. Also accepts CLINE_API_KEY.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://app.cline.bot/dashboard/subscription?personal=true"),
        },
        ProviderConfigInfo {
            id: ProviderId::Neuralwatt,
            name: "Neuralwatt",
            requires_api_key: true,
            api_key_env_var: Some("NEURALWATT_API_KEY"),
            api_key_help: Some("Get your API key from portal.neuralwatt.com."),
            config_file_path: None,
            dashboard_url: Some("https://portal.neuralwatt.com/dashboard"),
        },
        ProviderConfigInfo {
            id: ProviderId::Doubao,
            name: "Doubao / Volcengine Ark",
            requires_api_key: true,
            api_key_env_var: Some(
                "ARK_API_KEY or VOLCENGINE_ACCESS_KEY_ID + VOLCENGINE_SECRET_ACCESS_KEY",
            ),
            api_key_help: Some(
                "Use ARK_API_KEY for chat probe fallback, or paste Coding Plan credentials as access_key|secret_key|region (region defaults to cn-beijing).",
            ),
            config_file_path: None,
            dashboard_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::Crof,
            name: "Crof",
            requires_api_key: true,
            api_key_env_var: Some("CROF_API_KEY"),
            api_key_help: Some("Get your API key from Crof."),
            config_file_path: None,
            dashboard_url: Some("https://crof.ai"),
        },
        ProviderConfigInfo {
            id: ProviderId::StepFun,
            name: "StepFun",
            requires_api_key: true,
            api_key_env_var: Some("STEPFUN_OASIS_TOKEN"),
            api_key_help: Some("Paste an existing Oasis-Token from StepFun login."),
            config_file_path: None,
            dashboard_url: Some("https://platform.stepfun.com/dashboard"),
        },
        ProviderConfigInfo {
            id: ProviderId::Venice,
            name: "Venice",
            requires_api_key: true,
            api_key_env_var: Some("VENICE_API_KEY"),
            api_key_help: Some("Get your API key from Venice settings."),
            config_file_path: None,
            dashboard_url: Some("https://venice.ai/settings/api"),
        },
        ProviderConfigInfo {
            id: ProviderId::OpenAIApi,
            name: "OpenAI",
            requires_api_key: true,
            api_key_env_var: Some("OPENAI_ADMIN_KEY / OPENAI_API_KEY"),
            api_key_help: Some(
                "Use an OpenAI Admin API key for usage, or a platform key for legacy billing balance.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://platform.openai.com/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::Grok,
            name: "Grok",
            requires_api_key: false,
            api_key_env_var: None,
            api_key_help: Some("Uses Grok browser cookies or ~/.grok/auth.json."),
            config_file_path: Some("~/.grok/auth.json"),
            dashboard_url: Some("https://grok.com/settings/subscription"),
        },
        ProviderConfigInfo {
            id: ProviderId::ElevenLabs,
            name: "ElevenLabs",
            requires_api_key: true,
            api_key_env_var: Some("ELEVENLABS_API_KEY"),
            api_key_help: Some("Get your API key from ElevenLabs Settings > API Keys."),
            config_file_path: None,
            dashboard_url: Some("https://elevenlabs.io/app/settings/api-keys"),
        },
        ProviderConfigInfo {
            id: ProviderId::Deepgram,
            name: "Deepgram",
            requires_api_key: true,
            api_key_env_var: Some("DEEPGRAM_API_KEY"),
            api_key_help: Some("Use a Deepgram API key with Management API access."),
            config_file_path: None,
            dashboard_url: Some("https://console.deepgram.com/usage"),
        },
        ProviderConfigInfo {
            id: ProviderId::Groq,
            name: "Groq",
            requires_api_key: true,
            api_key_env_var: Some("GROQ_API_KEY"),
            api_key_help: Some("Groq metrics require Enterprise Prometheus metrics access."),
            config_file_path: None,
            dashboard_url: Some("https://console.groq.com/settings/metrics"),
        },
        ProviderConfigInfo {
            id: ProviderId::LLMProxy,
            name: "LLM Proxy",
            requires_api_key: true,
            api_key_env_var: Some("LLM_PROXY_API_KEY + LLM_PROXY_BASE_URL"),
            api_key_help: Some("Set an LLM Proxy API key and base URL for quota-stats."),
            config_file_path: None,
            dashboard_url: None,
        },
        ProviderConfigInfo {
            id: ProviderId::Chutes,
            name: "Chutes",
            requires_api_key: true,
            api_key_env_var: Some("CHUTES_API_KEY"),
            api_key_help: Some(
                "Paste a Chutes API key. Optional API URL override: CHUTES_API_URL.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://chutes.ai"),
        },
        ProviderConfigInfo {
            id: ProviderId::LiteLLM,
            name: "LiteLLM",
            requires_api_key: true,
            api_key_env_var: Some("LITELLM_API_KEY + LITELLM_BASE_URL"),
            api_key_help: Some(
                "Paste a LiteLLM key and set the base URL in provider extras or LITELLM_BASE_URL.",
            ),
            config_file_path: None,
            dashboard_url: None,
        },
        ProviderConfigInfo {
            id: ProviderId::Poe,
            name: "Poe",
            requires_api_key: true,
            api_key_env_var: Some("POE_API_KEY"),
            api_key_help: Some("Get your API key from Poe API settings."),
            config_file_path: None,
            dashboard_url: Some("https://poe.com/settings/subscription"),
        },
        ProviderConfigInfo {
            id: ProviderId::Devin,
            name: "Devin",
            requires_api_key: true,
            api_key_env_var: Some("DEVIN_BEARER_TOKEN + DEVIN_ORG"),
            api_key_help: Some(
                "Paste a Devin bearer token and set the organization in provider extras or DEVIN_ORG.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://app.devin.ai/settings/billing"),
        },
        ProviderConfigInfo {
            id: ProviderId::Zed,
            name: "Zed",
            requires_api_key: true,
            api_key_env_var: Some("ZED_CREDENTIALS"),
            api_key_help: Some(
                "Paste Zed credentials as `user_id access_token`; optional API URL in provider extras.",
            ),
            config_file_path: Some("~/.config/zed/settings.json"),
            dashboard_url: Some("https://zed.dev/account"),
        },
        ProviderConfigInfo {
            id: ProviderId::CrossModel,
            name: "CrossModel",
            requires_api_key: true,
            api_key_env_var: Some("CROSSMODEL_API_KEY"),
            api_key_help: Some(
                "Paste a CrossModel API key. Optional API URL override: CROSSMODEL_API_URL.",
            ),
            config_file_path: None,
            dashboard_url: Some("https://crossmodel.ai"),
        },
        ProviderConfigInfo {
            id: ProviderId::Sub2Api,
            name: "sub2api",
            requires_api_key: true,
            api_key_env_var: Some("SUB2API_API_KEY"),
            api_key_help: Some(
                "Paste a group API key and set the base URL in provider extras or SUB2API_BASE_URL. HTTPS required (loopback HTTP allowed for local dev).",
            ),
            config_file_path: None,
            dashboard_url: None,
        },
        ProviderConfigInfo {
            id: ProviderId::Factory,
            name: "Droid (Factory)",
            requires_api_key: true,
            api_key_env_var: Some("FACTORY_API_KEY"),
            api_key_help: Some(
                "Get your API key from Factory → Settings → API Keys. Optional fallback: %USERPROFILE%\\.factory\\.env. Auto mode tries the key first, then browser cookies.",
            ),
            config_file_path: Some("%USERPROFILE%\\.factory\\.env"),
            dashboard_url: Some("https://app.factory.ai/settings/api-keys"),
        },
    ]
}

/// Providers whose stored secret can be used directly for usage refreshes.
/// Cookie/session-only providers stay in the broader configuration catalog,
/// but must not expose app-managed API-key controls.
pub fn get_app_managed_api_key_providers() -> Vec<ProviderConfigInfo> {
    get_api_key_providers()
        .into_iter()
        .filter(|provider| !matches!(provider.id, ProviderId::Alibaba | ProviderId::Grok))
        .collect()
}

pub fn supports_app_managed_api_key(provider_id: ProviderId) -> bool {
    get_app_managed_api_key_providers()
        .iter()
        .any(|provider| provider.id == provider_id)
}

#[cfg(test)]
mod multi_key_tests {
    use super::*;

    #[test]
    fn glm_api_key_provider_uses_the_canonical_visible_name() {
        let zai = get_app_managed_api_key_providers()
            .into_iter()
            .find(|provider| provider.id == ProviderId::Zai)
            .expect("zai provider");

        assert_eq!(zai.name, "GLM（智谱 BigModel / z.ai）");
    }

    #[test]
    fn versioned_store_round_trips_two_keys_for_one_provider_in_order() {
        let mut keys = ApiKeys::default();

        let first_id = keys
            .add("openrouter", "sk-first", Some(" Personal "))
            .expect("add first key");
        let second_id = keys
            .add("openrouter", "sk-second", None)
            .expect("add second key");
        let serialized = serde_json::to_vec(&keys).expect("serialize v2 store");
        let loaded = serde_json::from_slice::<ApiKeys>(&serialized).expect("parse v2 store");
        assert_eq!(loaded.version, API_KEYS_STORE_VERSION);
        let entries = loaded.entries("openrouter");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, first_id);
        assert_eq!(entries[0].secret, "sk-first");
        assert_eq!(entries[0].label.as_deref(), Some("Personal"));
        assert_eq!(entries[0].ordinal, 1);
        assert_eq!(entries[1].id, second_id);
        assert_eq!(entries[1].secret, "sk-second");
        assert_eq!(entries[1].ordinal, 2);
    }

    #[test]
    fn deletion_keeps_ordinal_gap_and_later_add_uses_monotonic_ordinal() {
        let mut keys = ApiKeys::default();
        let first_id = keys.add("openrouter", "first", None).unwrap();
        let second_id = keys.add("openrouter", "second", None).unwrap();

        assert!(keys.delete("openrouter", first_id));
        let third_id = keys.add("openrouter", "third", None).unwrap();

        let entries = keys.entries("openrouter");
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].id, entries[0].ordinal), (second_id, 2));
        assert_eq!((entries[1].id, entries[1].ordinal), (third_id, 3));
    }

    #[test]
    fn display_uses_trimmed_custom_label_or_stable_key_fallback() {
        let mut keys = ApiKeys::default();
        let custom_id = keys.add("openrouter", "first", Some("  Work  ")).unwrap();
        let fallback_id = keys.add("openrouter", "second", None).unwrap();

        let display = keys.get_all_for_display();
        assert_eq!(display[0].credential_id, custom_id);
        assert_eq!(display[0].label, "Work");
        assert_eq!(display[0].display_ordinal, 1);
        assert_eq!(display[1].credential_id, fallback_id);
        assert_eq!(display[1].label, "Key 2");
        assert_eq!(display[1].display_ordinal, 2);
    }

    #[test]
    fn label_and_secret_updates_preserve_identity_and_ordinal() {
        let mut keys = ApiKeys::default();
        let id = keys.add("openrouter", "old-secret", None).unwrap();

        assert!(keys.update_label("openrouter", id, Some("  Primary  ")));
        assert!(keys.replace_secret("openrouter", id, "new-secret").is_ok());

        let entry = &keys.entries("openrouter")[0];
        assert_eq!(entry.id, id);
        assert_eq!(entry.ordinal, 1);
        assert_eq!(entry.label.as_deref(), Some("Primary"));
        assert_eq!(entry.secret, "new-secret");
    }

    #[test]
    fn reorder_requires_the_complete_provider_key_set_and_persists_display_order() {
        let mut keys = ApiKeys::default();
        let first = keys.add("openrouter", "first", None).unwrap();
        let second = keys.add("openrouter", "second", None).unwrap();

        keys.reorder("openrouter", &[second, first]).unwrap();
        let display = keys.get_all_for_display();
        assert_eq!(display[0].credential_id, second);
        assert_eq!(display[1].credential_id, first);
        assert!(keys.reorder("openrouter", &[second]).is_err());
    }

    #[test]
    fn duplicate_secrets_are_rejected_per_provider_but_duplicate_labels_are_allowed() {
        let mut keys = ApiKeys::default();
        let first_id = keys.add("openrouter", "same-secret", Some("Work")).unwrap();

        assert_eq!(
            keys.add("openrouter", "same-secret", Some("Work")),
            Err(ApiKeyMutationError::DuplicateSecret)
        );
        assert!(keys.add("zai", "same-secret", Some("Work")).is_ok());
        let second_id = keys
            .add("openrouter", "different-secret", Some("Work"))
            .unwrap();
        assert_eq!(
            keys.replace_secret("openrouter", second_id, "same-secret"),
            Err(ApiKeyMutationError::DuplicateSecret)
        );
        assert_eq!(keys.entries("openrouter")[1].secret, "different-secret");

        keys.set("openrouter", "different-secret", Some("ignored"));
        assert_eq!(keys.entries("openrouter")[0].id, first_id);
        assert_eq!(keys.entries("openrouter")[0].secret, "same-secret");
    }

    #[test]
    fn active_limit_rejects_the_sixty_fifth_new_key_but_allows_one_after_deactivation() {
        let mut keys = ApiKeys::default();
        let mut ids = Vec::new();
        for index in 0..MAX_ACTIVE_API_KEYS_PER_PROVIDER {
            ids.push(
                keys.add("openrouter", &format!("active-secret-{index}"), None)
                    .unwrap(),
            );
        }

        assert_eq!(
            keys.add("openrouter", "sixty-fifth-secret", None),
            Err(ApiKeyMutationError::ActiveLimitReached {
                limit: MAX_ACTIVE_API_KEYS_PER_PROVIDER
            })
        );
        keys.set_active("openrouter", ids[0], false).unwrap();
        let replacement = keys.add("openrouter", "replacement-active", None).unwrap();
        assert_eq!(
            keys.active_entries("openrouter").count(),
            MAX_ACTIVE_API_KEYS_PER_PROVIDER
        );
        assert_eq!(
            keys.set_active("openrouter", ids[0], true),
            Err(ApiKeyMutationError::ActiveLimitReached {
                limit: MAX_ACTIVE_API_KEYS_PER_PROVIDER
            })
        );
        keys.set_active("openrouter", replacement, false).unwrap();
        keys.set_active("openrouter", ids[0], true).unwrap();
        assert_eq!(
            keys.active_entries("openrouter").count(),
            MAX_ACTIVE_API_KEYS_PER_PROVIDER
        );
    }
}
