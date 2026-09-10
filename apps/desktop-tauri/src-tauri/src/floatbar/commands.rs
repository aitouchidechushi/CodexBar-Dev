//! Tauri commands that drive the floating-bar window.
//!
//! These are thin wrappers around the local `window` module. User-initiated
//! changes keep persisted settings in sync, while the resize command applies a
//! new size and the native interaction state together.

use codexbar::settings::{clamp_float_bar_opacity, normalize_float_bar_orientation};
use tauri::{AppHandle, Manager};

use super::window as floatbar_window;

#[tauri::command]
pub async fn show_float_bar(app: AppHandle) -> Result<(), String> {
    let (_, settings) = crate::storage_services::mutate_settings_from_app(&app, |settings| {
        settings.float_bar_enabled = true;
        Ok(())
    })
    .map_err(crate::storage_services::command_store_error)?;

    floatbar_window::show(
        &app,
        settings.float_bar_opacity,
        &settings.float_bar_orientation,
        &settings.float_bar_style,
        settings.float_bar_click_through,
    )
}

fn complete_initial_show_with<Show, Destroy, Rollback, Rebuild>(
    already_visible: bool,
    show: Show,
    destroy: Destroy,
    rollback: Rollback,
    rebuild_tray: Rebuild,
) -> Result<(), String>
where
    Show: FnOnce() -> Result<(), String>,
    Destroy: FnOnce() -> Result<(), String>,
    Rollback: FnOnce() -> Result<(), String>,
    Rebuild: FnOnce(),
{
    if already_visible {
        return Ok(());
    }

    match show() {
        Ok(()) => Ok(()),
        Err(original_error) => {
            // The hidden WebView must be removed before any persistence work:
            // even a failed settings rollback must not strand an invisible
            // FloatBar object that cannot complete its first show again.
            let _ = destroy();
            let _ = rollback();
            rebuild_tray();
            Err(original_error)
        }
    }
}

fn initial_visibility_with(query: impl FnOnce() -> Result<bool, String>) -> bool {
    match query() {
        Ok(visible) => visible,
        Err(error) => {
            tracing::warn!(%error, "FloatBar visibility query failed before initial show; attempting show");
            false
        }
    }
}

/// Complete only the hidden window created by the enable/startup path after
/// the webview has measured and resized its first frame. This command never
/// writes the enabled setting on success; the user action that created the
/// hidden window already owns that persistence step.
#[tauri::command]
pub fn complete_float_bar_initial_show(app: AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) else {
        return Ok(());
    };
    let already_visible =
        initial_visibility_with(|| window.is_visible().map_err(|error| error.to_string()));

    complete_initial_show_with(
        already_visible,
        || {
            floatbar_window::apply_no_activate(&window);
            floatbar_window::apply_always_on_top(&window);
            window.show().map_err(|error| error.to_string())?;
            floatbar_window::apply_always_on_top(&window);
            super::topmost_guard::set_active(true);
            Ok(())
        },
        || {
            super::topmost_guard::set_active(false);
            window
                .destroy()
                .or_else(|_| window.close())
                .map_err(|error| error.to_string())
        },
        || {
            crate::storage_services::mutate_settings_from_app(&app, |settings| {
                settings.float_bar_enabled = false;
                Ok(())
            })
            .map(|_| ())
            .map_err(crate::storage_services::command_store_error)
        },
        || crate::tray_bridge::rebuild_tray_menu(&app),
    )
}

#[tauri::command]
pub fn hide_float_bar(app: AppHandle) -> Result<(), String> {
    crate::storage_services::mutate_settings_from_app(&app, |settings| {
        settings.float_bar_enabled = false;
        Ok(())
    })
    .map_err(crate::storage_services::command_store_error)?;
    floatbar_window::hide(&app)
}

#[tauri::command]
pub fn set_float_bar_opacity(app: AppHandle, opacity: u8) -> Result<(), String> {
    let opacity = clamp_float_bar_opacity(opacity);
    crate::storage_services::mutate_settings_from_app(&app, |settings| {
        settings.float_bar_opacity = opacity;
        Ok(())
    })
    .map_err(crate::storage_services::command_store_error)?;

    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        floatbar_window::apply_no_activate(&window);
        floatbar_window::apply_opacity(&window, opacity);
        floatbar_window::apply_always_on_top(&window);
    }
    Ok(())
}

#[tauri::command]
pub fn set_float_bar_click_through(app: AppHandle, enabled: bool) -> Result<(), String> {
    crate::storage_services::mutate_settings_from_app(&app, |settings| {
        settings.float_bar_click_through = enabled;
        Ok(())
    })
    .map_err(crate::storage_services::command_store_error)?;

    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        floatbar_window::apply_no_activate(&window);
        floatbar_window::apply_click_through(&window, enabled);
        floatbar_window::apply_always_on_top(&window);
    }
    Ok(())
}

#[tauri::command]
pub fn resize_float_bar(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        // One native operation owns the resize + interaction-state invariant,
        // so the webview never has to repair Win32 window styles itself.
        floatbar_window::resize(&window, width, height, settings.float_bar_click_through)?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_float_bar_orientation(app: AppHandle, orientation: String) -> Result<(), String> {
    let orientation = normalize_float_bar_orientation(&orientation);
    crate::storage_services::mutate_settings_from_app(&app, |settings| {
        settings.float_bar_orientation = orientation;
        Ok(())
    })
    .map_err(crate::storage_services::command_store_error)?;

    // The webview re-reads orientation from settings on every config
    // change — emit the live-update event so it re-lays-out without us
    // needing to destroy/recreate the window (which races with Tauri's
    // async window lifecycle and can crash on Windows).
    use tauri::Emitter;
    let _ = app.emit(super::FLOAT_BAR_CONFIG_CHANGED_EVENT, ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn initial_visibility_query_failure_falls_through_to_show_attempt() {
        assert!(!initial_visibility_with(|| Err(
            "visibility query failed".to_string()
        )));
    }

    #[test]
    fn initial_completion_is_idempotent_when_window_is_already_visible() {
        let calls = RefCell::new(Vec::new());

        let result = complete_initial_show_with(
            true,
            || {
                calls.borrow_mut().push("show");
                Ok(())
            },
            || {
                calls.borrow_mut().push("destroy");
                Ok(())
            },
            || {
                calls.borrow_mut().push("rollback");
                Ok(())
            },
            || calls.borrow_mut().push("tray"),
        );

        assert_eq!(result, Ok(()));
        assert!(calls.into_inner().is_empty());
    }

    #[test]
    fn initial_completion_success_only_shows_the_hidden_window() {
        let calls = RefCell::new(Vec::new());

        let result = complete_initial_show_with(
            false,
            || {
                calls.borrow_mut().push("show");
                Ok(())
            },
            || {
                calls.borrow_mut().push("destroy");
                Ok(())
            },
            || {
                calls.borrow_mut().push("rollback");
                Ok(())
            },
            || calls.borrow_mut().push("tray"),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(calls.into_inner(), vec!["show"]);
    }

    #[test]
    fn initial_show_failure_destroys_before_best_effort_rollback_and_returns_original_error() {
        let calls = RefCell::new(Vec::new());

        let result = complete_initial_show_with(
            false,
            || {
                calls.borrow_mut().push("show");
                Err("original show error".to_string())
            },
            || {
                calls.borrow_mut().push("destroy");
                Err("destroy error".to_string())
            },
            || {
                calls.borrow_mut().push("rollback");
                Err("save error".to_string())
            },
            || calls.borrow_mut().push("tray"),
        );

        assert_eq!(result, Err("original show error".to_string()));
        assert_eq!(
            calls.into_inner(),
            vec!["show", "destroy", "rollback", "tray"]
        );
    }
}
