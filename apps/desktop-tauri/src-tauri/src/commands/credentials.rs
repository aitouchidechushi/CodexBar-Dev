use super::*;

// ── Credential store commands ─────────────────────────────────────────

/// Bridge-friendly API key info. This type intentionally contains no secret-derived field.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyInfoBridge {
    pub credential_id: String,
    pub provider_id: String,
    pub provider: String,
    pub label: String,
    pub custom_label: Option<String>,
    pub display_ordinal: u64,
    pub saved_at: String,
    pub active: bool,
    pub inactive_reason: Option<String>,
}

impl From<codexbar::settings::SavedApiKeyInfo> for ApiKeyInfoBridge {
    fn from(info: codexbar::settings::SavedApiKeyInfo) -> Self {
        Self {
            credential_id: info.credential_id.to_string(),
            provider_id: info.provider_id,
            provider: info.provider,
            label: info.label,
            custom_label: info.custom_label,
            display_ordinal: info.display_ordinal,
            saved_at: info.saved_at,
            active: info.active,
            inactive_reason: info.inactive_reason.map(|reason| match reason {
                codexbar::settings::ApiKeyInactiveReason::OverLimitMigration => {
                    "over-limit-migration".to_string()
                }
                codexbar::settings::ApiKeyInactiveReason::UserDisabled => {
                    "user-disabled".to_string()
                }
            }),
        }
    }
}

/// Bridge-friendly saved cookie info.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieInfoBridge {
    pub provider_id: String,
    pub provider: String,
    pub saved_at: String,
}

/// Bridge-friendly provider config info for the API keys tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyProviderInfoBridge {
    pub id: String,
    pub display_name: String,
    pub env_var: Option<String>,
    pub help: Option<String>,
    pub dashboard_url: Option<String>,
}

/// App metadata for the About tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfoBridge {
    pub name: String,
    pub version: String,
    pub build_number: String,
    pub update_channel: String,
    pub tagline: String,
}

#[tauri::command]
pub fn get_api_keys() -> Result<Vec<ApiKeyInfoBridge>, String> {
    Ok(api_key_bridges(
        load_api_keys_for_runtime()?.get_all_for_display(),
    ))
}

pub(crate) fn selected_api_key_secret(
    keys: &ApiKeys,
    provider_id: &str,
    credential_id: uuid::Uuid,
) -> Result<String, String> {
    keys.entries(provider_id)
        .iter()
        .find(|entry| entry.id == credential_id)
        .map(|entry| entry.secret.clone())
        .ok_or_else(|| "API key credential was not found".to_string())
}

#[tauri::command]
pub fn get_api_key_secret(
    window: tauri::WebviewWindow,
    provider_id: String,
    credential_id: String,
) -> Result<String, String> {
    if window.label() != "main" {
        return Err("API key secrets are available only from the homepage".to_string());
    }
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    let credential_id = parse_credential_id(&credential_id)?;
    selected_api_key_secret(
        &load_api_keys_for_runtime()?,
        &canonical_provider,
        credential_id,
    )
}

fn api_key_bridges(entries: Vec<codexbar::settings::SavedApiKeyInfo>) -> Vec<ApiKeyInfoBridge> {
    entries.into_iter().map(ApiKeyInfoBridge::from).collect()
}

#[tauri::command]
pub fn get_api_key_providers() -> Vec<ApiKeyProviderInfoBridge> {
    codexbar::settings::get_app_managed_api_key_providers()
        .into_iter()
        .map(|p| ApiKeyProviderInfoBridge {
            id: p.id.cli_name().to_string(),
            display_name: p.name.to_string(),
            env_var: p.api_key_env_var.map(|s| s.to_string()),
            help: p.api_key_help.map(|s| s.to_string()),
            dashboard_url: p.dashboard_url.map(|s| s.to_string()),
        })
        .collect()
}

#[tauri::command]
pub fn set_api_key(
    app: tauri::AppHandle,
    provider_id: String,
    api_key: String,
    label: Option<String>,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    validate_single_line_secret(&api_key, "API key", MAX_API_KEY_LEN)?;
    let label = sanitize_optional_label(label)?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        if let Some(credential_id) = keys.entries(provider_id).first().map(|entry| entry.id) {
            keys.replace_secret(provider_id, credential_id, api_key.trim())?;
            keys.update_label(provider_id, credential_id, label.as_deref());
            Ok(CredentialCacheMutation::SecretReplaced(credential_id))
        } else {
            let credential_id = keys.add(provider_id, api_key.trim(), label.as_deref())?;
            Ok(CredentialCacheMutation::Added(credential_id))
        }
    })
}

#[tauri::command]
pub fn remove_api_key(
    app: tauri::AppHandle,
    provider_id: String,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        keys.remove(provider_id);
        Ok(CredentialCacheMutation::ProviderRevoked)
    })
}

#[tauri::command]
pub fn add_api_key(
    app: tauri::AppHandle,
    provider_id: String,
    api_key: String,
    label: Option<String>,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    validate_single_line_secret(&api_key, "API key", MAX_API_KEY_LEN)?;
    let label = sanitize_optional_label(label)?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        keys.add(provider_id, api_key.trim(), label.as_deref())
            .map(CredentialCacheMutation::Added)
    })
}

#[tauri::command]
pub fn update_api_key_label(
    app: tauri::AppHandle,
    provider_id: String,
    credential_id: String,
    label: Option<String>,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    let credential_id = parse_credential_id(&credential_id)?;
    let label = sanitize_optional_label(label)?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        if !keys.update_label(provider_id, credential_id, label.as_deref()) {
            return Err(ApiKeyMutationError::NotFound);
        }
        Ok(CredentialCacheMutation::Relabeled(credential_id))
    })
}

#[tauri::command]
pub fn replace_api_key_secret(
    app: tauri::AppHandle,
    provider_id: String,
    credential_id: String,
    api_key: String,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    let credential_id = parse_credential_id(&credential_id)?;
    validate_single_line_secret(&api_key, "API key", MAX_API_KEY_LEN)?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        keys.replace_secret(provider_id, credential_id, api_key.trim())
            .map(|()| CredentialCacheMutation::SecretReplaced(credential_id))
    })
}

#[tauri::command]
pub fn delete_api_key(
    app: tauri::AppHandle,
    provider_id: String,
    credential_id: String,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    let credential_id = parse_credential_id(&credential_id)?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        if !keys.delete(provider_id, credential_id) {
            return Err(ApiKeyMutationError::NotFound);
        }
        Ok(CredentialCacheMutation::Deleted(credential_id))
    })
}

#[tauri::command]
pub fn reorder_api_keys(
    app: tauri::AppHandle,
    provider_id: String,
    credential_ids: Vec<String>,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let canonical_provider = api_key_provider_arg(&provider_id)?;
    let credential_ids = credential_ids
        .iter()
        .map(|credential_id| parse_credential_id(credential_id))
        .collect::<Result<Vec<_>, _>>()?;

    mutate_api_keys(&app, canonical_provider, |keys, provider_id| {
        keys.reorder(provider_id, &credential_ids)
            .map(|()| CredentialCacheMutation::Reordered)
    })
}

pub(crate) fn api_key_provider_arg(provider_id: &str) -> Result<String, String> {
    let canonical_provider = canonical_provider_arg(provider_id)?;
    if ProviderId::from_cli_name(&canonical_provider)
        .is_some_and(codexbar::settings::supports_app_managed_api_key)
    {
        Ok(canonical_provider)
    } else {
        Err(format!(
            "Provider '{canonical_provider}' does not support API-key storage"
        ))
    }
}

pub(crate) fn parse_credential_id(credential_id: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(credential_id.trim())
        .map_err(|_| "API key credential id is invalid".to_string())
}

fn mutate_api_keys(
    app: &tauri::AppHandle,
    provider_id: String,
    mutation: impl FnOnce(&mut ApiKeys, &str) -> Result<CredentialCacheMutation, ApiKeyMutationError>,
) -> Result<Vec<ApiKeyInfoBridge>, String> {
    let (mutation, api_keys, display) = ApiKeyRepository::discover()
        .map_err(|error| error.to_string())?
        .mutate(|keys| {
            let mutation = mutation(keys, &provider_id)?;
            Ok((mutation, keys.clone(), keys.get_all_for_display()))
        })
        .map_err(|error| error.to_string())?;

    finish_credential_mutation(app, &provider_id, &api_keys, mutation)?;
    Ok(api_key_bridges(display))
}

pub(crate) fn finish_credential_mutation(
    app: &tauri::AppHandle,
    provider_id: &str,
    api_keys: &ApiKeys,
    mutation: CredentialCacheMutation,
) -> Result<(), String> {
    let enabled_ids =
        crate::storage_services::settings_snapshot_from_app(app).get_enabled_provider_ids();
    let should_refresh = {
        let state = app.state::<Mutex<AppState>>();
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        apply_credential_mutation_result(
            &mut guard,
            &enabled_ids,
            api_keys,
            provider_id,
            Ok(mutation),
        )?
    };

    if provider_id == "kimi" {
        match mutation {
            CredentialCacheMutation::Added(credential_id)
            | CredentialCacheMutation::SecretReplaced(credential_id)
            | CredentialCacheMutation::Deleted(credential_id) => {
                crate::kimi_accounts::invalidate_credential_matches(app, Some(credential_id));
            }
            CredentialCacheMutation::ProviderRevoked => {
                crate::kimi_accounts::invalidate_credential_matches(app, None);
            }
            CredentialCacheMutation::Relabeled(_) | CredentialCacheMutation::Reordered => {}
        }
    }

    events::emit_settings_changed(app);
    if should_refresh {
        let app = app.clone();
        let refresh_provider_id = provider_id.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = do_refresh_providers(&app).await {
                tracing::warn!(
                    provider_id = %refresh_provider_id,
                    error = %error,
                    "forced provider refresh after credential mutation failed"
                );
            }
        });
    }
    Ok(())
}

#[tauri::command]
pub fn get_manual_cookies(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<Vec<CookieInfoBridge>, String> {
    let repository = crate::storage_services::storage_snapshot(&state).manual_cookies;
    let cookies = repository
        .load()
        .map_err(crate::storage_services::command_store_error)?
        .into_writable_for(StoreKind::ManualCookies)
        .map_err(crate::storage_services::command_store_error)?;
    Ok(manual_cookie_bridges(cookies))
}

pub(crate) fn manual_cookie_bridges(cookies: ManualCookies) -> Vec<CookieInfoBridge> {
    cookies
        .get_all_for_display()
        .into_iter()
        .map(|info| CookieInfoBridge {
            provider_id: info.provider_id,
            provider: info.provider,
            saved_at: info.saved_at,
        })
        .collect()
}

#[tauri::command]
pub fn set_manual_cookie(
    state: tauri::State<'_, Mutex<AppState>>,
    provider_id: String,
    cookie_header: String,
) -> Result<Vec<CookieInfoBridge>, String> {
    let id = validate_manual_cookie_input(&provider_id, &cookie_header)?;

    let repository = crate::storage_services::storage_snapshot(&state).manual_cookies;
    let cookies = repository
        .mutate(|cookies| {
            cookies.set(id.cli_name(), cookie_header.trim());
            cookies.clone()
        })
        .map_err(crate::storage_services::command_store_error)?;
    Ok(manual_cookie_bridges(cookies))
}

pub(crate) fn validate_manual_cookie_input(
    provider_id: &str,
    cookie_header: &str,
) -> Result<ProviderId, String> {
    let id = parse_provider_arg(provider_id)?;
    if id.cookie_domain().is_none() {
        return Err(format!(
            "Provider '{}' does not support manual cookie storage",
            id.cli_name()
        ));
    }
    validate_single_line_secret(cookie_header, "Cookie header", MAX_COOKIE_HEADER_LEN)?;
    Ok(id)
}

#[tauri::command]
pub fn remove_manual_cookie(
    state: tauri::State<'_, Mutex<AppState>>,
    provider_id: String,
) -> Result<Vec<CookieInfoBridge>, String> {
    let canonical_provider = canonical_provider_arg(&provider_id)?;
    let repository = crate::storage_services::storage_snapshot(&state).manual_cookies;
    let cookies = repository
        .mutate(|cookies| {
            cookies.remove(&canonical_provider);
            cookies.clone()
        })
        .map_err(crate::storage_services::command_store_error)?;
    Ok(manual_cookie_bridges(cookies))
}
