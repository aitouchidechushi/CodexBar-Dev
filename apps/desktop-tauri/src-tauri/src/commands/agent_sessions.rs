use codexbar::agent_sessions::{
    AgentSession, AgentSessionDiscovery, AgentSessionDiscoveryMode, AgentSessionDiscoveryResult,
    SessionFocusResult, focus_session,
};
#[tauri::command]
pub async fn list_agent_sessions(app: tauri::AppHandle) -> AgentSessionDiscoveryResult {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    let mode = if settings.agent_sessions_enabled {
        AgentSessionDiscoveryMode::Enabled {
            ssh_hosts: settings.agent_session_ssh_hosts,
        }
    } else {
        AgentSessionDiscoveryMode::Disabled
    };
    AgentSessionDiscovery::default().scan(mode).await
}

#[tauri::command]
pub fn focus_agent_session(session: AgentSession) -> SessionFocusResult {
    focus_session(&session)
}
