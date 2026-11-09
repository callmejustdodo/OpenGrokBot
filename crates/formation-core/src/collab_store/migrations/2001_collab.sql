-- B's sharing, ask, request, and policy tables. Version 2001, in B's range
-- 2000-2999, applied by A's migration runner after 2000 (team). No transaction or
-- PRAGMA statements. IDs are UUIDv7 strings; times are UTC RFC 3339 strings with
-- fixed nanosecond precision, so text order is time order. Nested values that are
-- never queried (targets, answers, errors, chains, tool calls, rule scopes) are
-- JSON text. No foreign keys, as in 2000. Lists are ordered by their time column,
-- then rowid (insertion order); an upsert keeps the rowid.

-- Sharing per agent. No row means private (`ShareSettings::private`).
CREATE TABLE agent_share (
    agent_id TEXT PRIMARY KEY NOT NULL,
    shared INTEGER NOT NULL CHECK (shared IN (0, 1)),
    visibility TEXT NOT NULL,
    accepts_requests INTEGER NOT NULL CHECK (accepts_requests IN (0, 1))
) STRICT;

-- Questions this Mac asked or received (each Mac keeps its own copy).
CREATE TABLE question (
    id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL,
    asker TEXT NOT NULL,
    target TEXT NOT NULL,      -- JSON AskTarget
    text TEXT NOT NULL,
    status TEXT NOT NULL,
    candidates TEXT NOT NULL,  -- JSON RoutingCandidate[]
    agent_id TEXT,
    answer TEXT,               -- JSON Answer
    error TEXT,                -- JSON ApiError
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
CREATE INDEX question_order ON question (created_at);

-- The forked session answering a thread (owner's Mac only; never sent to peers).
-- Outlives the question row, like A's fork sessions.
CREATE TABLE question_fork_session (
    thread_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL
) STRICT;

-- Work requests, on the requester's and the owner's Mac.
CREATE TABLE work_request (
    id TEXT PRIMARY KEY NOT NULL,
    requester TEXT NOT NULL,
    requester_agent TEXT,
    chain TEXT NOT NULL,       -- JSON Chain
    owner TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    status TEXT NOT NULL,
    result TEXT,
    error TEXT,                -- JSON ApiError
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (requester, idempotency_key)
) STRICT;
CREATE INDEX work_request_order ON work_request (created_at);

-- Each agent's queued messages, kept so a restart re-queues them.
CREATE TABLE queue_item (
    id TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL,
    source TEXT NOT NULL,      -- JSON QueueSource
    text TEXT NOT NULL,
    queued_at TEXT NOT NULL
) STRICT;
CREATE INDEX queue_item_order ON queue_item (agent_id, queued_at);

-- Tool calls waiting for, or decided by, the owner.
CREATE TABLE approval (
    id TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL,
    request_id TEXT,
    requester TEXT,
    requester_agent TEXT,
    purpose TEXT,
    call TEXT NOT NULL,        -- JSON ToolCall
    summary TEXT NOT NULL,
    proposed_rule TEXT,        -- JSON RuleDraft
    status TEXT NOT NULL,
    choice TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT,
    expires_at TEXT NOT NULL
) STRICT;
CREATE INDEX approval_order ON approval (status, created_at);

-- "Keep allowing similar" rules.
CREATE TABLE rule (
    id TEXT PRIMARY KEY NOT NULL,
    requester TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    scope TEXT NOT NULL,       -- JSON RuleScope
    expires_at TEXT,
    max_uses INTEGER CHECK (max_uses >= 0),
    uses INTEGER NOT NULL CHECK (uses >= 0),
    created_at TEXT NOT NULL,
    revoked_at TEXT
) STRICT;
CREATE INDEX rule_order ON rule (agent_id, created_at);
