mod quota_window;
mod window;

pub fn install(app: &tauri::AppHandle, enabled: bool) {
    apply_enabled(app, enabled);
}

pub fn apply_enabled(app: &tauri::AppHandle, enabled: bool) {
    if !enabled && let Err(error) = quota_window::close(app) {
        tracing::warn!(
            target: "codexbar::floating_ball",
            %error,
            "floating quota close failed"
        );
    }

    let result = match window::desired_action(enabled) {
        window::WindowAction::Show => window::show(app),
        window::WindowAction::Close => window::close(app),
    };
    if let Err(error) = result {
        tracing::warn!(
            target: "codexbar::floating_ball",
            %error,
            enabled,
            "floating ball state update failed"
        );
    }
}

#[tauri::command]
pub async fn toggle_floating_quota(app: tauri::AppHandle) -> Result<bool, String> {
    quota_window::toggle(&app)
}

#[tauri::command]
pub fn hide_floating_quota(app: tauri::AppHandle) -> Result<(), String> {
    quota_window::hide(&app)
}

#[tauri::command]
pub fn resize_floating_quota(app: tauri::AppHandle, height: f64) -> Result<(), String> {
    quota_window::resize(&app, height)
}

#[tauri::command]
pub fn start_floating_quota_drag(app: tauri::AppHandle) -> Result<(), String> {
    quota_window::start_drag(&app)
}

#[tauri::command]
pub fn toggle_floating_quota_topmost(app: tauri::AppHandle) -> Result<String, String> {
    quota_window::toggle_topmost(&app)
}

#[tauri::command]
pub fn toggle_floating_quota_desktop(app: tauri::AppHandle) -> Result<String, String> {
    quota_window::toggle_desktop(&app)
}

#[tauri::command]
pub fn open_main_window_from_floating_quota(app: tauri::AppHandle) -> Result<(), String> {
    quota_window::open_main(&app)
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use super::*;

    fn assert_async_toggle<F, Fut>(command: F)
    where
        F: FnOnce(tauri::AppHandle) -> Fut,
        Fut: Future<Output = Result<bool, String>>,
    {
        let _ = command;
    }

    #[test]
    fn window_creating_toggle_command_is_async() {
        assert_async_toggle(toggle_floating_quota);
    }
}
