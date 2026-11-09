-- B's team tables (TASK-017). Version 2000, in B's range 2000-2999.
-- Written for A's migration runner (TASK-011): it applies each version once, in
-- order, inside its own transaction, so this file has no transaction or PRAGMA
-- statements. IDs are UUIDv7 strings; times are UTC RFC 3339 strings.
-- No foreign keys: the repositories accept rows in any order (a member's Mac stores
-- a roster whose invitations' creators it may not hold), as `MemoryCollabStore` does.

-- A team member. Live presence is the peer module's; `online` is the last value the
-- team service wrote, kept so a stored member reads back as written.
CREATE TABLE member (
    id TEXT PRIMARY KEY NOT NULL,
    github_login TEXT NOT NULL,
    display_name TEXT NOT NULL,
    -- Tailscale WhoIs: UserProfile.LoginName and Node.StableID. Both must match.
    tailscale_login TEXT NOT NULL,
    tailscale_node TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('creator', 'member')),
    joined_at TEXT NOT NULL,
    online INTEGER NOT NULL DEFAULT 0 CHECK (online IN (0, 1)),
    last_seen_at TEXT,
    -- Set once; a removed member keeps the row for names in history.
    removed_at TEXT
) STRICT;

-- Unique among current members only, so a removed member can join again.
CREATE UNIQUE INDEX member_github_login_current
    ON member (github_login COLLATE NOCASE) WHERE removed_at IS NULL;
CREATE UNIQUE INDEX member_tailscale_login_current
    ON member (tailscale_login) WHERE removed_at IS NULL;

-- Invitations made on the creator's Mac. Single use; 24 hours.
CREATE TABLE invitation (
    id TEXT PRIMARY KEY NOT NULL,
    code TEXT NOT NULL UNIQUE,
    link TEXT NOT NULL,
    created_by TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_by TEXT,
    used_at TEXT,
    CHECK ((used_by IS NULL) = (used_at IS NULL))
) STRICT;

-- The one team this Mac belongs to. No row until it creates or joins one, and
-- none again after it is removed.
CREATE TABLE team (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    id TEXT NOT NULL,
    -- The creator's peer address, from the invitation link (its own on the creator's Mac).
    creator_host TEXT NOT NULL,
    creator_port INTEGER NOT NULL CHECK (creator_port BETWEEN 1 AND 65535),
    -- Only the creator's Mac increases it; members hold the last applied version.
    roster_version INTEGER NOT NULL DEFAULT 0 CHECK (roster_version >= 0),
    open_invitation_code TEXT
) STRICT;

-- This Mac's own member (`TeamRepo::me`). Written before the team row, which
-- marks the team as created or joined.
CREATE TABLE team_me (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    member_id TEXT NOT NULL
) STRICT;
