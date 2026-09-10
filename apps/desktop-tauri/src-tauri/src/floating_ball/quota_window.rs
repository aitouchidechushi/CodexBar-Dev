use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewUrl};

use crate::state::AppState;
use crate::surface::SurfaceMode;
use crate::surface_target::SurfaceTarget;
use crate::window_positioner::{PanelSize, Rect, calculate_sidecar_position};

use super::window::LABEL as BALL_LABEL;

pub const LABEL: &str = "floating-quota";
const WIDTH: f64 = 320.0;
const DEFAULT_HEIGHT: f64 = 180.0;
const MIN_HEIGHT: f64 = 80.0;
const MAX_HEIGHT: f64 = 520.0;
static MANUALLY_POSITIONED: AtomicBool = AtomicBool::new(false);
static WINDOW_MODE: AtomicU8 = AtomicU8::new(FloatingQuotaMode::Normal as u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToggleAction {
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositioningAction {
    AnchorToBall,
    KeepCurrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FloatingQuotaMode {
    Normal = 0,
    Topmost = 1,
    Desktop = 2,
}

impl FloatingQuotaMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Topmost,
            2 => Self::Desktop,
            _ => Self::Normal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Topmost => "topmost",
            Self::Desktop => "desktop",
        }
    }
}

fn desired_toggle_action(visible: bool) -> ToggleAction {
    if visible {
        ToggleAction::Hide
    } else {
        ToggleAction::Show
    }
}

fn positioning_action(manually_positioned: bool) -> PositioningAction {
    if manually_positioned {
        PositioningAction::KeepCurrent
    } else {
        PositioningAction::AnchorToBall
    }
}

fn current_window_mode() -> FloatingQuotaMode {
    FloatingQuotaMode::from_u8(WINDOW_MODE.load(Ordering::Relaxed))
}

fn mode_after_click(current: FloatingQuotaMode, clicked: FloatingQuotaMode) -> FloatingQuotaMode {
    if current == clicked {
        FloatingQuotaMode::Normal
    } else {
        clicked
    }
}

fn should_open_dashboard(mode: SurfaceMode) -> bool {
    mode == SurfaceMode::Hidden
}

fn apply_window_mode(window: &tauri::WebviewWindow, mode: FloatingQuotaMode) -> Result<(), String> {
    set_desktop_style(window, mode == FloatingQuotaMode::Desktop)?;
    window
        .set_always_on_top(mode == FloatingQuotaMode::Topmost)
        .map_err(|error| error.to_string())?;
    if mode == FloatingQuotaMode::Desktop {
        place_at_desktop_level(window)?;
    }
    Ok(())
}

#[cfg(windows)]
fn window_handle(window: &tauri::WebviewWindow) -> Result<isize, String> {
    use raw_window_handle::HasWindowHandle;

    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("floating quota window handle is unavailable".to_string());
    };
    Ok(handle.hwnd.get())
}

#[cfg(windows)]
fn set_desktop_style(window: &tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    let hwnd = window_handle(window)?;
    unsafe {
        const GWL_EXSTYLE: i32 = -20;
        const WS_EX_NOACTIVATE: isize = 0x08000000;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;
        const SWP_FRAMECHANGED: u32 = 0x0020;

        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let next = if enabled {
            current | WS_EX_NOACTIVATE
        } else {
            current & !WS_EX_NOACTIVATE
        };
        if next != current {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
            if SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_desktop_style(_window: &tauri::WebviewWindow, _enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn place_at_desktop_level(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window_handle(window)?;
    unsafe {
        const HWND_BOTTOM: isize = 1;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOACTIVATE: u32 = 0x0010;
        if SetWindowPos(
            hwnd,
            HWND_BOTTOM,
            0,
            0,
            0,
            0,
            SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn place_at_desktop_level(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, new: isize) -> isize;
    fn SetWindowPos(
        hwnd: isize,
        hwnd_insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
}

fn position_if_needed(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    if positioning_action(MANUALLY_POSITIONED.load(Ordering::Relaxed))
        == PositioningAction::AnchorToBall
    {
        position_window(app, window)?;
    }
    Ok(())
}

fn position_window(
    app: &tauri::AppHandle,
    quota_window: &tauri::WebviewWindow,
) -> Result<(), String> {
    let ball_window = app
        .get_webview_window(BALL_LABEL)
        .ok_or_else(|| "floating ball window is unavailable".to_string())?;
    let ball_position = ball_window
        .outer_position()
        .map_err(|error| error.to_string())?;
    let ball_size = ball_window
        .outer_size()
        .map_err(|error| error.to_string())?;
    let ball_center_x = ball_position.x + ball_size.width as i32 / 2;
    let ball_center_y = ball_position.y + ball_size.height as i32 / 2;
    let monitors = ball_window
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let monitor = monitors
        .iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            ball_center_x >= position.x
                && ball_center_x < position.x + size.width as i32
                && ball_center_y >= position.y
                && ball_center_y < position.y + size.height as i32
        })
        .or_else(|| monitors.first())
        .ok_or_else(|| "no monitor available for floating quota".to_string())?;
    let work_area = monitor.work_area();
    let work_rect = Rect {
        x: work_area.position.x,
        y: work_area.position.y,
        width: work_area.size.width,
        height: work_area.size.height,
    };
    let ball_rect = Rect {
        x: ball_position.x,
        y: ball_position.y,
        width: ball_size.width,
        height: ball_size.height,
    };
    let current_size = quota_window
        .inner_size()
        .map_err(|error| error.to_string())?;
    let current_scale = quota_window
        .scale_factor()
        .map_err(|error| error.to_string())?;
    let logical_height =
        ((current_size.height as f64 / current_scale).round()).clamp(MIN_HEIGHT, MAX_HEIGHT) as u32;
    let panel_size = PanelSize {
        width: WIDTH as u32,
        height: logical_height,
    };
    let (x, y) =
        calculate_sidecar_position(&ball_rect, &work_rect, &panel_size, monitor.scale_factor());

    quota_window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}

fn get_or_create(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }

    let builder =
        tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("floating-quota.html".into()))
            .title("CodexBar")
            .inner_size(WIDTH, DEFAULT_HEIGHT)
            .resizable(false)
            .decorations(false)
            .always_on_top(false)
            .skip_taskbar(true)
            .focusable(true)
            .visible(false);
    #[cfg(windows)]
    let builder = builder.transparent(true);

    builder
        .background_color(tauri::utils::config::Color(0, 0, 0, 0))
        .build()
        .map_err(|error| error.to_string())
}

fn show(app: &tauri::AppHandle) -> Result<(), String> {
    let window = get_or_create(app)?;
    position_if_needed(app, &window)?;
    window.show().map_err(|error| error.to_string())?;
    let mode = current_window_mode();
    if mode != FloatingQuotaMode::Desktop {
        window.set_focus().map_err(|error| error.to_string())?;
    }
    apply_window_mode(&window, mode)
}

pub(super) fn toggle(app: &tauri::AppHandle) -> Result<bool, String> {
    let visible = app
        .get_webview_window(LABEL)
        .map(|window| window.is_visible().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(false);

    match desired_toggle_action(visible) {
        ToggleAction::Show => {
            show(app)?;
            Ok(true)
        }
        ToggleAction::Hide => {
            hide(app)?;
            Ok(false)
        }
    }
}

pub(super) fn hide(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn resize(app: &tauri::AppHandle, height: f64) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT);
    window
        .set_size(LogicalSize::new(WIDTH, height))
        .map_err(|error| error.to_string())?;
    position_if_needed(app, &window)?;
    apply_window_mode(&window, current_window_mode())
}

pub(super) fn start_drag(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "floating quota window is unavailable".to_string())?;
    MANUALLY_POSITIONED.store(true, Ordering::Relaxed);
    window.start_dragging().map_err(|error| error.to_string())?;
    apply_window_mode(&window, current_window_mode())
}

fn toggle_window_mode(
    app: &tauri::AppHandle,
    clicked: FloatingQuotaMode,
) -> Result<String, String> {
    let window = app
        .get_webview_window(LABEL)
        .ok_or_else(|| "floating quota window is unavailable".to_string())?;
    let current = current_window_mode();
    let next = mode_after_click(current, clicked);
    if let Err(error) = apply_window_mode(&window, next) {
        if let Err(rollback_error) = apply_window_mode(&window, current) {
            tracing::warn!(
                target: "codexbar::floating_quota",
                %rollback_error,
                "failed to restore floating quota window mode"
            );
        }
        return Err(error);
    }
    WINDOW_MODE.store(next as u8, Ordering::Relaxed);
    Ok(next.as_str().to_string())
}

pub(super) fn toggle_topmost(app: &tauri::AppHandle) -> Result<String, String> {
    toggle_window_mode(app, FloatingQuotaMode::Topmost)
}

pub(super) fn toggle_desktop(app: &tauri::AppHandle) -> Result<String, String> {
    toggle_window_mode(app, FloatingQuotaMode::Desktop)
}

pub(super) fn open_main(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_string())?;
    let mode = app
        .try_state::<Mutex<AppState>>()
        .ok_or_else(|| "app state unavailable".to_string())?
        .lock()
        .map_err(|error| error.to_string())?
        .surface_machine
        .current();

    if should_open_dashboard(mode) {
        crate::shell::reopen_to_target(app, SurfaceMode::PopOut, SurfaceTarget::Dashboard, None)?;
    } else {
        if window.is_minimized().map_err(|error| error.to_string())? {
            window.unminimize().map_err(|error| error.to_string())?;
        }
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
    }

    hide(app)
}

pub(super) fn close(app: &tauri::AppHandle) -> Result<(), String> {
    MANUALLY_POSITIONED.store(false, Ordering::Relaxed);
    WINDOW_MODE.store(FloatingQuotaMode::Normal as u8, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(LABEL) {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_window_selects_show_and_visible_window_selects_hide() {
        assert_eq!(desired_toggle_action(false), ToggleAction::Show);
        assert_eq!(desired_toggle_action(true), ToggleAction::Hide);
    }

    #[test]
    fn manual_drag_keeps_position_until_process_restart() {
        assert_eq!(positioning_action(false), PositioningAction::AnchorToBall);
        assert_eq!(positioning_action(true), PositioningAction::KeepCurrent);
    }

    #[test]
    fn every_window_layer_transition_takes_one_click() {
        use FloatingQuotaMode::{Desktop, Normal, Topmost};

        assert_eq!(mode_after_click(Normal, Topmost), Topmost);
        assert_eq!(mode_after_click(Normal, Desktop), Desktop);
        assert_eq!(mode_after_click(Topmost, Topmost), Normal);
        assert_eq!(mode_after_click(Topmost, Desktop), Desktop);
        assert_eq!(mode_after_click(Desktop, Topmost), Topmost);
        assert_eq!(mode_after_click(Desktop, Desktop), Normal);
    }

    #[test]
    fn opening_main_restores_visible_state_but_hidden_state_opens_dashboard() {
        assert!(should_open_dashboard(crate::surface::SurfaceMode::Hidden));
        assert!(!should_open_dashboard(crate::surface::SurfaceMode::PopOut));
        assert!(!should_open_dashboard(
            crate::surface::SurfaceMode::Settings
        ));
    }
}
