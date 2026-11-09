//! Windows, menus and the menu bar icon (frontend architecture, "Windows"):
//! the main window, the menu bar panel (SC-002) under the tray icon, the
//! Settings window (SC-018), the app menu, and the icon itself.
//!
//! The tray icon is created here only, never also in `tauri.conf.json`: defining
//! it in both places shows two icons (tauri#10912).

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindowBuilder, Window,
    WindowEvent,
};

pub const MAIN: &str = "main";
pub const PANEL: &str = "menubar";
pub const SETTINGS: &str = "settings";

/// Tauri event asking the main window to open an item; the payload is an
/// `AttentionTarget` (`src/notifications/navigate.ts` handles it).
pub const NAVIGATE: &str = "app://navigate";

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
}

/// Opens the main window on one item (an `AttentionTarget`), for the menu
/// bar panel's rows and notification clicks.
pub fn focus_item(app: &AppHandle, target: Value) {
    show_main_window(app);
    let _ = app.emit_to(MAIN, NAVIGATE, target);
}

/// Shows the Settings window, creating it if it was closed.
pub fn show_settings_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    let window = match app.get_webview_window(SETTINGS) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
            .title("Settings")
            .inner_size(620.0, 460.0)
            .resizable(false)
            .minimizable(false)
            .build()?,
    };
    window.show()?;
    window.set_focus()
}

/// The app menu: the standard macOS menus, with "Settings…" (⌘,) in the app
/// menu after "About".
pub fn create_app_menu(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::default(app)?;
    #[cfg(target_os = "macos")]
    if let Some(tauri::menu::MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
        let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
        app_menu.insert_items(&[&separator, &settings], 1)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id() == "settings" {
            let _ = show_settings_window(app);
        }
    });
    Ok(())
}

/// The menu bar icon: a left click toggles the panel; a right click shows a
/// short menu to open the main window or quit. Quitting leaves formationd and
/// every agent running (D-048).
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Formation", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Formation", true, Some("Cmd+Q"))?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("formation")
        .tooltip("Formation")
        .icon_as_template(true)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_panel(tray.app_handle(), rect);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Shows the panel centered under the tray icon, or hides it if it is open.
fn toggle_panel(app: &AppHandle, icon: Rect) {
    let Some(panel) = app.get_webview_window(PANEL) else {
        return;
    };
    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
        return;
    }
    let scale = panel.scale_factor().unwrap_or(1.0);
    let icon_pos = icon.position.to_physical::<f64>(scale);
    let icon_size = icon.size.to_physical::<f64>(scale);
    let width = panel
        .outer_size()
        .map(|s| f64::from(s.width))
        .unwrap_or(0.0);
    let (x, y) = panel_origin(
        icon_pos.x,
        icon_pos.y,
        icon_size.width,
        icon_size.height,
        width,
    );
    let _ = panel.set_position(PhysicalPosition::new(x, y));
    let _ = panel.show();
    let _ = panel.set_focus();
}

/// Top-left of a panel `width` wide, centered under an icon.
fn panel_origin(icon_x: f64, icon_y: f64, icon_w: f64, icon_h: f64, width: f64) -> (f64, f64) {
    (
        (icon_x + icon_w / 2.0 - width / 2.0).max(0.0),
        icon_y + icon_h,
    )
}

/// The panel closes when it loses focus, like a menu.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() == PANEL
        && let WindowEvent::Focused(false) = event
    {
        let _ = window.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::panel_origin;

    #[test]
    fn the_panel_is_centered_under_the_icon() {
        assert_eq!(panel_origin(1000.0, 0.0, 40.0, 48.0, 680.0), (680.0, 48.0));
    }

    #[test]
    fn the_panel_stays_on_screen_at_the_left_edge() {
        assert_eq!(panel_origin(10.0, 0.0, 40.0, 48.0, 680.0).0, 0.0);
    }
}
