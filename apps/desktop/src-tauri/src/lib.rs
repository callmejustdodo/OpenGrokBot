pub mod formationd;
mod launch_agent;
mod notifications;
mod shell;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use formationd::{CallError, Connection, Sink, Status};

/// Launch argument that keeps the main window closed, so formationd can open
/// the app in the background to show a notification (D-048).
pub const HIDDEN_ARG: &str = "--hidden";

/// Tauri event carrying each `EventEnvelope` from formationd to every window.
pub const EVENT: &str = "formationd://event";
/// Tauri event carrying connection [`Status`] changes to every window.
pub const STATUS: &str = "formationd://status";

/// Whether the arguments ask for a hidden start.
fn starts_hidden(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|arg| arg == HIDDEN_ARG)
}

/// Whether the app runs against `fake-formationd` (`FORMATION_FAKE=daemon`),
/// in which case it must not register the real service.
fn uses_fake_daemon(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.split(',').any(|part| part.trim() == "daemon"))
}

/// Emits formationd's events and status to every window.
struct WindowSink(AppHandle);

impl Sink for WindowSink {
    fn event(&self, envelope: Value) {
        let _ = self.0.emit(EVENT, envelope);
    }

    fn status(&self, status: Status) {
        let _ = self.0.emit(STATUS, status);
    }
}

/// Forwards one command to formationd and returns its result.
#[tauri::command]
async fn formationd_call(
    conn: State<'_, Connection>,
    method: String,
    params: Value,
) -> Result<Value, CallError> {
    conn.call(&method, params).await
}

#[tauri::command]
fn formationd_status(conn: State<'_, Connection>) -> Status {
    conn.status()
}

/// Opens the main window, for the menu bar panel and notifications.
#[tauri::command]
fn show_main_window(app: AppHandle) {
    shell::show_main_window(&app);
}

/// Opens the main window on one item (an `AttentionTarget`).
#[tauri::command]
fn focus_item(app: AppHandle, target: Value) {
    shell::focus_item(&app, target);
}

/// Opens the Settings window (SC-018). Async, since it may create a window.
#[tauri::command]
async fn show_settings_window(app: AppHandle) -> Result<(), String> {
    shell::show_settings_window(&app).map_err(|e| e.to_string())
}

/// Shows a macOS notification that opens `target` when clicked (SC-019).
#[tauri::command]
fn show_notification(
    app: AppHandle,
    title: String,
    body: String,
    target: Value,
) -> Result<(), String> {
    notifications::show(&app, title, body, target)
}

/// Quits the app; formationd and every agent keep running (D-048).
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let hidden = starts_hidden(std::env::args());
    let fake = std::env::var("FORMATION_FAKE").ok();
    let fake_daemon = uses_fake_daemon(fake.as_deref());

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(notifications::Shown::default())
        .invoke_handler(tauri::generate_handler![
            formationd_call,
            formationd_status,
            show_main_window,
            focus_item,
            show_settings_window,
            show_notification,
            quit_app
        ])
        .setup(move |app| {
            if fake_daemon {
                eprintln!("formationd LaunchAgent: skipped (FORMATION_FAKE=daemon)");
            } else {
                let identifier = app.config().identifier.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    match launch_agent::register(&identifier) {
                        Ok(outcome) => eprintln!("formationd LaunchAgent: {outcome}"),
                        Err(err) => eprintln!("formationd LaunchAgent not registered: {err}"),
                    }
                });
            }

            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            let socket = std::env::var("FORMATION_SOCKET").ok();
            let path = formationd::socket_path(socket.as_deref(), fake.as_deref(), &home);
            eprintln!("formationd socket: {}", path.display());
            let sink = WindowSink(app.handle().clone());
            let conn = tauri::async_runtime::block_on(async move { Connection::start(path, sink) });
            app.manage(conn);

            shell::create_app_menu(app.handle())?;
            shell::create_tray(app.handle())?;
            notifications::on_click(app.handle());
            if !hidden {
                shell::show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(shell::on_window_event)
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            // Clicking the Dock icon of an app started hidden opens its window.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = _event
            {
                shell::show_main_window(_app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn hidden_only_with_the_flag() {
        assert!(starts_hidden(args(&["formation-desktop", "--hidden"])));
        assert!(!starts_hidden(args(&["formation-desktop"])));
        assert!(!starts_hidden(args(&["formation-desktop", "--hiddenx"])));
    }

    #[test]
    fn fake_daemon_is_one_of_the_fake_parts() {
        assert!(uses_fake_daemon(Some("daemon")));
        assert!(uses_fake_daemon(Some("claude, daemon")));
        assert!(!uses_fake_daemon(Some("claude")));
        assert!(!uses_fake_daemon(None));
    }
}
