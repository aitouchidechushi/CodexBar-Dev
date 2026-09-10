use std::path::PathBuf;

use tauri::{Manager, PhysicalPosition, Url, WebviewUrl, webview::NewWindowResponse};

pub const LABEL: &str = "kimi-login";
const LOGIN_WINDOW_ZOOM: f64 = 0.9;
const LOGIN_WINDOW_RATIO: f64 = 0.9;
const LOGIN_WINDOW_MAX_WIDTH: f64 = 1200.0;
const LOGIN_WINDOW_MAX_HEIGHT: f64 = 900.0;
const LOGIN_WINDOW_MARGIN: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoginWindowGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn login_window_geometry(
    work_x: f64,
    work_y: f64,
    work_width: f64,
    work_height: f64,
) -> LoginWindowGeometry {
    let available_width = (work_width - LOGIN_WINDOW_MARGIN * 2.0).max(1.0);
    let available_height = (work_height - LOGIN_WINDOW_MARGIN * 2.0).max(1.0);
    let width = (work_width * LOGIN_WINDOW_RATIO)
        .min(LOGIN_WINDOW_MAX_WIDTH)
        .min(available_width);
    let height = (work_height * LOGIN_WINDOW_RATIO)
        .min(LOGIN_WINDOW_MAX_HEIGHT)
        .min(available_height);

    LoginWindowGeometry {
        x: work_x + (work_width - width) / 2.0,
        y: work_y + (work_height - height) / 2.0,
        width,
        height,
    }
}

pub fn open_or_focus(
    app: &tauri::AppHandle,
    data_directory: PathBuf,
) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show().map_err(|error| error.to_string())?;
        let _ = window.unminimize();
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(window);
    }

    let monitor = app
        .get_webview_window("settings")
        .and_then(|window| window.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())
        .ok_or_else(|| "无法确定 Kimi 登录窗口所在的显示器".to_string())?;
    let scale = monitor.scale_factor();
    let work_area = monitor.work_area();
    let geometry = login_window_geometry(
        work_area.position.x as f64 / scale,
        work_area.position.y as f64 / scale,
        work_area.size.width as f64 / scale,
        work_area.size.height as f64 / scale,
    );
    let url =
        Url::parse(crate::kimi_accounts::KIMI_LOGIN_URL).map_err(|error| error.to_string())?;
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(url))
        .title("Kimi 登录 - CodexBar")
        .inner_size(geometry.width, geometry.height)
        .position(geometry.x, geometry.y)
        .data_directory(data_directory)
        .on_navigation(crate::kimi_accounts::is_allowed_kimi_navigation)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .decorations(true)
        .resizable(true)
        .visible(true)
        .skip_taskbar(false)
        .build()
        .map_err(|error| error.to_string())?;

    let _ = window.set_position(PhysicalPosition::new(
        (geometry.x * scale).round() as i32,
        (geometry.y * scale).round() as i32,
    ));
    let _ = window.with_webview(|webview| unsafe {
        let _ = webview.controller().SetZoomFactor(LOGIN_WINDOW_ZOOM);
    });
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(window)
}

pub fn focus(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "Kimi 登录窗口不存在".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    let _ = window.unminimize();
    window.set_focus().map_err(|error| error.to_string())
}

pub fn close(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_window_uses_ninety_percent_with_caps_and_centers_in_work_area() {
        let geometry = login_window_geometry(0.0, 0.0, 1920.0, 1040.0);

        assert_eq!(geometry.width, 1200.0);
        assert_eq!(geometry.height, 900.0);
        assert_eq!(geometry.x, 360.0);
        assert_eq!(geometry.y, 70.0);
    }

    #[test]
    fn login_window_keeps_at_least_twenty_four_pixels_from_small_work_area_edges() {
        let geometry = login_window_geometry(100.0, 50.0, 400.0, 300.0);

        assert_eq!(geometry.width, 352.0);
        assert_eq!(geometry.height, 252.0);
        assert_eq!(geometry.x, 124.0);
        assert_eq!(geometry.y, 74.0);
    }
}
