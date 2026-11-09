mod launch_agent;

use tauri::{AppHandle, Manager};

/// Launch argument that keeps the main window closed, so formationd can open
/// the app in the background to show a notification (D-048).
pub const HIDDEN_ARG: &str = "--hidden";

/// Whether the arguments ask for a hidden start.
fn starts_hidden(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|arg| arg == HIDDEN_ARG)
}

/// Whether the app runs against `fake-formationd` (`FORMATION_FAKE=daemon`),
/// in which case it must not register the real service.
fn uses_fake_daemon(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.split(',').any(|part| part.trim() == "daemon"))
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let hidden = starts_hidden(std::env::args());
    let fake_daemon = uses_fake_daemon(std::env::var("FORMATION_FAKE").ok().as_deref());

    tauri::Builder::default()
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
            if !hidden {
                show_main_window(app.handle());
            }
            Ok(())
        })
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
                show_main_window(_app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
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
