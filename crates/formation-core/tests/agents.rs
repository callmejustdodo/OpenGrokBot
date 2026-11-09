use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use formation_core::{
    agents::{Agents, HarnessReadiness},
    ports::{AgentRepo, CommandCall, CommandHandler, Result},
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{
    agent::{Agent, AgentFiles, AgentState, HarnessKind, HarnessStatus},
    common::ErrorCode,
    events::Event,
    policy::Caller,
};
use serde_json::{Value, json};

struct NotReady;
impl HarnessReadiness for NotReady {
    fn check(&self) -> Result<Vec<HarnessStatus>> {
        Ok(vec![HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: false,
            signed_in: Some(false),
            version: None,
            path: None,
            problem: Some("Install Claude Code".into()),
        }])
    }
}

struct Fixture {
    handler: Agents,
    store: Arc<MemoryStore>,
    events: Arc<RecordingEventSink>,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("formation-agents-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let store = Arc::new(MemoryStore::default());
        let events = Arc::new(RecordingEventSink::default());
        let handler = Agents::new(
            root.clone(),
            store.clone(),
            events.clone(),
            Arc::new(NotReady),
        )
        .unwrap();
        Self {
            handler,
            store,
            events,
            root,
        }
    }
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(self.handler.handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            }))
    }
    fn create(&self) -> Agent {
        serde_json::from_value(self.call("agent_create", create_params()).unwrap()).unwrap()
    }
    fn read(&self, agent: &Agent) -> AgentFiles {
        serde_json::from_value(
            self.call("agent_files_read", json!({"agent_id": agent.id}))
                .unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn create_params() -> Value {
    json!({"name": "Marketing", "role": "Customer support", "harness": "codex",
        "avatar": {"shape": "round", "color": "blue"}, "folder": null,
        "repositories": ["/tmp/example-repo"], "instructions": "# Help customers\n"})
}

#[test]
fn agents_create_layout_stopped_store_list_and_event() {
    let f = Fixture::new();
    let a = f.create();
    assert_eq!(a.slug, "marketing");
    assert_eq!(a.state, AgentState::Stopped);
    assert_eq!(a.repositories, vec![PathBuf::from("/tmp/example-repo")]);
    assert_eq!(
        fs::read_to_string(a.folder_path.join("AGENTS.md")).unwrap(),
        "# Help customers\n"
    );
    assert_eq!(
        fs::read_to_string(a.folder_path.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
    assert!(a.folder_path.join(".claude/skills").is_dir());
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(a.folder_path.join("formation.toml")).unwrap()).unwrap();
    assert_eq!(config["name"].as_str(), Some("Marketing"));
    assert_eq!(config["role"].as_str(), Some("Customer support"));
    assert_eq!(config["harness"].as_str(), Some("codex"));
    assert_eq!(config["avatar"]["shape"].as_str(), Some("round"));
    assert_eq!(f.store.agent(a.id).unwrap(), Some(a.clone()));
    assert_eq!(f.call("agents_list", json!({})).unwrap(), json!([a]));
    assert_eq!(
        f.events
            .events()
            .iter()
            .filter(|(_, e)| matches!(e, Event::AgentUpdate(_)))
            .count(),
        1
    );
}

#[test]
fn agents_list_preserves_agent_with_deleted_folder() {
    let f = Fixture::new();
    let missing = f.create();
    let mut params = create_params();
    params["name"] = json!("Healthy");
    let healthy: Agent = serde_json::from_value(f.call("agent_create", params).unwrap()).unwrap();
    fs::remove_dir_all(&missing.folder_path).unwrap();

    let agents: Vec<Agent> =
        serde_json::from_value(f.call("agents_list", json!({})).unwrap()).unwrap();
    assert_eq!(agents.len(), 2);
    assert!(agents.contains(&missing));
    assert!(agents.contains(&healthy));
}

#[test]
fn agents_revision_ignores_unrelated_folder_contents() {
    let f = Fixture::new();
    let a = f.create();
    fs::create_dir_all(a.folder_path.join("node_modules/package")).unwrap();
    fs::write(a.folder_path.join("node_modules/package/index.js"), "code").unwrap();
    fs::write(a.folder_path.join(".claude/settings.json"), "{}").unwrap();
    assert_eq!(f.read(&a).revision, a.files_revision);

    let mut revision = a.files_revision.clone();
    for path in [
        "AGENTS.md",
        "CLAUDE.md",
        "formation.toml",
        ".claude/skills/SKILL.md",
    ] {
        fs::write(a.folder_path.join(path), "Outside edit").unwrap();
        let next = f.read(&a).revision;
        assert_ne!(next, revision, "{path}");
        revision = next;
    }
}

#[test]
fn agents_not_ready_still_creates_and_harness_check_publishes() {
    let f = Fixture::new();
    let status = f.call("harness_check", json!({})).unwrap();
    assert_eq!(status[0]["installed"], false);
    assert!(
        matches!(&f.events.events()[0].1, Event::HarnessUpdate(e) if !e.harnesses[0].installed)
    );
    let mut params = create_params();
    params["harness"] = json!("claude_code");
    let a: Agent = serde_json::from_value(f.call("agent_create", params).unwrap()).unwrap();
    assert!(a.folder_path.join("AGENTS.md").is_file());
}

#[test]
fn agents_slug_conflict_and_nonempty_folder_leave_no_partial_files() {
    let f = Fixture::new();
    let a = f.create();
    let mut params = create_params();
    params["name"] = json!("MARKETING!");
    assert_eq!(
        f.call("agent_create", params).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(f.store.agents().unwrap().len(), 1);
    // Without a chosen folder, an occupied default folder is still refused.
    let occupied = f.root.join("other");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "existing").unwrap();
    let mut params = create_params();
    params["name"] = json!("Other");
    assert_eq!(
        f.call("agent_create", params).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(fs::read_dir(&occupied).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 2);
    assert_eq!(f.store.agent(a.id).unwrap().unwrap(), a);
}

/// Every file under `root`, with its bytes; folders as `None`.
fn snapshot(root: &std::path::Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.push((relative, None));
                stack.push(path);
            } else {
                out.push((relative, Some(fs::read(&path).unwrap())));
            }
        }
    }
    out.sort();
    out
}

fn create_in(f: &Fixture, name: &str, folder: &std::path::Path) -> Result<Value> {
    let mut params = create_params();
    params["name"] = json!(name);
    params["folder"] = json!(folder);
    f.call("agent_create", params)
}

#[test]
fn agents_adopt_a_chosen_nonempty_folder_and_keep_its_files() {
    let f = Fixture::new();
    let project = f.root.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join(".claude")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(project.join(".claude/settings.json"), "{\"a\":1}").unwrap();
    fs::write(project.join("AGENTS.md"), "# Project rules\r\nkeep\n").unwrap();
    fs::write(project.join("CLAUDE.md"), "My own Claude notes\n").unwrap();
    let before = snapshot(&project);

    let a: Agent = serde_json::from_value(create_in(&f, "Project", &project).unwrap()).unwrap();
    assert_eq!(a.folder_path, project.canonicalize().unwrap());
    assert_eq!(f.store.agent(a.id).unwrap(), Some(a.clone()));
    // What was there is byte-identical; only formation.toml and .claude/skills/ are new.
    let after = snapshot(&project);
    for entry in &before {
        assert!(after.contains(entry), "{:?} changed", entry.0);
    }
    let mut added: Vec<_> = after
        .iter()
        .filter(|e| !before.contains(e))
        .map(|e| e.0.clone())
        .collect();
    added.sort();
    assert_eq!(
        added,
        vec![
            PathBuf::from(".claude/skills"),
            PathBuf::from("formation.toml")
        ]
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(project.join("formation.toml")).unwrap()).unwrap();
    assert_eq!(config["name"].as_str(), Some("Project"));
    let files = f.read(&a);
    let agents_md = files
        .files
        .iter()
        .find(|file| file.path == std::path::Path::new("AGENTS.md"))
        .unwrap();
    assert_eq!(agents_md.content, "# Project rules\r\nkeep\n");
}

#[test]
fn agents_adopt_adds_only_the_missing_instruction_files() {
    let f = Fixture::new();
    let project = f.root.join("notes");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("README.md"), "notes").unwrap();
    let a: Agent = serde_json::from_value(create_in(&f, "Notes", &project).unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(project.join("AGENTS.md")).unwrap(),
        "# Help customers\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
    assert!(project.join(".claude/skills").is_dir());
    assert_eq!(
        fs::read_to_string(project.join("README.md")).unwrap(),
        "notes"
    );
    assert_eq!(a.folder_path, project.canonicalize().unwrap());
}

#[test]
fn agents_adopt_refuses_another_agents_formation_toml_and_changes_nothing() {
    let f = Fixture::new();
    let project = f.root.join("taken");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("formation.toml"), "name = \"Someone\"\n").unwrap();
    fs::write(project.join("notes.txt"), "mine").unwrap();
    let before = snapshot(&project);
    assert_eq!(
        create_in(&f, "Taken", &project).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(snapshot(&project), before);
    assert!(f.store.agents().unwrap().is_empty());
}

#[test]
fn agents_adopt_refuses_a_folder_that_is_another_agents_in_the_store() {
    let f = Fixture::new();
    let a = f.create();
    // Even without its formation.toml, the folder is still the agent's.
    fs::remove_file(a.folder_path.join("formation.toml")).unwrap();
    let before = snapshot(&a.folder_path);
    assert_eq!(
        create_in(&f, "Second", &a.folder_path).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(snapshot(&a.folder_path), before);
    assert_eq!(f.store.agents().unwrap(), vec![a]);
}

#[test]
fn agents_store_repositories_as_absolute_paths() {
    let f = Fixture::new();
    let mut params = create_params();
    params["repositories"] = json!(["/code/api", "../shared"]);
    let a: Agent = serde_json::from_value(f.call("agent_create", params).unwrap()).unwrap();
    assert_eq!(
        a.repositories,
        vec![PathBuf::from("/code/api"), a.folder_path.join("../shared")]
    );
    let updated: Agent = serde_json::from_value(
        f.call(
            "agent_update",
            json!({"agent_id": a.id, "name": null, "role": null, "avatar": null,
                "repositories": ["web", "/code/web"]}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        updated.repositories,
        vec![a.folder_path.join("web"), PathBuf::from("/code/web")]
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(a.folder_path.join("formation.toml")).unwrap()).unwrap();
    assert_eq!(
        config["repositories"],
        toml::Value::Array(vec![
            a.folder_path.join("web").display().to_string().into(),
            "/code/web".into(),
        ])
    );
    assert_eq!(f.store.agent(a.id).unwrap(), Some(updated));
}

#[test]
fn agents_update_metadata_and_store_preserves_folder_and_slug() {
    let f = Fixture::new();
    let a = f.create();
    let updated: Agent = serde_json::from_value(f.call("agent_update", json!({"agent_id": a.id,
        "name": "Support \"team\"", "role": "Support\nlead", "avatar": {"shape": "square", "color": "red"},
        "repositories": []})).unwrap()).unwrap();
    assert_eq!(updated.folder_path, a.folder_path);
    assert_eq!(updated.slug, a.slug);
    assert_ne!(updated.files_revision, a.files_revision);
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(a.folder_path.join("formation.toml")).unwrap()).unwrap();
    assert_eq!(config["name"].as_str(), Some("Support \"team\""));
    assert_eq!(config["role"].as_str(), Some("Support\nlead"));
    assert_eq!(config["avatar"]["color"].as_str(), Some("red"));
    assert_eq!(f.store.agent(a.id).unwrap(), Some(updated));
}

#[test]
fn agents_files_roundtrip_add_edit_remove_and_revision_conflict() {
    let f = Fixture::new();
    let a = f.create();
    let mut files = f.read(&a);
    for (path, content) in [
        ("AGENTS.md", Some("New instructions")),
        (".claude/skills/review/SKILL.md", Some("# Review")),
        (".claude/skills/review/SKILL.md", Some("# Updated")),
        (".claude/skills/review/SKILL.md", None),
    ] {
        let old = files.revision.clone();
        files = serde_json::from_value(
            f.call(
                "agent_files_write",
                json!({"agent_id": a.id, "path": path,
            "content": content, "base_revision": old}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_ne!(files.revision, old);
        assert_eq!(f.read(&a), files);
        assert_eq!(
            f.store.agent(a.id).unwrap().unwrap().files_revision,
            files.revision
        );
        assert_eq!(
            files
                .files
                .iter()
                .find(|v| v.path == std::path::Path::new(path))
                .map(|v| v.content.as_str()),
            content
        );
    }
    assert_eq!(
        f.call(
            "agent_files_write",
            json!({"agent_id": a.id, "path": "AGENTS.md", "content": "stale",
        "base_revision": a.files_revision})
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
}

#[test]
fn agents_files_escape_and_noneditable_paths_rejected() {
    let f = Fixture::new();
    let a = f.create();
    for path in [
        "../outside",
        "/tmp/outside",
        ".claude/skills/../../../outside",
        "CLAUDE.md",
        "formation.toml",
        ".claude/skills",
        "AGENTS.md/child",
    ] {
        assert_eq!(
            f.call(
                "agent_files_write",
                json!({"agent_id": a.id, "path": path,
            "content": "bad", "base_revision": f.read(&a).revision})
            )
            .unwrap_err()
            .code,
            ErrorCode::Denied,
            "{path}"
        );
    }
    assert_eq!(
        f.call(
            "agent_files_write",
            json!({"agent_id": a.id, "path": "AGENTS.md",
        "content": null, "base_revision": f.read(&a).revision})
        )
        .unwrap_err()
        .code,
        ErrorCode::Denied
    );
}

#[test]
fn agents_outside_edit_emits_event_and_stale_save_conflicts() {
    let f = Fixture::new();
    let a = f.create();
    let before = f.events.events().len();
    fs::write(a.folder_path.join("AGENTS.md"), "Outside edit").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.events.events().len() == before && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let events = f.events.events();
    assert!(
        matches!(&events.last().unwrap().1, Event::AgentUpdate(formation_proto::agent::AgentUpdateEvent::Upsert { agent }) if agent.files_revision != a.files_revision)
    );
    assert!(events.len() > before);
    assert_eq!(
        f.call(
            "agent_files_write",
            json!({"agent_id": a.id, "path": "AGENTS.md",
        "content": "overwrite", "base_revision": a.files_revision})
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(f.read(&a).files[0].content, "Outside edit");
}

#[test]
fn agents_handler_registration_invalid_params_and_unsupported_harness() {
    let f = Fixture::new();
    assert_eq!(
        f.handler.methods(),
        vec![
            "agents_list",
            "agent_create",
            "agent_update",
            "agent_files_read",
            "agent_files_write",
            "harness_check"
        ]
    );
    for method in [
        "agent_create",
        "agent_update",
        "agent_files_read",
        "agent_files_write",
    ] {
        assert_eq!(
            f.call(method, json!({})).unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }
    let mut params = create_params();
    params["harness"] = json!("detected");
    assert_eq!(
        f.call("agent_create", params).unwrap_err().code,
        ErrorCode::HarnessUnsupported
    );
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
}

#[test]
fn agents_claude_readiness_reports_install_and_sign_in_state() {
    use formation_core::agents::ClaudeReadiness;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let binary = f.root.join("claude-test");
    let probe = ClaudeReadiness::new(binary.clone());
    let missing = probe.check().unwrap();
    assert!(!missing[0].installed);
    assert_eq!(missing[0].signed_in, None);
    assert!(missing[0].problem.is_some());
    for (output, code, expected) in [
        (r#"{"loggedIn":true}"#, 0, Some(true)),
        (r#"{"loggedIn":false}"#, 1, Some(false)),
    ] {
        fs::write(&binary, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.319 (Claude Code)'; exit 0; fi\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 2\nprintf '%s\\n' '{output}'\nexit {code}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let status = probe.check().unwrap().remove(0);
        assert!(status.installed);
        assert_eq!(status.path, Some(binary.canonicalize().unwrap()));
        assert_eq!(status.version.as_deref(), Some("2.1.319 (Claude Code)"));
        assert_eq!(status.signed_in, expected);
        assert_eq!(status.problem.is_none(), expected == Some(true));
    }
}
