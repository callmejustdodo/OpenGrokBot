//! macOS notifications (SC-019). The menu bar panel's webview decides when a
//! `notify` event alerts and words it (it holds the mirror and the language);
//! it asks this module to show it. Only that window does, so each item alerts
//! once. A click focuses the main window on the item it was shown for.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::{NotificationExt, PermissionState};

use crate::shell;

/// How many shown notifications keep their item for a later click.
const KEEP: usize = 64;

/// The notifications shown so far, by ID, with the item each one opens.
#[derive(Default)]
pub struct Shown(Mutex<ShownInner>);

#[derive(Default)]
struct ShownInner {
    last_id: i32,
    targets: VecDeque<(i32, Value)>,
}

impl Shown {
    /// Remembers `target` and returns the ID to show its notification with.
    pub fn add(&self, target: Value) -> i32 {
        let mut inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        inner.last_id = inner.last_id.checked_add(1).unwrap_or(1);
        let id = inner.last_id;
        inner.targets.push_back((id, target));
        while inner.targets.len() > KEEP {
            inner.targets.pop_front();
        }
        id
    }

    /// The item the notification `id` opens, while it is remembered.
    pub fn target(&self, id: i32) -> Option<Value> {
        let inner = self.0.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .targets
            .iter()
            .find(|(shown, _)| *shown == id)
            .map(|(_, target)| target.clone())
    }
}

/// Shows one notification that opens `target` (an `AttentionTarget`) when
/// clicked. Asks for permission the first time one is needed.
pub fn show(app: &AppHandle, title: String, body: String, target: Value) -> Result<(), String> {
    let notification = app.notification();
    if notification.permission_state().map_err(|e| e.to_string())? != PermissionState::Granted
        && notification
            .request_permission()
            .map_err(|e| e.to_string())?
            != PermissionState::Granted
    {
        return Err("notifications are not allowed".to_owned());
    }
    let id = app.state::<Shown>().add(target);
    notification
        .builder()
        .id(id)
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// Clicking a notification focuses the main window on its item.
pub fn on_click(app: &AppHandle) {
    let handle = app.clone();
    // Never fails on desktop; the signature matches the mobile one.
    let _ = app.notification().on_action(move |performed| {
        if performed.action_id() != "tap" {
            return;
        }
        let target = performed
            .notification()
            .and_then(|n| handle.state::<Shown>().target(n.id()));
        match target {
            Some(target) => shell::focus_item(&handle, target),
            None => shell::show_main_window(&handle),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_notification_keeps_its_own_target() {
        let shown = Shown::default();
        let a = shown.add(json!({ "kind": "approval", "approval_id": "a" }));
        let b = shown.add(json!({ "kind": "agent", "agent_id": "b" }));
        assert_ne!(a, b);
        assert_eq!(shown.target(a).unwrap()["approval_id"], "a");
        assert_eq!(shown.target(b).unwrap()["agent_id"], "b");
        assert_eq!(shown.target(b + 1), None);
    }

    #[test]
    fn old_notifications_are_forgotten() {
        let shown = Shown::default();
        let first = shown.add(json!({ "kind": "agent", "agent_id": "0" }));
        for i in 1..=KEEP {
            shown.add(json!({ "kind": "agent", "agent_id": i.to_string() }));
        }
        assert_eq!(shown.target(first), None);
        assert!(shown.target(first + 1).is_some());
    }
}
