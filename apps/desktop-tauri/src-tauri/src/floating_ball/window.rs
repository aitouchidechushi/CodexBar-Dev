use tauri::{Manager, PhysicalPosition, WebviewUrl};

use crate::geometry_store::{self, StoredGeometry};

pub const LABEL: &str = "floating-ball";
const BALL_WINDOW_SIZE: i32 = 42;
const DEFAULT_MARGIN: i32 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkAreaRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowAction {
    Show,
    Close,
}

pub(super) fn desired_action(enabled: bool) -> WindowAction {
    if enabled {
        WindowAction::Show
    } else {
        WindowAction::Close
    }
}

fn rectangles_intersect(first: WorkAreaRect, second: WorkAreaRect) -> bool {
    let first_right = i64::from(first.x) + i64::from(first.width);
    let first_bottom = i64::from(first.y) + i64::from(first.height);
    let second_right = i64::from(second.x) + i64::from(second.width);
    let second_bottom = i64::from(second.y) + i64::from(second.height);

    i64::from(first.x) < second_right
        && first_right > i64::from(second.x)
        && i64::from(first.y) < second_bottom
        && first_bottom > i64::from(second.y)
}

fn default_position(primary: WorkAreaRect) -> (i32, i32) {
    let right = i64::from(primary.x) + i64::from(primary.width);
    let bottom = i64::from(primary.y) + i64::from(primary.height);
    let max_x = (right - i64::from(BALL_WINDOW_SIZE)).max(i64::from(primary.x));
    let max_y = (bottom - i64::from(BALL_WINDOW_SIZE)).max(i64::from(primary.y));
    let x =
        (right - i64::from(BALL_WINDOW_SIZE + DEFAULT_MARGIN)).clamp(i64::from(primary.x), max_x);
    let y = (i64::from(primary.y) + (i64::from(primary.height) - i64::from(BALL_WINDOW_SIZE)) / 2)
        .clamp(i64::from(primary.y), max_y);
    (x as i32, y as i32)
}

fn resolve_position(
    stored: Option<(i32, i32)>,
    monitors: &[WorkAreaRect],
    primary: WorkAreaRect,
) -> (i32, i32) {
    if let Some((x, y)) = stored {
        let ball = WorkAreaRect {
            x,
            y,
            width: BALL_WINDOW_SIZE as u32,
            height: BALL_WINDOW_SIZE as u32,
        };
        if monitors
            .iter()
            .copied()
            .any(|monitor| rectangles_intersect(ball, monitor))
        {
            return (x, y);
        }
    }

    default_position(primary)
}

fn monitor_work_area(monitor: &tauri::Monitor) -> WorkAreaRect {
    let area = monitor.work_area();
    WorkAreaRect {
        x: area.position.x,
        y: area.position.y,
        width: area.size.width,
        height: area.size.height,
    }
}

fn restore_position(window: &tauri::WebviewWindow) -> Result<(), String> {
    let monitors = window
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let primary = window
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .or_else(|| monitors.first().cloned())
        .ok_or_else(|| "no monitor available for floating ball".to_string())?;
    let work_areas = monitors.iter().map(monitor_work_area).collect::<Vec<_>>();
    let stored = geometry_store::load_entry(LABEL).map(|entry| (entry.x, entry.y));
    let (x, y) = resolve_position(stored, &work_areas, monitor_work_area(&primary));
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}

pub(super) fn show(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        restore_position(&window)?;
        return window.show().map_err(|error| error.to_string());
    }

    let builder =
        tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("floating-ball.html".into()))
            .title("CodexBar")
            .inner_size(BALL_WINDOW_SIZE as f64, BALL_WINDOW_SIZE as f64)
            .resizable(false)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .visible(false);
    #[cfg(windows)]
    let builder = builder.transparent(true);

    let window = builder
        .background_color(tauri::utils::config::Color(0, 0, 0, 0))
        .build()
        .map_err(|error| error.to_string())?;
    restore_position(&window)?;
    window.on_window_event(|event| {
        if let tauri::WindowEvent::Moved(position) = event {
            geometry_store::queue_save_entry(
                LABEL,
                StoredGeometry {
                    x: position.x,
                    y: position.y,
                    width: None,
                    height: None,
                },
            );
        }
    });
    window.show().map_err(|error| error.to_string())
}

pub(super) fn close(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: WorkAreaRect = WorkAreaRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1040,
    };

    #[test]
    fn enabled_state_selects_show_or_close() {
        assert_eq!(desired_action(true), WindowAction::Show);
        assert_eq!(desired_action(false), WindowAction::Close);
    }

    #[test]
    fn valid_stored_position_is_restored_unchanged() {
        assert_eq!(
            resolve_position(Some((120, 240)), &[PRIMARY], PRIMARY),
            (120, 240)
        );
    }

    #[test]
    fn disconnected_monitor_position_falls_back_to_primary() {
        assert_eq!(
            resolve_position(Some((4000, 3000)), &[PRIMARY], PRIMARY),
            (1854, 499)
        );
    }

    #[test]
    fn valid_negative_coordinate_monitor_position_is_preserved() {
        let secondary = WorkAreaRect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            resolve_position(Some((-100, 100)), &[PRIMARY, secondary], PRIMARY),
            (-100, 100)
        );
    }
}
