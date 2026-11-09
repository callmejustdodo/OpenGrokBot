//! Formation's shared contract: the commands, events, snapshot, peer payloads, and
//! errors that formationd, the app, the CLI, and MCP exchange. Written once here and
//! generated into TypeScript with `ts-rs` (`cargo test` writes `bindings/`).
//!
//! Each file has one owning role (implementation plan, "Roles and ownership"):
//!
//! | Role | Files |
//! | --- | --- |
//! | A · Agent runtime | `common`, `agent`, `harness` |
//! | B · Collaboration core | `team`, `peer`, `policy`, `ask`, `request` |
//! | C · App | `snapshot`, `events` (the envelope; payloads live with their owner) |
//!
//! After the first merge, changes are additive only; a breaking change needs the
//! approval of all four roles. JSON is snake_case throughout.

pub mod agent;
pub mod ask;
pub mod common;
pub mod events;
pub mod harness;
pub mod peer;
pub mod policy;
pub mod request;
pub mod snapshot;
pub mod team;

/// Every JSON-RPC method on the local socket.
pub fn methods() -> impl Iterator<Item = &'static str> {
    [
        agent::METHODS,
        harness::METHODS,
        team::METHODS,
        peer::METHODS,
        policy::METHODS,
        ask::METHODS,
        request::METHODS,
        snapshot::METHODS,
    ]
    .into_iter()
    .flatten()
    .copied()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use crate::common::Timestamp;

    /// Serializes, deserializes, checks equality, and returns the JSON for shape checks.
    pub fn roundtrip<T>(value: &T) -> serde_json::Value
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        let json = serde_json::to_value(value).expect("serialize");
        let back: T = serde_json::from_value(json.clone()).expect("deserialize");
        assert_eq!(&back, value);
        json
    }

    pub fn t0() -> Timestamp {
        "2026-11-09T09:00:00Z".parse().expect("timestamp")
    }
}

#[cfg(test)]
mod tests {
    use super::methods;

    #[test]
    fn method_names_are_unique_snake_case() {
        let mut names: Vec<&str> = methods().collect();
        for name in &names {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name} is not snake_case"
            );
        }
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate method name");
    }

    #[test]
    fn documented_commands_exist() {
        // Tech spec, "UI ↔ formationd", and the frontend screen mapping.
        let names: Vec<&str> = methods().collect();
        for documented in [
            "agents_list",
            "agent_create",
            "agent_start",
            "agent_stop",
            "agent_message_send",
            "agent_message_steer",
            "agent_message_withdraw",
            "agent_files_read",
            "agent_files_write",
            "agent_connect",
            "share_update",
            "share_preview",
            "ask",
            "question_cancel",
            "question_refresh",
            "question_route_choose",
            "question_answer",
            "question_handoff",
            "request_create",
            "request_cancel",
            "approval_decide",
            "rules_update",
            "rules_revoke",
            "team_create",
            "team_join",
            "team_invite",
            "team_remove",
            "team_leave",
            "history_delete",
        ] {
            assert!(names.contains(&documented), "missing {documented}");
        }
    }
}
