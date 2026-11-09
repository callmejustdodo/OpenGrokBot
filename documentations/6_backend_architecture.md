# Formation — Backend architecture

Rev 1 · 2026-11-09 · Implements the [tech spec](4_techspec.md) (Rev 3) for the requirements in the [PRD](1_product_requirements_documentation.md) (Rev 2). The frontend side is in the [frontend architecture](5_frontend_architecture.md). Decision IDs refer to the [planning progress](0_one_pager.md#planning-progress) record.

## Scope

"Backend" here is everything outside the React UI, all running on each member's Mac. There is no central server: Macs talk to each other over Tailscale (D-045).

| Part | Binary | Role |
| --- | --- | --- |
| `formationd` | `crates/formationd` (uses `formation-core`) | Background service (LaunchAgent): harness sessions, policy, queues, peer server, storage (D-048) |
| `formation` | `crates/formation-cli` | CLI and MCP server (teammate tools, and the permission tool for Claude Code runs); a client of formationd |
| App client | `apps/desktop/src-tauri` | Thin client of formationd for the UI, tray, menus, notifications |
| Shared types | `crates/formation-proto` | Requests, responses, events, errors, generated into TypeScript |

## Modules in `formation-core`

| Module | Responsibility | Requirements |
| --- | --- | --- |
| `service` | Lifecycle (the app registers formationd with launchd; see the [tech spec](4_techspec.md#technology-choices)), local socket server (newline-delimited JSON-RPC 2.0), snapshot builder, event bus with sequence numbers | D-048, NFR-007 |
| `harness::claude` | One supervisor task per running agent: spawn `claude -p` in stream-json mode in the agent folder, parse stdout events, write user messages to stdin, track queue and turn state, stop and resume, stalled detection | FR-003, FR-022, FR-007 |
| `permission` | The gate behind `permission_decide`, which `formation mcp --permission` and the `formation hook pre-tool-use` hook send: finds the run's caller, asks `policy`, raises approvals; the hook only denies or defers, and a deny wins for that tool call | FR-015, FR-022, NFR-004 |
| `harness::codex` | `codex app-server` JSON-RPC adapter (after Claude Code) | D-002 |
| `harness::detect` | Find sessions started elsewhere (transcripts, live-session markers), Hermes and OpenClaw installs; report per-harness capabilities | FR-006 |
| `agents` | Agent folders: create layout (`AGENTS.md`, `CLAUDE.md` importing it, `.claude/skills/`, `formation.toml` with name, role, harness, avatar), watch for outside edits, file read and write for the UI | FR-001, FR-004 |
| `schedules` | Timers per schedule; detect missed runs after sleep and run them once; record runs | FR-005 |
| `policy` | Membership and sharing checks, visibility, always-allowed reads, rule matching, approvals and expiry, chain depth and rate limits | FR-009, FR-015, FR-019, D-024 to D-028 |
| `ask` | Routing (score shared agents, decide confident or unsure), answer runner (an isolated, tool-less fork; see the tech spec's [ask pipeline](4_techspec.md#ask-pipeline)), excerpts, `as_of` and staleness, secret masking | FR-010 to FR-013, NFR-003, NFR-005 |
| `requests` | Per-agent ordered queue for work requests, idempotency, status machine; fork execution follows [FR-014](1_product_requirements_documentation.md#work-requests-and-approvals) | FR-014, FR-017 |
| `peer` | HTTP and WebSocket server on the Tailscale address; client with an outbox and retries; Tailscale WhoIs identity; presence | FR-016, NFR-006 |
| `team` | Members, invitations, GitHub identity binding, roster changes | FR-008, D-046 |
| `store` | SQLite schema, migrations, transactions | D-029, FR-020 |
| `notify` | Needs-attention items, mute settings, opening the app hidden to show notifications | FR-007 |

## Where each rule is enforced

All enforcement happens in formationd on the owner's Mac. The UI, CLI, MCP clients, and peers only request.

| Rule | Enforced in | How |
| --- | --- | --- |
| Private agents are invisible to teammates | `policy` before every peer response and routing step | Filter by `agent_share.shared`; never list, route to, or answer from private agents |
| Teammates see answers only, or answers and excerpts | `ask` post-processing | Drop excerpts when visibility is `answer` (D-025) |
| Side conversations change nothing | `ask` runner | Fork with all tools disabled; never resume the original session for a question |
| Nothing runs without approval except allowed reads | `permission`, reached by the permission tool and the `PreToolUse` hook, calling `policy` | Allowed reads inside the agent folder and assigned repositories; saved rules; otherwise raise an approval and wait (D-028) |
| Rules never widen | `policy::rules` | Match program, argument pattern, and folder exactly as stored; reject command chaining and paths outside the folder |
| Only the owner approves | `policy::approvals` | Decisions are accepted only from the local socket of the owner's own session, never from peers |
| A retried request runs once | `requests` with a unique index on (requester, idempotency key) | Insert-or-return in one transaction |
| Only the owner writes to the main session | `harness::claude` and `requests` | Serialize owner turns; execute teammate requests in separate forks per [FR-014](1_product_requirements_documentation.md#work-requests-and-approvals) |
| Removed members lose access at once | `team` and `peer` | Roster check on every request; open WebSocket connections of removed members are closed |
| Chains cannot run away | `policy::limits` | Depth over 3 or over 30 requests per minute per caller returns `rate_limited` |

## Request handling

```mermaid
sequenceDiagram
  participant A as Asker's formationd
  participant O as Owner's formationd
  participant C as claude (fork, no tools)
  A->>O: POST /v1/ask (over Tailscale)
  O->>O: WhoIs → member; policy checks
  O-->>A: 202 question_id
  O->>O: routing (confident or unsure)
  alt unsure
    O-->>A: event: unsure + candidates
  else confident or agent picked
    O->>C: isolated tool-less fork of the session
    C-->>O: answer stream
    O->>O: excerpts, as_of, masking
    O-->>A: event: answered
  end
```

Work requests follow the same entry path to `requests`; they run per [FR-014](1_product_requirements_documentation.md#work-requests-and-approvals), with the policy context bound at fork startup as specified in the [Claude Code integration](4_techspec.md#claude-code-integration-first-harness), and every tool call goes through `permission`.

## Error codes

`not_member`, `agent_not_shared`, `requests_not_accepted`, `not_found`, `peer_offline` (queued in the outbox), `approval_expired`, `denied`, `rate_limited`, `harness_unavailable` (not installed or not signed in), `harness_unsupported` (detected agent cannot do this), `conflict` (state changed), `invalid_params` (the request does not match the command), `internal` (also what an older Mac reads for a code added later). The CLI maps them to the exit codes in the [tech spec](4_techspec.md#cli-formation-and-mcp-server).

## Storage and data ownership

- **Location:** `~/Library/Application Support/Formation/formation.db` (SQLite, WAL mode), schema from the [data model](4_techspec.md#data-model), migrations applied by formationd at start.
- **What each Mac owns:** the owner's Mac owns its agents, their folders, approvals, rules, and the answers its agents produce. The asker's Mac keeps a copy of its own questions and the answers received (D-029).
- **Harness transcripts** stay where Claude Code and Codex keep them; Formation reads them and never copies them off the Mac, except excerpts allowed by visibility.
- **Deletion (proposed):** an asker's delete removes only the asker's copy. An owner's delete removes the owner's copy and asks the asker's Mac to delete its copy the next time it is reachable. Confirm in review; the PRD allows both deletions but does not say whether the owner's delete reaches the asker's copy.
- **Backups (assumption):** none in the first version. Losing a Mac loses its Formation history; the harness transcripts and agent folders remain wherever the user backs them up.

## Recovery

- **formationd crash:** launchd restarts it (`KeepAlive`). On start it rebuilds state: resumes Formation-managed sessions with `--resume` (never detected originals, per [FR-006](1_product_requirements_documentation.md#managing-agents-owner)), re-queues requests that were `queued`, marks interrupted ones `failed` with a reason, expires approvals past 24 hours, and retries the outbox.
- **App crash or quit:** no effect on agents; the UI reconnects and takes a fresh snapshot.
- **Harness crash:** the agent goes to `error` with the exit code; restart resumes a Formation-managed session; detected originals follow [FR-006](1_product_requirements_documentation.md#managing-agents-owner).
- **Peer unreachable:** questions and requests wait in the asker's outbox; the owner re-checks membership and sharing when they arrive (FR-016).

## Identity and team

- **Joining:** the creator generates an invitation link (single-use code, 24 hours). The invitee's formationd presents the code to the creator's formationd over Tailscale, along with a GitHub login proven through the GitHub OAuth device flow. The roster, link format, and join rules are in the tech spec's [team and roster](4_techspec.md#team-and-roster).
- **GitHub verification (assumption to verify):** a GitHub OAuth App registered by the team creator supplies the client ID; formationd uses the device flow to read the user's login. Fallback for the MVP, in use now: the joiner's Mac reads the login from `gh api user`, and the creator's Mac can check only its shape.
- **Per request:** Tailscale WhoIs on the connection gives the Tailscale user (`LoginName`) and node (`StableID`), which must both match a current member's recorded Tailscale login and node (D-046). Tagged nodes have no user and are refused, as are removed members. The join call skips only the member check, never WhoIs.

## Configuration, logs, and operations

- **Config:** `~/Library/Application Support/Formation/config.toml` (peer port 7413, harness paths, thresholds such as stalled after 10 minutes).
- **Local socket:** `~/Library/Application Support/Formation/formationd.sock`; override with `FORMATION_SOCKET`.
- **Logs:** `~/Library/Logs/Formation/`, rotated daily and kept 7 days (assumption); no telemetry leaves the Mac.
- **Operations:** the team creator runs the Tailscale tailnet and the GitHub OAuth App; each member keeps Claude Code (and later Codex) signed in. No servers to run.

## Reuse plan (D-051)

Inspect and copy before writing from scratch. Licenses checked 2026-11-09; keep notices and record each copied file in `THIRD_PARTY_NOTICES.md`.

| Formation module | Source | License | Use |
| --- | --- | --- | --- |
| `harness::detect` | [Reason Network](https://github.com/reason-machines/reason-network) `src/daemon/sessions.rs`, `src/setup/hermes.rs` | MIT or Apache-2.0 | Copy and adapt: discovers Claude Code sessions from local session files and Hermes profiles |
| `harness::claude`, `harness::codex` | Reason Network `src/claude.rs`, `src/codex.rs`, `src/hook.rs` | MIT or Apache-2.0 | Copy and adapt: harness specifics, hook payloads, Codex app-server usage |
| Hook and MCP setup for harnesses | Reason Network `src/setup.rs` | MIT or Apache-2.0 | Adapt: installing hooks and MCP config per harness, with consent (FR-006) |
| `formation mcp` | Reason Network `src/mcp.rs`; official [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) | MIT or Apache-2.0; MIT | Reference for tool shapes; build on `rmcp` |
| `team`, invitations | Reason Network `src/team.rs`, `src/invite.rs` | MIT or Apache-2.0 | Adapt roster and invite flow to Tailscale and GitHub identity |
| `store` | Reason Network `src/store.rs` | MIT or Apache-2.0 | Reference schema and migration approach |
| `notify`, tray | Reason Network `src/daemon/notify.rs`, `src/tray.rs` | MIT or Apache-2.0 | Reference macOS notification and menu bar handling |
| `peer` (after the MVP) | Reason Network `src/net.rs` (iroh peer-to-peer with relay fallback); [Agent Relay](https://github.com/AgentWorkforce/relay) | MIT or Apache-2.0; Apache-2.0 | Candidate replacement for Tailscale before wider release |
| CLI messaging patterns | [squad (mco-org)](https://github.com/mco-org/squad) | MIT | Reference |
| Approvals model | [Happier](https://github.com/happier-dev/happier), [Paperclip](https://github.com/paperclipai/paperclip) | MIT | Reference |
| Not to copy | HAPI, Claude Squad (AGPL-3.0); Flowpad (no license) | — | Read for ideas only |

## Tests

| Boundary | Tests |
| --- | --- |
| `policy` | Table-driven unit tests for visibility, rules (including chaining and path escapes), approvals expiry, limits |
| `harness::claude` | Replay recorded `stream-json` from a fake `claude`; queue, stop, resume, stalled detection |
| `ask` | Routing thresholds; fork invocation flags; excerpt dropping by visibility; masking patterns; staleness |
| `requests` | Idempotency and ordering under concurrent inserts |
| `peer` | Two formationd instances over loopback with a stub WhoIs; outbox retry; removed-member disconnect |
| Recovery | Kill formationd during a turn and during a queued request; check the rebuilt state |
| Manual | Two Macs on the tailnet: ask a teammate's agent; send a work request and approve it |
