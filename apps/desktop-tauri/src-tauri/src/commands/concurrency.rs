use super::*;
use codexbar::concurrency_probe::{self, Verdict};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use tokio::sync::watch;

fn selected_credentials(settings: &Settings, keys: &ApiKeys) -> Vec<(String, uuid::Uuid)> {
    ["kimi", "minimax", "zai"]
        .into_iter()
        .filter(|id| settings.enabled_providers.contains(*id))
        .flat_map(|id| {
            keys.active_entries(id)
                .map(move |entry| (id.to_string(), entry.id))
        })
        .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeItem {
    provider_id: String,
    credential_id: String,
    label: String,
    endpoint: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbePreview {
    items: Vec<ProbeItem>,
    skipped: Vec<String>,
    confirmation_token: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRow {
    #[serde(flatten)]
    item: ProbeItem,
    model: String,
    status: String,
    checked_at: Option<String>,
    limited_at: Option<String>,
    #[serde(skip)]
    fingerprint: String,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeState {
    running: bool,
    run_id: u64,
    rows: Vec<ProbeRow>,
    warnings: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeOptions {
    models: HashMap<String, String>,
    credential_ids: Vec<String>,
    endpoints: HashMap<String, String>,
    confirmation_token: String,
}
#[derive(Default)]
struct Runtime {
    view: ProbeState,
    cancel: Option<watch::Sender<bool>>,
    confirmation: Option<(String, Vec<(String, String)>)>,
}
static RUNTIME: LazyLock<Mutex<Runtime>> = LazyLock::new(|| Mutex::new(Runtime::default()));

fn fingerprint(secret: &str, endpoint: &str, model: &str) -> String {
    let mut hash = Sha256::new();
    for part in [secret, endpoint, model] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn preview(settings: &Settings, keys: &ApiKeys) -> ProbePreview {
    let items = selected_credentials(settings, keys)
        .into_iter()
        .filter_map(|(id, uuid)| {
            let provider = ProviderId::from_cli_name(&id)?;
            let entry = keys.active_entries(&id).find(|entry| entry.id == uuid)?;
            Some(ProbeItem {
                endpoint: concurrency_probe::endpoint(&id, settings.api_region(provider))?.into(),
                provider_id: id,
                credential_id: uuid.to_string(),
                label: entry
                    .label
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("Key {}", entry.ordinal)),
            })
        })
        .collect::<Vec<_>>();
    let mut skipped = settings
        .enabled_providers
        .iter()
        .filter(|id| !items.iter().any(|item| &item.provider_id == *id))
        .cloned()
        .collect::<Vec<_>>();
    skipped.sort();
    ProbePreview {
        items,
        skipped,
        confirmation_token: String::new(),
    }
}

fn confirmation_signature(items: &[ProbeItem], keys: &ApiKeys) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|item| {
            keys.active_entries(&item.provider_id)
                .find(|entry| entry.id.to_string() == item.credential_id)
                .map(|entry| {
                    (
                        item.credential_id.clone(),
                        fingerprint(&entry.secret, &item.endpoint, ""),
                    )
                })
        })
        .collect()
}

#[tauri::command]
pub fn concurrency_preview(app: tauri::AppHandle) -> Result<ProbePreview, String> {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    let keys = load_api_keys_for_runtime().map_err(|_| "无法读取密钥存储".to_string())?;
    let mut preview = preview(&settings, &keys);
    preview.confirmation_token = uuid::Uuid::new_v4().to_string();
    RUNTIME
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .confirmation = Some((
        preview.confirmation_token.clone(),
        confirmation_signature(&preview.items, &keys),
    ));
    Ok(preview)
}

fn matches_current(row: &ProbeRow, settings: &Settings, keys: &ApiKeys) -> bool {
    if !settings.enabled_providers.contains(&row.item.provider_id) {
        return false;
    }
    let Some(provider) = ProviderId::from_cli_name(&row.item.provider_id) else {
        return false;
    };
    let Some(url) =
        concurrency_probe::endpoint(&row.item.provider_id, settings.api_region(provider))
    else {
        return false;
    };
    keys.active_entries(&row.item.provider_id).any(|entry| {
        entry.id.to_string() == row.item.credential_id
            && fingerprint(&entry.secret, url, &row.model) == row.fingerprint
    })
}

#[tauri::command]
pub fn concurrency_status(app: tauri::AppHandle) -> ProbeState {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    let keys = load_api_keys_for_runtime().ok();
    let mut runtime = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
    for row in &mut runtime.view.rows {
        if keys
            .as_ref()
            .is_some_and(|keys| !matches_current(row, &settings, keys))
        {
            row.status = "changed".into();
            row.limited_at = None;
        }
    }
    runtime.view.clone()
}

#[tauri::command]
pub fn concurrency_cancel() {
    let runtime = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(cancel) = &runtime.cancel {
        let _ = cancel.send(true);
    }
}

#[tauri::command]
pub fn concurrency_start(
    app: tauri::AppHandle,
    options: ProbeOptions,
) -> Result<ProbeState, String> {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    let keys = load_api_keys_for_runtime().map_err(|_| "无法读取密钥存储".to_string())?;
    let preview = preview(&settings, &keys);
    let signature = confirmation_signature(&preview.items, &keys);
    if preview.items.is_empty()
        || options.credential_ids.len() != preview.items.len()
        || options.credential_ids.iter().collect::<HashSet<_>>().len()
            != options.credential_ids.len()
        || preview.items.iter().any(|item| {
            !options.credential_ids.contains(&item.credential_id)
                || options.endpoints.get(&item.credential_id) != Some(&item.endpoint)
        })
    {
        return Err("检测范围已变化，请重新确认".into());
    }
    let mut rows = vec![];
    for item in preview.items {
        let model = options
            .models
            .get(&item.provider_id)
            .map(|s| s.trim())
            .filter(|s| concurrency_probe::valid_model(s))
            .ok_or("请填写有效的检测模型")?
            .to_string();
        let entry = keys
            .active_entries(&item.provider_id)
            .find(|entry| entry.id.to_string() == item.credential_id)
            .ok_or("Key 已变化")?;
        rows.push(ProbeRow {
            fingerprint: fingerprint(&entry.secret, &item.endpoint, &model),
            item,
            model,
            status: "pending".into(),
            checked_at: None,
            limited_at: None,
        });
    }
    let (sender, receiver) = watch::channel(false);
    let view = {
        let mut runtime = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
        if runtime.view.running {
            return Err("已有并发检测正在运行".into());
        }
        if runtime.confirmation.as_ref() != Some(&(options.confirmation_token, signature)) {
            return Err("确认后 Key 或检测范围已变化，请重新确认".into());
        }
        runtime.confirmation = None;
        for row in &mut rows {
            if let Some(previous) = runtime.view.rows.iter().find(|old| {
                old.item.credential_id == row.item.credential_id
                    && old.fingerprint == row.fingerprint
            }) {
                row.limited_at = previous.limited_at.clone();
            }
        }
        let warnings = preview
            .skipped
            .into_iter()
            .map(|id| format!("{id}：未适配或没有启用的 API Key，本次未检测"))
            .collect();
        runtime.view = ProbeState {
            running: true,
            run_id: runtime.view.run_id + 1,
            rows,
            warnings,
        };
        runtime.cancel = Some(sender);
        runtime.view.clone()
    };
    let work = view.rows.clone();
    tauri::async_runtime::spawn(async move {
        // Join failure also releases the global run guard; a panic must not wedge the button.
        let _ = tauri::async_runtime::spawn(run(app, work, receiver)).await;
        let mut runtime = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
        let cancelled = runtime
            .cancel
            .as_ref()
            .is_some_and(|sender| *sender.borrow());
        for row in &mut runtime.view.rows {
            if matches!(row.status.as_str(), "pending" | "running") {
                row.status = if cancelled { "cancelled" } else { "failed" }.into();
            }
        }
        runtime.view.running = false;
        runtime.cancel = None;
    });
    Ok(view)
}

fn update_result(id: &str, status: &str) {
    let mut runtime = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(row) = runtime
        .view
        .rows
        .iter_mut()
        .find(|row| row.item.credential_id == id)
    {
        // Once invalidated by a credential/config change, old work cannot restore the badge.
        if row.status == "changed" {
            return;
        }
        row.status = status.into();
        if !matches!(status, "pending" | "running") {
            row.checked_at = Some(chrono::Utc::now().to_rfc3339());
        }
        if status == "limited" {
            row.limited_at = row.checked_at.clone();
        }
        if matches!(status, "passed" | "changed") {
            row.limited_at = None;
        }
    }
}

async fn run(app: tauri::AppHandle, rows: Vec<ProbeRow>, mut cancel: watch::Receiver<bool>) {
    for row in rows {
        if *cancel.borrow() {
            break;
        }
        let settings = crate::storage_services::settings_snapshot_from_app(&app);
        let Ok(keys) = load_api_keys_for_runtime() else {
            update_result(&row.item.credential_id, "failed");
            continue;
        };
        if !matches_current(&row, &settings, &keys) {
            update_result(&row.item.credential_id, "changed");
            continue;
        }
        let Some(entry) = keys
            .active_entries(&row.item.provider_id)
            .find(|entry| entry.id.to_string() == row.item.credential_id)
        else {
            continue;
        };
        let provider =
            ProviderId::from_cli_name(&row.item.provider_id).expect("validated provider");
        update_result(&row.item.credential_id, "running");
        let verdict = tokio::select! {
            biased;
            _=cancel.changed()=>Verdict::Cancelled,
            value=concurrency_probe::probe(&row.item.provider_id,settings.api_region(provider),&row.model,&entry.secret)=>value,
        };
        let fresh_settings = crate::storage_services::settings_snapshot_from_app(&app);
        let status = match load_api_keys_for_runtime() {
            Err(_) => "failed".into(),
            Ok(keys) if !matches_current(&row, &fresh_settings, &keys) => "changed".into(),
            Ok(_) => serde_json::to_value(verdict)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or("failed".into()),
        };
        update_result(&row.item.credential_id, &status);
        if verdict == Verdict::Cancelled {
            break;
        }
        // Serial batches avoid our own cross-key pressure; remote release is not guaranteed.
        tokio::select! { _=cancel.changed()=>break, _=tokio::time::sleep(std::time::Duration::from_secs(2))=>{} }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_and_endpoint_changes_invalidate_evidence_and_fingerprint_is_private() {
        let mut settings = Settings::default();
        settings.enabled_providers = ["kimi"].into_iter().map(str::to_owned).collect();
        let mut keys = ApiKeys::default();
        keys.add("kimi", "fake-secret", None).unwrap();
        let initial_signature = confirmation_signature(&preview(&settings, &keys).items, &keys);
        let item = preview(&settings, &keys).items.remove(0);
        let row = ProbeRow {
            fingerprint: fingerprint("fake-secret", &item.endpoint, "kimi-for-coding"),
            item,
            model: "kimi-for-coding".into(),
            status: "limited".into(),
            checked_at: None,
            limited_at: Some("timestamp".into()),
        };
        assert!(matches_current(&row, &settings, &keys));
        let serialized = serde_json::to_string(&row).unwrap();
        assert!(!serialized.contains("fake-secret"));
        assert!(!serialized.contains(&row.fingerprint));
        assert_ne!(
            fingerprint("fake-secret", "other-endpoint", &row.model),
            row.fingerprint
        );
        assert_ne!(
            fingerprint("fake-secret", &row.item.endpoint, "other-model"),
            row.fingerprint
        );
        keys.providers.get_mut("kimi").unwrap().entries[0].secret = "replacement".into();
        assert!(!matches_current(&row, &settings, &keys));
        assert_ne!(
            initial_signature,
            confirmation_signature(&preview(&settings, &keys).items, &keys)
        );
    }
    #[test]
    fn batch_includes_every_active_key_only_for_checked_supported_providers() {
        let mut settings = Settings::default();
        settings.enabled_providers = ["kimi", "minimax", "codex"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut keys = ApiKeys::default();
        let a = keys.add("kimi", "fake-a", None).unwrap();
        let b = keys.add("kimi", "fake-b", None).unwrap();
        let c = keys.add("minimax", "fake-c", None).unwrap();
        keys.add("zai", "fake-d", None).unwrap();
        keys.add("openrouter", "fake-e", None).unwrap();
        assert_eq!(
            selected_credentials(&settings, &keys),
            vec![
                ("kimi".into(), a),
                ("kimi".into(), b),
                ("minimax".into(), c)
            ]
        );
        keys.providers.get_mut("kimi").unwrap().entries[1].active = false;
        assert_eq!(selected_credentials(&settings, &keys).len(), 2);
    }
}
