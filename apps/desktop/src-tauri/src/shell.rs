//! Windows, menus and the menu bar icon (frontend architecture, "Windows"):
//! the main window, the menu bar panel (SC-002) under the tray icon, the
//! Settings window (SC-018), the app menu, and the icon itself.
//!
//! The tray icon is created here only, never also in `tauri.conf.json`: defining
//! it in both places shows two icons (tauri#10912).
//!
//! Closing the main window hides it instead of destroying it: the app keeps
//! running in the menu bar, and the panel, notifications and the Dock icon
//! show the same window again, with its webview (and its navigation
//! listener) still alive.

use std::sync::Mutex;

use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindowBuilder, Window,
    WindowEvent,
};

pub const MAIN: &str = "main";
pub const PANEL: &str = "menubar";
pub const SETTINGS: &str = "settings";
/// The tray icon's ID.
pub const TRAY: &str = "formation";

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

/// Shows the main window when the Dock icon is clicked and it is hidden,
/// even if another window (Settings) is open.
pub fn on_reopen(app: &AppHandle) {
    let visible = app
        .get_webview_window(MAIN)
        .is_some_and(|w| w.is_visible().unwrap_or(false));
    if !visible {
        show_main_window(app);
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

/// The app menu: the standard macOS menus, with "Check for Updates…" and
/// "Settings…" (⌘,) in the app menu after "About". Picking "Check for
/// Updates…" calls `check_for_updates`.
pub fn create_app_menu(
    app: &AppHandle,
    check_for_updates: impl Fn(&AppHandle) + Send + Sync + 'static,
) -> tauri::Result<()> {
    let menu = Menu::default(app)?;
    #[cfg(target_os = "macos")]
    if let Some(tauri::menu::MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let updates = MenuItem::with_id(app, "updates", "Check for Updates…", true, None::<&str>)?;
        let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
        let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
        app_menu.insert_items(&[&updates, &separator, &settings], 1)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(move |app, event| match event.id().as_ref() {
        "settings" => {
            let _ = show_settings_window(app);
        }
        "updates" => check_for_updates(app),
        _ => {}
    });
    Ok(())
}

/// The menu bar icon: a left click toggles the panel; a right click shows a
/// short menu to open the main window or quit. Quitting leaves formationd and
/// every agent running (D-048).
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open OpenGrokBot", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit OpenGrokBot", true, Some("Cmd+Q"))?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY)
        .tooltip("OpenGrokBot")
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
    if let Some(icon) = tray_glyph() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// The tray icon in its two looks: plain, and with a dot when something needs
/// the owner (SC-002). Both are template images, made once from the tray glyph.
pub struct TrayIcons {
    plain: Option<Image<'static>>,
    dotted: Option<Image<'static>>,
    attention: Mutex<bool>,
}

impl TrayIcons {
    pub fn new() -> Self {
        let plain = tray_glyph();
        let dotted = plain.as_ref().map(|icon| {
            let (width, height) = (icon.width(), icon.height());
            Image::new_owned(with_dot(icon.rgba(), width, height), width, height)
        });
        Self {
            plain,
            dotted,
            attention: Mutex::new(false),
        }
    }
}

/// The menu bar glyph: the app icon's chat bubble with its eyes cut out, black
/// on clear (icons/source/tray-icon.svg). The app icon itself is a filled
/// square, which as a template image would draw as a solid block.
fn tray_glyph() -> Option<Image<'static>> {
    Image::from_bytes(include_bytes!("../icons/tray-icon.png"))
        .ok()
        .map(Image::to_owned)
}

/// Shows or removes the tray icon's dot. The menu bar panel's window calls
/// this when whether anything needs the owner changes.
pub fn set_tray_attention(app: &AppHandle, attention: bool) -> tauri::Result<()> {
    let Some(icons) = app.try_state::<TrayIcons>() else {
        return Ok(());
    };
    {
        let mut current = icons.attention.lock().unwrap_or_else(|p| p.into_inner());
        if *current == attention {
            return Ok(());
        }
        *current = attention;
    }
    let icon = if attention {
        &icons.dotted
    } else {
        &icons.plain
    };
    match (app.tray_by_id(TRAY), icon) {
        (Some(tray), Some(icon)) => tray.set_icon_with_as_template(Some(icon.clone()), true),
        _ => Ok(()),
    }
}

/// A copy of an RGBA icon with a dot in its top-right corner and a clear ring
/// around the dot, so the dot stands apart from the icon's shape. The dot is
/// opaque black: in a template image macOS draws it in the menu bar's text
/// color like the rest of the icon, so it cannot be orange as in the mockup.
fn with_dot(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let size = f64::from(width.min(height));
    let radius = size * 0.2;
    let ring = radius + size * 0.08;
    // Pixel centers sit on whole coordinates, so the edges are at -0.5.
    let (cx, cy) = (f64::from(width) - 0.5 - radius, radius - 0.5);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let Some(pixel) = out.get_mut(i..i + 4) else {
                return out;
            };
            let distance = (f64::from(x) - cx).hypot(f64::from(y) - cy);
            // Coverage, anti-aliased over one pixel.
            let dot = (radius + 0.5 - distance).clamp(0.0, 1.0);
            let clear = (ring + 0.5 - distance).clamp(0.0, 1.0);
            let kept = f64::from(pixel[3]) * (1.0 - clear);
            if dot > 0.0 {
                pixel[..3].fill(0);
            }
            pixel[3] = (dot * 255.0 + kept * (1.0 - dot)).round() as u8;
        }
    }
    out
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

/// The panel closes when it loses focus, like a menu. Closing the main window
/// hides it, so it can be shown again on an item.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    match (window.label(), event) {
        (PANEL, WindowEvent::Focused(false)) => {
            let _ = window.hide();
        }
        (MAIN, WindowEvent::CloseRequested { api, .. }) => {
            api.prevent_close();
            let _ = window.hide();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{panel_origin, tray_glyph, with_dot};

    fn alpha(rgba: &[u8], width: u32, x: u32, y: u32) -> u8 {
        rgba[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_tray_glyph_is_three_linked_rings() {
        let glyph = tray_glyph().expect("icons/tray-icon.png decodes");
        let (width, height) = (glyph.width(), glyph.height());
        assert_eq!((width, height), (44, 44));
        // A template image of the Figma mark: the corner is clear, the link between the
        // top rings and a ring's stroke are opaque, a ring's center and the gap below
        // the bottom ring are clear.
        assert_eq!(alpha(glyph.rgba(), width, 0, 0), 0);
        assert_eq!(alpha(glyph.rgba(), width, 22, 14), 255);
        assert_eq!(alpha(glyph.rgba(), width, 17, 14), 255);
        assert_eq!(alpha(glyph.rgba(), width, 12, 14), 0);
        assert_eq!(alpha(glyph.rgba(), width, 22, 30), 0);
    }

    #[test]
    fn the_dot_is_drawn_in_the_top_right_corner() {
        let icon = vec![255; 32 * 32 * 4];
        let dotted = with_dot(&icon, 32, 32);
        assert_eq!(dotted.len(), icon.len());
        // The dot's center is opaque black.
        let center = ((5 * 32 + 26) * 4) as usize;
        assert_eq!(&dotted[center..center + 4], &[0, 0, 0, 255]);
        // A clear ring separates it from the icon.
        assert_eq!(alpha(&dotted, 32, 18, 5), 0);
        // The rest of the icon is untouched.
        assert_eq!(alpha(&dotted, 32, 2, 30), 255);
        assert_eq!(&dotted[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn the_dot_shows_over_a_transparent_corner() {
        let icon = vec![0; 16 * 16 * 4];
        let dotted = with_dot(&icon, 16, 16);
        assert_eq!(alpha(&dotted, 16, 13, 2), 255);
        assert_eq!(alpha(&dotted, 16, 2, 13), 0);
    }

    #[test]
    fn a_short_buffer_does_not_panic() {
        assert_eq!(with_dot(&[0; 7], 4, 4).len(), 7);
    }

    #[test]
    fn the_panel_is_centered_under_the_icon() {
        assert_eq!(panel_origin(1000.0, 0.0, 40.0, 48.0, 680.0), (680.0, 48.0));
    }

    #[test]
    fn the_panel_stays_on_screen_at_the_left_edge() {
        assert_eq!(panel_origin(10.0, 0.0, 40.0, 48.0, 680.0).0, 0.0);
    }
}
