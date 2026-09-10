//! Non-activating quota window shown while the pointer is over the tray icon.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewUrl};

use crate::state::TrayAnchor;
use crate::window_positioner::{PanelSize, Rect, calculate_panel_position};

pub const TRAY_HOVER_LABEL: &str = "tray-hover";
const DEFAULT_WIDTH: f64 = 320.0;
const DEFAULT_HEIGHT: f64 = 300.0;
const MIN_WIDTH: f64 = 248.0;
const DESIGN_MAX_WIDTH: f64 = 520.0;
const WORK_AREA_HORIZONTAL_MARGIN: f64 = 16.0;
const HIDE_DELAY_MS: u64 = 200;

static VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(0);
static WINDOW_READY: AtomicBool = AtomicBool::new(false);
static VISIBILITY_REQUESTED: AtomicBool = AtomicBool::new(false);
static LAST_ANCHOR: OnceLock<Mutex<Option<TrayAnchor>>> = OnceLock::new();

fn should_show_window(is_ready: bool, visibility_requested: bool) -> bool {
    is_ready && visibility_requested
}

fn anchor_store() -> &'static Mutex<Option<TrayAnchor>> {
    LAST_ANCHOR.get_or_init(|| Mutex::new(None))
}

fn cancel_pending_hide() {
    VISIBILITY_GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn anchor_monitor(monitors: &[tauri::Monitor], anchor: TrayAnchor) -> Option<&tauri::Monitor> {
    let center_x = anchor.x + anchor.width as i32 / 2;
    let center_y = anchor.y + anchor.height as i32 / 2;
    monitors
        .iter()
        .find(|monitor| {
            let p = monitor.position();
            let s = monitor.size();
            center_x >= p.x
                && center_x < p.x + s.width as i32
                && center_y >= p.y
                && center_y < p.y + s.height as i32
        })
        .or_else(|| monitors.first())
}

fn clamp_logical_size(
    requested_width: f64,
    requested_height: f64,
    work_area: Option<(u32, f64)>,
) -> (f64, f64) {
    let max_width = work_area
        .map(|(physical_width, scale_factor)| {
            let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
                scale_factor
            } else {
                1.0
            };
            (physical_width as f64 / scale_factor - WORK_AREA_HORIZONTAL_MARGIN).max(1.0)
        })
        .unwrap_or(DESIGN_MAX_WIDTH)
        .min(DESIGN_MAX_WIDTH);
    let min_width = MIN_WIDTH.min(max_width);
    let width = if requested_width.is_finite() {
        requested_width
    } else {
        DEFAULT_WIDTH
    }
    .clamp(min_width, max_width);
    let height = if requested_height.is_finite() {
        requested_height
    } else {
        DEFAULT_HEIGHT
    }
    .clamp(48.0, 520.0);
    (width, height)
}

fn position_window(window: &tauri::WebviewWindow, anchor: TrayAnchor) {
    let Ok(monitors) = window.available_monitors() else {
        return;
    };
    let Some(monitor) = anchor_monitor(&monitors, anchor) else {
        return;
    };
    let scale = monitor.scale_factor();
    let bounds = Rect {
        x: monitor.position().x,
        y: monitor.position().y,
        width: monitor.size().width,
        height: monitor.size().height,
    };
    let area = monitor.work_area();
    let work_area = Rect {
        x: area.position.x,
        y: area.position.y,
        width: area.size.width,
        height: area.size.height,
    };
    let logical_size = window
        .inner_size()
        .ok()
        .map(|size| PanelSize {
            width: ((size.width as f64 / scale).round().max(1.0)) as u32,
            height: ((size.height as f64 / scale).round().max(1.0)) as u32,
        })
        .unwrap_or(PanelSize {
            width: DEFAULT_WIDTH as u32,
            height: DEFAULT_HEIGHT as u32,
        });
    let icon = Rect {
        x: anchor.x,
        y: anchor.y,
        width: anchor.width,
        height: anchor.height,
    };
    let (x, y) = calculate_panel_position(&icon, &bounds, &work_area, &logical_size, scale);
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

pub fn show(app: &tauri::AppHandle, anchor: TrayAnchor) -> Result<(), String> {
    cancel_pending_hide();
    VISIBILITY_REQUESTED.store(true, Ordering::SeqCst);
    *anchor_store().lock().unwrap() = Some(anchor);

    let window = if let Some(window) = app.get_webview_window(TRAY_HOVER_LABEL) {
        window
    } else {
        WINDOW_READY.store(false, Ordering::SeqCst);
        let url = WebviewUrl::App("index.html?window=tray-hover".into());
        let builder = tauri::WebviewWindowBuilder::new(app, TRAY_HOVER_LABEL, url)
            .title("CodexBar")
            .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .visible(false);
        #[cfg(windows)]
        let builder = builder.transparent(true);
        builder
            .background_color(tauri::utils::config::Color(0, 0, 0, 0))
            .build()
            .map_err(|error| error.to_string())?
    };

    crate::floatbar::apply_no_activate(&window);
    position_window(&window, anchor);
    if should_show_window(
        WINDOW_READY.load(Ordering::SeqCst),
        VISIBILITY_REQUESTED.load(Ordering::SeqCst),
    ) {
        window.show().map_err(|error| error.to_string())?;
        crate::floatbar::apply_no_activate(&window);
    }
    Ok(())
}

pub fn schedule_hide(app: &tauri::AppHandle) {
    let generation = VISIBILITY_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(HIDE_DELAY_MS)).await;
        if VISIBILITY_GENERATION.load(Ordering::SeqCst) == generation {
            VISIBILITY_REQUESTED.store(false, Ordering::SeqCst);
            if let Some(window) = app.get_webview_window(TRAY_HOVER_LABEL) {
                let _ = window.hide();
            }
        }
    });
}

#[tauri::command]
pub fn set_tray_hover_pointer_inside(app: tauri::AppHandle, inside: bool) {
    if inside {
        cancel_pending_hide();
    } else {
        schedule_hide(&app);
    }
}

#[tauri::command]
pub fn resize_tray_hover(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    let Some(window) = app.get_webview_window(TRAY_HOVER_LABEL) else {
        return Ok(());
    };
    let anchor = *anchor_store().lock().unwrap();
    let work_area = anchor.and_then(|anchor| {
        let monitors = window.available_monitors().ok()?;
        let monitor = anchor_monitor(&monitors, anchor)?;
        let area = monitor.work_area();
        Some((area.size.width, monitor.scale_factor()))
    });
    let (width, height) = clamp_logical_size(width, height, work_area);
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|error| error.to_string())?;
    if let Some(anchor) = anchor {
        position_window(&window, anchor);
    }
    crate::floatbar::apply_no_activate(&window);
    WINDOW_READY.store(true, Ordering::SeqCst);
    if should_show_window(
        WINDOW_READY.load(Ordering::SeqCst),
        VISIBILITY_REQUESTED.load(Ordering::SeqCst),
    ) {
        window.show().map_err(|error| error.to_string())?;
        crate::floatbar::apply_no_activate(&window);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shows_after_content_is_ready_and_visibility_is_requested() {
        assert!(!should_show_window(false, true));
        assert!(!should_show_window(true, false));
        assert!(should_show_window(true, true));
    }

    #[test]
    fn wide_work_area_keeps_requested_width_above_legacy_cap() {
        assert_eq!(
            clamp_logical_size(480.0, 300.0, Some((1_920, 1.0))),
            (480.0, 300.0)
        );
    }

    #[test]
    fn requested_width_stops_at_design_cap() {
        assert_eq!(
            clamp_logical_size(900.0, 300.0, Some((1_920, 1.0))),
            (520.0, 300.0)
        );
    }

    #[test]
    fn physical_work_area_is_converted_with_monitor_scale() {
        assert_eq!(
            clamp_logical_size(480.0, 300.0, Some((600, 1.5))),
            (384.0, 300.0)
        );
        assert_eq!(
            clamp_logical_size(480.0, 300.0, Some((800, 2.0))),
            (384.0, 300.0)
        );
    }

    #[test]
    fn extremely_narrow_work_area_uses_available_width_without_panicking() {
        assert_eq!(
            clamp_logical_size(480.0, 300.0, Some((200, 1.0))),
            (184.0, 300.0)
        );
        assert_eq!(
            clamp_logical_size(480.0, 300.0, Some((8, 1.0))),
            (1.0, 300.0)
        );
    }

    #[test]
    fn failed_monitor_query_falls_back_to_design_cap_and_height_clamp_is_unchanged() {
        assert_eq!(clamp_logical_size(900.0, 20.0, None), (520.0, 48.0));
        assert_eq!(clamp_logical_size(480.0, 900.0, None), (480.0, 520.0));
    }
}
