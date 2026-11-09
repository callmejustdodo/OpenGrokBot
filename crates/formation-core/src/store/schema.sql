CREATE TABLE agents (
    id TEXT PRIMARY KEY NOT NULL, slug TEXT NOT NULL, name TEXT NOT NULL,
    role TEXT NOT NULL, harness TEXT NOT NULL, folder_path TEXT NOT NULL,
    repositories TEXT NOT NULL, avatar_shape TEXT NOT NULL, avatar_color TEXT NOT NULL,
    state TEXT NOT NULL, files_revision TEXT NOT NULL, created_at TEXT NOT NULL,
    last_activity_at TEXT, detected TEXT, deleted_at TEXT
);
CREATE UNIQUE INDEX agents_live_slug ON agents(slug) WHERE deleted_at IS NULL;
CREATE TABLE main_sessions (
    agent_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, transcript_path TEXT NOT NULL,
    cwd TEXT NOT NULL, started_at TEXT NOT NULL, last_activity_at TEXT
);
CREATE TABLE fork_sessions (
    run_id TEXT PRIMARY KEY NOT NULL, agent_id TEXT NOT NULL, session_id TEXT NOT NULL,
    transcript_path TEXT NOT NULL, cwd TEXT NOT NULL, started_at TEXT NOT NULL, last_activity_at TEXT
);
CREATE TABLE schedules (
    id TEXT PRIMARY KEY NOT NULL, agent_id TEXT NOT NULL, prompt TEXT NOT NULL,
    rule TEXT NOT NULL, time TEXT NOT NULL, paused INTEGER NOT NULL,
    next_run_at TEXT, last_run TEXT
);
CREATE TABLE schedule_runs (
    id TEXT PRIMARY KEY NOT NULL, schedule_id TEXT NOT NULL, planned_at TEXT NOT NULL,
    started_at TEXT, finished_at TEXT, result TEXT
);
CREATE INDEX schedule_runs_page ON schedule_runs(schedule_id, planned_at DESC);
CREATE TABLE attention (
    id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, target TEXT NOT NULL,
    subject TEXT NOT NULL, at TEXT NOT NULL, resolved_at TEXT
);
CREATE INDEX attention_open ON attention(at) WHERE resolved_at IS NULL;
CREATE TABLE activity (
    id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL, actor TEXT, agent_id TEXT,
    target TEXT, at TEXT NOT NULL, summary TEXT NOT NULL
);
CREATE INDEX activity_page ON activity(at DESC);
CREATE TABLE settings (id INTEGER PRIMARY KEY CHECK(id = 1), language TEXT, notifications TEXT NOT NULL);
