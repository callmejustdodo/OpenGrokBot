# Formation — Technical specification

Rev 3 · 2026-11-09 · Based on the confirmed [product requirements](1_product_requirements_documentation.md) (Rev 2), [user stories](2_user_stories.md) (Rev 2), and [screens and flows](3_screen_list_and_user_flows.md) (Rev 3) with the approved [mockup M3](mockups/index.html). Decision IDs refer to the [planning progress](0_one_pager.md#planning-progress) record. Rev 2 sets the minimum macOS version to 13 (D-047). Rev 3 moves the core out of the app into a background service, because agents are harness sessions that outlive the app (D-048).

## Context and constraints

- **Product:** a macOS app that runs role agents on Claude Code and Codex, and lets a team of four ask each other's agents (one-pager).
- **Existing code:** none. The repository holds planning documents, the mockup, five macOS-related skills under `.claude/skills/`, and repository conventions (`AGENTS.md`, issue and PR templates, a documentation link check).
- **Builders:** three developers and one designer, each working through a coding agent; same-day first use is the goal (D-020). Running cost has no limit.
- **Integration:** Formation launches and talks to the Claude Code and Codex CLI processes directly; it does not use the Claude Agent SDK (D-043). Claude Code comes first (D-005).
- **Team network for the MVP:** a Tailscale private network (D-045).
- **Agents outlive the app:** an agent is a Claude Code or Codex session; the app is only an interface to it. Quitting the app never stops agents (D-048).
- **Build first:** Formation is built as its own app; extending existing products may come later (D-044, reading not yet confirmed).

## Technology choices

| Area | Choice | Rationale | Status |
| --- | --- | --- | --- |
| App framework | **Tauri 2** | The approved mockup is HTML and CSS and carries over; the designer can work in CSS through an agent; one Rust core serves the app, CLI, and MCP server; tray, notifications, and launch at login are supported on macOS; much lighter than Electron. Swift was the more native alternative. | Chosen by the user, 2026-11-09 (D-041) |
| Background service | `formationd`, a Rust binary bundled in the app (`Contents/MacOS/formationd`, next to the app's executable) and registered by the app as a per-user launchd LaunchAgent: the app writes `~/Library/LaunchAgents/<app identifier>.formationd.plist` with `RunAtLoad` and `KeepAlive`, and bootstraps it in the `gui/<uid>` domain with `launchctl`. It re-registers only when the plist changes or the service is not loaded, so opening the app never restarts a running formationd. SMAppService was the alternative; it needs the app bundle, and the team runs unbundled dev builds. formationd starts at login and keeps running when the app quits. | Harness sessions need a parent that holds their stdin and stdout and keeps answering teammates while the app is closed (D-048). | Registration built in TASK-009 |
| Core language | Rust (stable, edition 2021 or later), tokio | Process supervision, JSON streaming, networking, and the policy engine in one place; shared by app, CLI, and MCP server. | Follows from Tauri |
| UI | React with TypeScript, Vite | Most familiar to coding agents; maps the mockup's components directly. | Proposed |
| UI foundation | Plain CSS with the approved design tokens as CSS variables, plus [Radix UI](https://www.radix-ui.com/) primitives for dialogs, menus, popovers, switches, and tabs | Keeps the mockup's styling as is and gives accessible behavior (focus, keyboard, screen readers) without a second visual system. shadcn/ui was considered; it would add Tailwind on top of tokens the designer already has in CSS. | Proposed; confirm in review |
| Local storage | SQLite (via `rusqlite` or `sqlx`) in `~/Library/Application Support/Formation/` | Questions, requests, rules, and activity are kept until deleted (D-029); a single local file per Mac. | Proposed |
| Peer transport | Plain HTTP and WebSocket over the tailnet, JSON messages | Tailscale already encrypts and authenticates the link (WireGuard), so the MVP adds no TLS layer of its own. Built on tokio's mainstream stack with TLS and proxy features off: `axum` for the server and its WebSocket upgrade, `reqwest` for calls, `tokio-tungstenite` (the WebSocket library under `axum`) for the events connection (TASK-018). | Proposed (D-045) |
| Identity | Tailscale WhoIs for "which machine and Tailscale user is calling," bound at join time to the member's GitHub account (D-046) | WhoIs answers come from the WireGuard peer and cannot be forged by headers; GitHub gives each member a stable, recognizable identity. | Proposed |
| MCP server | Official Rust SDK [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk), stdio transport | Same crate set as the core. | Proposed |
| Localization | `i18next` with Korean and English bundles; follows the macOS language, overridable in Settings (D-033) | | Proposed |
| Agent avatars | [`open-bot-avatar`](https://github.com/DominikScholz/open-bot-avatar) package (MIT): definitions plus its renderer, including live motion and moods (D-042). OpenAvatar (3D) is deferred. | The mockup already uses its shapes; the package renders volume, gaze, and blinking. | Proposed |
| Minimum macOS | macOS 13 Ventura | Chosen by the user; within Tauri 2's supported range. Test on macOS 13 and the newest release. | Chosen by the user, 2026-11-09 (D-047) |
| Distribution (MVP) | Each member builds from source (`pnpm tauri build`) or runs the dev build | No signing account needed today. | Proposed; Developer ID signing, notarization, and an updater come before wider release |

### Starter

| Candidate | Source | Checked | Use | License | Decision |
| --- | --- | --- | --- | --- | --- |
| `create-tauri-app`, React + TypeScript template | [Tauri docs: Create a Project](https://v2.tauri.app/start/create-project/) | 2026-11-09 (page updated 2026-11-05) | Official scaffold for a Tauri 2 app with a React + TypeScript frontend | MIT or Apache-2.0 | **Selected.** Run `pnpm create tauri-app`, choose TypeScript, React, TypeScript. Verify at scaffold time that the template uses Vite (the docs page does not say). |
| No starter (manual setup) | Same page, manual setup section | 2026-11-09 | — | — | Not needed; the official scaffold is small. |

Adaptation: add the Rust workspace crates listed under [Code layout](#code-layout), the M3 tokens as CSS variables, Radix primitives, i18next, and `open-bot-avatar`. Remove the template's sample greet command.

## System overview

```mermaid
flowchart LR
  subgraph Mac["Owner's Mac"]
    UI["Tauri app: UI only (React)"] <-- "local socket: commands + events" --> Core["formationd: background service (formation-core)"]
    Core <--> DB[("SQLite")]
    Core -- "spawn, stdin/stdout stream-json" --> CC["claude -p (per agent)"]
    Core -- "spawn, JSON-RPC (later)" --> CX["codex app-server"]
    Core -- "reads" --> TR["$CLAUDE_CONFIG_DIR/projects/ transcripts"]
    CC -- "permission prompts (MCP)" --> MCPp["formation mcp (permission tool)"]
    MCPp <--> Core
    CLI["formation CLI"] <--> Core
    MCPt["formation mcp (teammate tools)"] <--> Core
  end
  Core <-- "HTTP + WebSocket over Tailscale" --> Peer["Teammate's Mac: formationd"]
```

- **formationd** runs formation-core as a background service. It owns the harness processes, policy, the request queue, the peer server, and storage. "The core" below means formationd.
- **The Tauri app is an interface.** It connects to formationd over a local socket, shows its state, and sends commands. Quitting the app (⌘Q) leaves formationd and every agent session running; teammates can still ask. Stopping an agent is always an explicit action (FR-003). Stopping formationd itself stops every agent, so it is a CLI action (`formation daemon stop`, which asks for confirmation); the first version adds no screen for it, keeping the approved screens unchanged.
- **Alerts while the app is closed:** formationd keeps needs-attention items; when one arrives and the app is not running, formationd opens the app hidden in the menu bar so the macOS notification can show (proposed; verify).
- **The CLI and MCP server** are one `formation` binary that talks to formationd over the same local socket. If formationd is not running, the CLI starts it through launchd, or reports that it cannot.
- **Answers to teammates are produced on the owner's Mac,** in a side conversation forked from the agent's session (see [Ask pipeline](#ask-pipeline)).

## Claude Code integration (first harness)

Flags below follow the [Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference) (checked 2026-11-09), with process-control and permission-routing results from TASK-004 (#13) and TASK-005 (#10).

| Need | How | PRD |
| --- | --- | --- |
| Run an agent and chat with it | One long-lived process per running agent: `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --replay-user-messages --session-id <uuid>` started in the agent's folder. The core writes user messages as JSON lines to stdin and reads events from stdout. | FR-003, FR-022 |
| Queue while working | formationd holds queued messages itself so they can be edited or withdrawn, then sends each with `"priority":"later"` to start a later turn. Without `priority`, a message joins the running turn (TASK-004). The UI shows it as queued until the replayed user message arrives. | FR-022 |
| Steer now | Send a default-priority message; it is applied at the next tool boundary (TASK-004). | FR-022 |
| Stop | Send the stdin `control_request` interrupt to stop the turn. SIGINT ends the process (TASK-004). | FR-003 |
| Restart and continue | `claude -p ... --resume <session-id>`. | FR-003 |
| Owner's own permission prompts | `--permission-prompt-tool mcp__formation__permission` with `--mcp-config` pointing at `formation mcp --permission`. The tool forwards each prompt to the core, which shows it in the conversation (proposed default 11) and returns the owner's answer. | FR-022 |
| Teammates' work requests | Same permission tool. Requests from teammates run in forks with a per-request policy context; bind the requester and request ID when each separate fork process starts (D-056). File isolation and result delivery are [open](#open-decisions). formationd passes `FORMATION_RUN_ID` to each run, permission server and hook; the permission tool has no session ID in its input. The core answers prompts from saved rules and the always-allowed reads (D-028), otherwise it raises an approval (SC-011). | FR-014, FR-015 |
| Required policy hook | Inject a required `PreToolUse` hook through `--settings` that asks the core before every tool call (NFR-004). In acceptEdits, auto, and bypassPermissions, writes, commands, and network calls can skip the permission tool; the hook sees every call, and a hook deny wins (TASK-005). | NFR-004 |
| Agent instructions and skills | The agent folder's `AGENTS.md` is canonical; `CLAUDE.md` contains one line importing it (`@AGENTS.md`), and skills live in `.claude/skills/`. Codex reads `AGENTS.md` natively. | FR-001, FR-004 |
| Read a session conversation | Transcript files (`.jsonl`) under `$CLAUDE_CONFIG_DIR/projects/` (default `~/.claude/projects/`); `--resume` also accepts a transcript path. | FR-012 |
| Detect sessions started elsewhere | Read detected sessions and work only in forks; never resume or write to the original (FR-006). Scan transcript folders for recent activity. Live-process detection (for example `~/.claude/sessions/<pid>.json`, used by Reason Network) is undocumented; treat as best effort and label states "unknown" when unsure. Skip sessions Formation created as forks: every fork writes a full transcript copy (`<fork-id>.jsonl`) into the same project folder (TASK-006). | FR-006 |

**Codex (second harness):** launch `codex app-server` and speak its JSON-RPC protocol (thread, turn, and approval requests), per the [Codex App Server docs](https://learn.chatgpt.com/docs/app-server) referenced in the product plan. Verify the protocol and approval hooks before building it.

## Ask pipeline

1. A question arrives (UI, CLI, MCP, or a peer) with the asker's identity, target (person or agent), and question text.
2. **Access check** on the owner's Mac: the asker is a team member; the target agent is shared (D-026); otherwise reply `agent_not_shared`.
3. **Routing** (person-level only, FR-010): score each shared agent from its role, the first lines of `AGENTS.md`, and recent session text. The scoring step is itself a short Claude Code call with tools disabled (`--tools ""`) and the same isolation flags as step 4; it resumes no session and is not saved (`--no-session-persistence`). If the top score is below a threshold (assumed 0.6 of 1) or within 0.15 of the second, return the choice prompt (D-027) with up to five candidates, best first. Candidate reasons reach the asker, so they are masked like answers (step 5). If scoring fails, the asker gets the choice prompt.
4. **Answer:** in the agent's folder, run `claude -p --resume <agent-session> --fork-session --tools "" --safe-mode --setting-sources "" --strict-mcp-config --disable-slash-commands --system-prompt-snapshot off --append-system-prompt <answer prompt> --output-format stream-json`. The answer prompt requires: answer only from the conversation, say what the conversation does not establish, and list the messages used. The question goes on stdin after a short lead-in, never in argv, and the parent's `CLAUDE*` environment variables are cleared. If the CLI's `init` event lists any tool, stop the run. The fork never alters the original session, and with no tools it cannot read or write files (NFR-002, D-024).
   - The isolation flags keep the agent folder's `CLAUDE.md`, the owner's hooks and MCP servers, and slash commands out of the fork. Without them, `CLAUDE.md` shows up in answers, hooks run on every fork, a question starting with `/` runs as a command, and the answer prompt is ignored on resume. `--bare` is not an option: it fails under a subscription login. Measured with claude 2.1.319 in TASK-006.
   - Content already recorded in the transcript, such as hook output and the owner's account email, still reaches the fork, so answers are masked (step 5). A fork taken during a running turn sees the owner's newest message as unanswered, so the question then carries a note that the agent is still working on it. Each thread records its newest fork; follow-ups resume it, a fork replaced by a follow-up, a refresh, or another agent is deleted, and the last one is deleted with the thread (TASK-019).
5. **Post-processing:** turn the answer's list of messages used into excerpts (user and assistant messages only, never tool output) and remove the list from the answer text; attach `as_of` (timestamp of the newest transcript entry used, or of the newest entry the fork saw when the answer cites none), mark `possibly_stale` if older than 1 hour, keep excerpts only if the agent's visibility allows (D-025), and mask secret-like strings and the owner's email addresses in the answer and excerpts (NFR-003).
6. Reply to the asker; store the question and answer on both Macs (D-029). Follow-ups continue the forked session. A refresh answers again from a new fork of the main session, with the thread's earlier questions restated.

## Data model

Stored in SQLite on each member's Mac. Times are UTC in storage and shown in local time. IDs are UUIDv7 strings. The member, invitation, team, sharing, question, request, queue, approval and rule tables are B's migrations 2000–2999 (`crates/formation-core/src/collab_store/`); nested values that are never queried are JSON text.

| Entity | Key fields | Rules |
| --- | --- | --- |
| `member` | id, github_login, display_name, tailscale_login, tailscale_node (the node's `StableID`), joined_at, role (`creator` or `member`), online (last value written; live presence is the peer module's), last_seen_at, removed_at | Unique `github_login` and `tailscale_login` among current members; removal ends access at once (FR-008). |
| `invitation` | id, code, link, created_by, expires_at, used_by, used_at | Single use; expires 24 hours after it is made; single team. |
| `team` | id, creator_host, creator_port, roster_version, open invitation; this Mac's own member | One per Mac; see [team and roster](#team-and-roster). |
| `agent` | id, slug, name, role, harness (`claude_code`, `codex`, `detected`), folder_path, avatar_shape, avatar_color, created_at, deleted_at | Unique slug per Mac; detected agents record their harness and source path. |
| `agent_share` | agent_id, shared, visibility (`answer`, `answer_excerpts`), accepts_requests | Defaults: not shared, `answer_excerpts`, false (D-025, D-026, FR-009). |
| `session` | id (harness session ID), agent_id, started_at, last_activity_at, state | One main session per running agent. |
| `schedule` | id, agent_id, prompt, rule (daily, weekdays, weekly, every N hours), time, paused | Missed runs run once on wake (FR-005). |
| `schedule_run` | id, schedule_id, planned_at, started_at, finished_at, result (`completed`, `failed`, `missed_ran`) | |
| `question` | id, thread_id, asker, target (JSON: person or agent), text, status, candidates (JSON), agent_id, answer (JSON: id, agent_id, text, excerpts, as_of, possibly_stale, answered_at), error, created_at, updated_at | Status: received, answering, unsure, waiting_offline, sent_to_owner, answered, canceled, failed. Answer excerpts are empty when visibility is `answer`. |
| `question_fork_session` | thread_id, session_id | The forked session answering a thread; owner's Mac only, never sent to peers. |
| `work_request` | id, requester, requester_agent, chain (JSON: id, depth), owner, agent_id, prompt, idempotency_key, status, result, error, created_at, updated_at | Unique (requester, idempotency_key); status per FR-014. |
| `queue_item` | id, agent_id, source (JSON: owner, request, or schedule), text, queued_at | Each agent's queue in arrival order, kept so a restart re-queues it. |
| `approval` | id, agent_id, request_id (nullable for the owner's own work), requester, requester_agent, purpose, call (JSON: tool and input), summary, proposed_rule (JSON), status, choice, created_at, decided_at, expires_at | Expires after 24 hours (FR-015). |
| `rule` | id, requester, agent_id, scope (JSON: all, or program, arg_pattern, folder), expires_at, max_uses, uses, created_at, revoked_at | Never matches command chaining or another folder (FR-015). |
| `activity` | id, kind, actor, agent_id, ref_id, at, summary | Local record (FR-020). |

**Invariants:** a private agent never appears in any peer response; approval decisions are made only by the owner's core; a request with a repeated idempotency key returns the original; each agent's main session accepts one turn at a time, in arrival order.

## Interfaces

### UI ↔ formationd

The Tauri app's Rust side is a thin client: it forwards each command to formationd as newline-delimited JSON-RPC 2.0 over the local socket and relays formationd's events to the React UI. On connect, the UI asks for a snapshot and then applies events, so it can close and reopen at any time without losing state.

The exact names and shapes live in `crates/formation-proto` (TASK-002): each command's params type implements `Command`, which binds its method name and result type, and the `Event` enum lists every event.

- **Commands** (request and response): `snapshot`, `agents_list`, `agent_create`, `agent_update`, `agent_start`, `agent_stop`, `agents_detect`, `agent_connect`, `agent_files_read`, `agent_files_write`, `harness_check`, `conversation_page`, `agent_message_send`, `agent_message_steer`, `agent_message_edit`, `agent_turn_interrupt`, `agent_message_withdraw`, `schedule_*`, `share_update`, `share_preview`, `teammate_agents`, `ask`, `question_*`, `request_create`, `request_get`, `request_cancel`, `approval_decide`, `rules_*`, `team_*`, `history_*`, `settings_update`, and `permission_decide` (used by `formation mcp --permission` and the `PreToolUse` hook).
- **Events** (core → UI), sent as `event` notifications with a sequence number: `agent.state`, `agent.stream` (partial text and steps, with a per-agent stream sequence), `agent.queue`, `agent.update`, `schedule.update`, `harness.update`, `notify`, `approval.raised`, `approval.update`, `question.update`, `request.update`, `share.update`, `rule.update`, `team.update`, `peer.presence`, `settings.update`. Every part of the snapshot has an event that keeps it current.

### Peer protocol (over the tailnet)

The core listens on the Mac's Tailscale address only, port 7413 (assumed), versioned under `/v1`.

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/agents` | The caller's view of this member's shared agents (name, role, harness, state, last activity, accepts_requests). |
| `POST /v1/ask` | Ask a person or agent; returns a question ID immediately. The asker's Mac picks the ID, so outbox retries are idempotent. |
| `POST /v1/questions/{id}/action` | The asker cancels, refreshes, or routes a question ("Ask another agent", "Ask Minsu"). |
| `POST /v1/requests` | Create a work request with an idempotency key. |
| `GET /v1/requests/{id}` · `POST /v1/requests/{id}/cancel` | Status and cancel. |
| `GET /v1/team` | The roster and its version; the creator's Mac is authoritative ([team and roster](#team-and-roster)). |
| `POST /v1/team/join` | Creator's Mac only: join with an invitation code and a GitHub login. |
| `WS /v1/events` | Presence, question, request, and roster updates for this caller. |

Every request is authenticated by Tailscale WhoIs on the connection and mapped to a member; unknown callers get `403`, except `POST /v1/team/join`, which still needs WhoIs but checks the invitation code instead of membership. Errors use `{ "error": { "code", "message" } }`.

#### Team and roster

Decided 2026-11-09 (TASK-017); identity checks are in the backend architecture's [identity and team](6_backend_architecture.md#identity-and-team).

- **Roster:** the creator's Mac holds the authoritative roster, with a version that starts at 1 and goes up by one on every join or removal. Other members' Macs fetch it with `GET /v1/team`, get a `roster` push on `WS /v1/events` when it changes, and fetch it again on reconnect; a Mac applies a roster only when its version is newer. While the creator's Mac is offline, nobody can join and members keep the roster they hold.
- **Removal applies first:** the creator's Mac stores the removal before anything else and closes the member's connections; every other Mac refuses the member as soon as it applies the new roster. A removed member's Mac forgets the team.
- **Invitation link:** `formation://join/<code>?host=<MagicDNS name or 100.x>&port=<peer port>&team=<team id>`. The code, such as `7F3K-Q9PX` (8 symbols of Crockford base32), works once and expires 24 hours after it is made; a new invitation replaces the open one.
- **Joining:** the invitee's formationd sends the code, the GitHub login, and a display name to the host in the link. The new member's Tailscale login and node come from WhoIs on that connection, never from the body. Unknown, used, or expired codes return `not_found` and count as bad attempts, which are rate-limited per Tailscale login and for all callers together (assumed 5 and 20 per 10 minutes).

- **Binding and callers:** the server refuses to bind anything but a Tailscale address (100.64.0.0/10, fd7a:115c:a1e0::/48) or loopback (tests and development), never `0.0.0.0`. The caller comes from the TCP peer address only, never a header. Calls whose WhoIs maps to this Mac's own member are refused too, since WhoIs names a machine and another process or OS user on the Mac would otherwise pass as a teammate.
- **Statuses:** `POST /v1/ask` answers `202`; other successes `200`. Errors carry the code in the body; the status follows it: `403` for `not_member`, `agent_not_shared`, `requests_not_accepted`, and `denied`; `400` `invalid_params` (also for a body that does not decode); `404` `not_found`; `409` `conflict`; `410` `approval_expired`; `422` `harness_unsupported`; `429` `rate_limited`; `503` `peer_offline` and `harness_unavailable`; `500` `internal`. Clients act on the code.
- **Events connection:** `WS /v1/events` starts with a `presence` message from the owner's Mac (online, with the caller's view of its shared agents), sent again when its shared agents change, then re-sends the caller's questions and requests that are not final or changed while the caller was away. The server pings every 15 seconds. The asker's Mac counts the teammate online while the connection is open and reconnects with the outbox's backoff. A Mac speaks only for itself: the asker drops presence about another member, updates for another owner's questions or requests, and roster messages from anyone but the creator.

### CLI (`formation`) and MCP server

- CLI commands: `formation members`, `formation agents list --member <m>`, `formation ask <member>[/<agent>] "<question>"`, `formation request <member>/<agent> "<prompt>" --idempotency-key <k>`, `formation requests get|cancel <id>`; all accept `--json`. Waiting for approval returns immediately with the request ID and `awaiting_approval`; `--wait <seconds>` waits up to a timeout.
- Exit codes: 0 ok, 2 usage, 3 not shared or forbidden, 4 not found, 5 peer offline (request queued), 6 timeout, 1 other.
- MCP tools (`formation mcp`): `list_members`, `list_agents`, `ask`, `request_work`, `get_request`, `cancel_request`, with the same fields as the CLI JSON. A separate mode, `formation mcp --permission`, exposes only the `permission` tool used by Claude Code runs.
- Agent-to-agent safeguards: every call carries `chain_id` and `chain_depth`; depth over 3, or more than 30 requests per minute per caller, returns `rate_limited` (FR-019).

## Concurrency, ordering, and offline

- Only the owner writes to the main session (D-056); the core serializes its turns and queued owner messages. Teammate requests use separate fork processes after the current turn, in request order (FR-014).
- Questions are answered in parallel with the main session because forks are separate processes.
- When the owner's Mac is unreachable, the asker's core keeps the question or request in an outbox and retries when the peer reappears; the owner's core re-checks membership and sharing before acting (FR-016). The call returns `peer_offline` at once. Retries back off from 2 seconds to 60 seconds (assumed; jitter of 20% either way, capped after the jitter) and happen at once when the teammate's events connection reopens; a teammate's calls go out in the order they were queued, and nothing expires. A replay reuses the asker's `question_id` or `request_id`, so the owner acts once. A refusal on arrival (such as `agent_not_shared`) is final and reaches the asker as a failed `question_update` or `request_update`; only `rate_limited` within the chain-depth limit (the per-minute limit, hit when many waiting calls go out at once) keeps the call waiting for the next retry.
- Stalled detection: a working agent with no stdout event for 10 minutes raises an alert (FR-007).

## Security

- Provider credentials stay in each harness's own storage; Formation never reads or sends them.
- Peer access requires a tailnet connection plus a member record; the team creator manages the roster.
- Side conversations run with all tools disabled; answers, excerpts, and routing reasons pass secret masking (API keys, tokens, private keys, passwords in common formats, and the owner's email addresses) before leaving the Mac (NFR-003).
- The permission tool and the `PreToolUse` hook both consult the same policy; deny wins when they disagree.
- The local Unix socket is created with owner-only permissions.

## Code layout

```text
formation/
├── apps/desktop/              Tauri app: src-tauri (Rust), src (React UI)
├── crates/formation-core/     agents, harness adapters, policy, queue, storage, peer server
├── crates/formation-proto/    shared types: requests, events, errors (serde)
├── crates/formationd/         background service binary (LaunchAgent)
├── crates/formation-cli/      `formation` binary: CLI and MCP modes (rmcp)
└── docs/ (existing documentations/)
```

UI work (developer C and the designer) stays in `apps/desktop/src`; the core (developers A and B) stays in `crates/`; the two meet only through `formation-proto` types and the command/event names above.

## Testing

| Layer | What | How |
| --- | --- | --- |
| Policy engine, routing scores, rule matching, idempotency | Unit tests | `cargo test` |
| Claude Code adapter | Contract tests against recorded `stream-json` transcripts and a fake `claude` script that replays them | `cargo test -p formation-core` |
| Peer protocol | Two core instances on one machine over loopback with a stubbed identity provider | `cargo test` |
| UI | Component tests for list, conversation, approval sheet | Vitest with React Testing Library |
| Service lifecycle | Quit and reopen the app: sessions keep running and the UI reconnects to a correct snapshot; restart formationd: sessions continue with `--resume` | Integration test plus manual check |
| End to end | The two core journeys on two Macs on the tailnet: chat with my agent; ask a teammate's agent | Manual checklist in the implementation plan |

## Assumptions to verify early

| Assumption | Why it matters | Verification |
| --- | --- | --- |
| Messages can steer a running turn in print mode | FR-022 "Steer now" | Proven in TASK-004: see [Claude Code integration](#claude-code-integration-first-harness) |
| Every tool call that needs approval reaches the permission tool or the hook | NFR-004 | Proven in TASK-005: the [required policy hook](#claude-code-integration-first-harness) covers calls that skip the permission tool |
| Transcript format and location are stable enough to read | FR-012, FR-006 | Parse current transcripts; pin the Claude Code version in the team's setup |
| Tailscale WhoIs is reachable from Rust | Peer identity | Call the `tailscale whois --json` CLI or the LocalAPI socket; the Rust bindings lack WhoIs ([tailscale-rs #478](https://github.com/tailscale/tailscale-rs/issues/478)) |
| formationd can be registered as a LaunchAgent from the app on macOS 13, and can open the app hidden for notifications | D-048, FR-007 | Plist plus `launchctl` chosen (TASK-009). On macOS 27.2, formationd survived quitting the app, and launchd restarted it after a kill. Still to check: macOS 13, and formationd opening the app with `--hidden` (formationd side comes with TASK-034). |
| The tray icon shows once | Menu bar panel | Check for the duplicate-icon report ([tauri #10912](https://github.com/tauri-apps/tauri/issues/10912)) |
| Codex app-server supports turns, interrupts, and approvals as needed | Second harness | Prototype after Claude Code works |

## Open decisions

- UI foundation: plain CSS tokens plus Radix primitives (proposed) or shadcn/ui.
- D-044 build-first reading.
- Request-fork file isolation (D-056): forks share the main session's folder; a git worktree per request is one option, not decided.
- How a request fork's result reaches the owner and the main session (D-056), for example a reviewed diff; not decided.
