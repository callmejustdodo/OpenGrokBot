//! Updates the app from GitHub Releases (tech spec, "Distribution").
//!
//! The updater plugin reads `latest.json` from the latest release, checks its
//! signature against the public key in `tauri.conf.json`, and replaces the app
//! bundle. The bundle holds formationd too, so after installing, the app
//! restarts formationd on the new binary before relaunching itself; otherwise
//! the new app would talk to the old service until the next login.
//!
//! The app checks once at launch and whenever the owner picks "Check for
//! Updates…" in the app menu, and always asks before installing, because
//! restarting formationd stops agent turns in progress. Only a copy running
//! from an app bundle updates: the updater replaces the folder around the
//! executable, which for a development build would be `target/debug`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

use crate::launch_agent;

/// How long a check may take before it gives up, so a stalled network does
/// not keep the menu's check waiting behind the launch check.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether a check is running, so a second one (a menu click during the launch
/// check) does not open a second dialog.
#[derive(Default)]
pub struct Checking(AtomicBool);

/// Clears [`Checking`] when a check ends, even by a panic.
struct CheckingGuard<'a>(&'a AtomicBool);

impl Drop for CheckingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Whether `exe` runs from an app bundle (`<name>.app/Contents/MacOS/<exe>`),
/// the only place the updater can safely replace.
pub fn in_app_bundle(exe: &Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && contents
            .parent()
            .and_then(Path::extension)
            .is_some_and(|ext| ext == "app")
}

/// Who started a check: the launch check stays silent unless an update is
/// found; a check from the menu also reports "up to date" and errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Launch,
    Menu,
}

/// Starts a check in the background.
pub fn check(app: &AppHandle, trigger: Trigger, restart_service: bool) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let bundled = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .is_ok_and(|exe| in_app_bundle(&exe));
        if !bundled {
            if trigger == Trigger::Menu {
                tell(&app, MessageDialogKind::Info, NOT_INSTALLED).await;
            }
            return;
        }
        let checking = app.state::<Checking>();
        if checking.0.swap(true, Ordering::SeqCst) {
            if trigger == Trigger::Menu {
                tell(&app, MessageDialogKind::Info, ALREADY_CHECKING).await;
            }
            return;
        }
        let _guard = CheckingGuard(&checking.0);
        run(&app, trigger, restart_service).await;
    });
}

/// The answer to a menu check from a copy that is not an installed app.
pub const NOT_INSTALLED: &str =
    "Updates install only into the installed OpenGrokBot app, not into a development build.";

/// The answer to a menu check while another check is running.
pub const ALREADY_CHECKING: &str = "OpenGrokBot is already checking for updates.";

async fn run(app: &AppHandle, trigger: Trigger, restart_service: bool) {
    let found = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(updater) => updater.check().await,
        Err(err) => Err(err),
    };
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => {
            if trigger == Trigger::Menu {
                let current = app.package_info().version.to_string();
                tell(app, MessageDialogKind::Info, &up_to_date(&current)).await;
            }
            return;
        }
        Err(err) => {
            eprintln!("update check failed: {err}");
            if trigger == Trigger::Menu {
                tell(
                    app,
                    MessageDialogKind::Error,
                    &format!("Couldn't check for updates.\n\n{err}"),
                )
                .await;
            }
            return;
        }
    };

    if !ask(app, &available(&update.version, &update.current_version)).await {
        return;
    }
    if let Err(err) = update.download_and_install(|_, _| {}, || {}).await {
        eprintln!("update install failed: {err}");
        tell(
            app,
            MessageDialogKind::Error,
            &format!("Couldn't install the update.\n\n{err}"),
        )
        .await;
        return;
    }
    if restart_service {
        let identifier = app.config().identifier.clone();
        let restarted =
            tauri::async_runtime::spawn_blocking(move || launch_agent::restart(&identifier)).await;
        match restarted {
            Ok(Ok(())) => eprintln!("formationd LaunchAgent: restarted for the update"),
            Ok(Err(err)) => eprintln!("formationd LaunchAgent not restarted: {err}"),
            Err(err) => eprintln!("formationd LaunchAgent not restarted: {err}"),
        }
    }
    app.restart();
}

/// The question before installing `version` over `current`.
pub fn available(version: &str, current: &str) -> String {
    format!(
        "OpenGrokBot {version} is available. You have {current}.\n\n\
         Installing restarts OpenGrokBot and its background service. \
         Agent turns in progress will stop."
    )
}

/// The answer to a menu check when nothing newer is published.
pub fn up_to_date(current: &str) -> String {
    format!("OpenGrokBot {current} is the latest version.")
}

/// Asks to install; true when the owner picks "Install and Restart".
async fn ask(app: &AppHandle, message: &str) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title("Update Available")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Install and Restart".into(),
            "Later".into(),
        ))
        .show(move |install| {
            let _ = tx.send(install);
        });
    rx.await.unwrap_or(false)
}

/// Shows a message and waits until it is closed.
async fn tell(app: &AppHandle, kind: MessageDialogKind, message: &str) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title("Software Update")
        .kind(kind)
        .show(move |_| {
            let _ = tx.send(());
        });
    let _ = rx.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_names_both_versions_and_warns_about_agents() {
        let text = available("0.2.0", "0.1.0");
        assert!(text.starts_with("OpenGrokBot 0.2.0 is available. You have 0.1.0."));
        assert!(text.contains("Agent turns in progress will stop."));
    }

    #[test]
    fn only_an_app_bundle_updates() {
        assert!(in_app_bundle(Path::new(
            "/Applications/OpenGrokBot.app/Contents/MacOS/formation-desktop"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/me/OpenGrokBot/target/debug/formation-desktop"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/me/OpenGrokBot/target/release/formation-desktop"
        )));
        assert!(!in_app_bundle(Path::new(
            "/tmp/Contents/MacOS/formation-desktop"
        )));
        assert!(!in_app_bundle(Path::new("formation-desktop")));
    }

    #[test]
    fn up_to_date_names_the_current_version() {
        assert_eq!(
            up_to_date("0.1.0"),
            "OpenGrokBot 0.1.0 is the latest version."
        );
    }
}
