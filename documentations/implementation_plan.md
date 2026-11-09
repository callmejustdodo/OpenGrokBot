# OpenGrokBot — Implementation plan

Rev 2 · 2026-11-09 · For the coding agents of a team of three developers and one designer. Decision IDs refer to the [planning progress](0_one_pager.md#planning-progress) record. Rev 2 removes the waits between roles: the contract is written together at kickoff, each role builds the fakes it needs, tasks depend on interfaces instead of another role's implementation, and work is rebalanced (D-054).

## Goal

Build OpenGrokBot, a macOS app that runs role agents on Claude Code (later Codex) and lets a four-person team ask each other's agents, as specified in the documents below. **First milestone (today, D-052):** on two Macs on the team's tailnet, a member can chat with their own Claude Code agent, a teammate can ask that member's agent and get an answer from its session conversation, and a teammate can send a work request that runs after the owner approves its command.

## Read first, in this order

1. [One-pager](0_one_pager.md): why, for whom, scope.
2. [Product requirements](1_product_requirements_documentation.md): FR and NFR IDs with acceptance criteria.
3. [User stories](2_user_stories.md): US IDs with Given/When/Then.
4. [Screens and flows](3_screen_list_and_user_flows.md) and the approved **[mockup M3](mockups/index.html)** (open it in a browser; every SC ID and state is selectable).
5. [Tech spec](4_techspec.md): stack, Claude Code process contract, data model, interfaces.
6. [Frontend architecture](5_frontend_architecture.md) and [backend architecture](6_backend_architecture.md): modules, enforcement points, reuse plan.

## Current state and exclusions

- **Code:** none yet. The repository has the documents, the mockup, `.claude/skills/` (macOS skills; Tauri was chosen over Swift, so they matter mainly for native details), `AGENTS.md` conventions, and one check: `node scripts/check-documentation.mjs`.
- **Excluded from the first version:** see the [PRD exclusions](1_product_requirements_documentation.md#exclusions) (usage and cost view, Slack, customer-facing channels, personal cloud agents, web UI, Windows and Linux, billing, large-team administration).
- **Not built here:** signing, notarization, and an updater (before wider release); a non-Tailscale transport (after the MVP).

## Roles and ownership

Each role is one person and their coding agent. A role changes only the paths it owns; anything else goes through a pull request reviewed by the owner.

| Role | Area | Owns (paths) |
| --- | --- | --- |
| **A · Agent runtime** | formationd service, local storage, Claude Code (later Codex) harness supervisor, agent folders, schedules, detection, notifications | `crates/formationd/`, `crates/formation-core/src/{service,store,harness,agents,schedules,notify}/`, migrations `1000–1999` and the migration runner, proto files `common.rs`, `agent.rs`, `harness.rs`, and the `Harness`, `Store`, `Notifier`, `EventSink`, `CommandHandler` traits |
| **B · Collaboration core** | Team and identity, Tailscale peer protocol, policy and approvals, the permission gate (Claude Code permission tool and `PreToolUse` hook), ask pipeline, work requests, CLI and MCP | `crates/formation-core/src/{team,peer,policy,permission,ask,requests,collab_store}/`, `crates/formation-cli/`, migrations `2000–2999`, proto files `team.rs`, `peer.rs`, `policy.rs`, `ask.rs`, `request.rs`, and the `CollabStore`, `Policy`, `PermissionGate`, `Peers`, `RequestQueue` traits |
| **C · App** | Tauri client, LaunchAgent registration from the app, tray, menus, windows, notifications bridge, IPC, store, view logic, `fake-formationd` | `apps/desktop/src-tauri/`, `apps/desktop/src/{ipc,store,views}/`, window entries, `tools/fake-formationd/`, proto files `snapshot.rs`, `events.rs` (the envelope; each event payload lives with its domain owner) |
| **D · Design** | Design tokens, component styles, avatars, Korean and English copy, preview page, visual QA against M3 | `apps/desktop/src/styles/`, `apps/desktop/src/**/*.css`, `apps/desktop/src/i18n/`, `apps/desktop/src/preview/` |

Shared component `.tsx` files in `apps/desktop/src/components/` and `ui/` are co-owned by C and D; each change is reviewed by the other.

## How the parts join (integration contract)

1. **Contract written together at kickoff (TASK-002).** In the first 30 to 45 minutes, A, B, and C write `formation-proto` and the port traits in one pull request, each in the files their role owns. After it merges, changes are additive only; a breaking change needs approval from all four roles.
2. **Ports and adapters.** `formation-core/src/ports.rs` re-exports the traits, one file per port under `ports/`: `Harness`, `Store`, `Notifier`, `EventSink`, `CommandHandler` (A); `CollabStore`, `Policy`, `PermissionGate`, `Peers`, `RequestQueue` (B). Each role implements one `CommandHandler` for the commands its proto files define, and the service routes each method to it. A module never imports another module's internals; `formationd` wires the real adapters.
3. **Each role builds the fakes it needs others to use.** A ships `fake-claude` (replays recorded `stream-json`), `FakeHarness`, an in-memory `Store`, and a recording `EventSink` (TASK-003). B ships `stub-whois`, `FakePolicy`, `FakePermissionGate`, `FakePeers`, `FakeRequestQueue`, and an in-memory `CollabStore` (TASK-042). C ships `fake-formationd`, which serves the mockup's sample snapshot and events (TASK-043). Each real piece replaces its fake by configuration (`FORMATION_FAKE=…`), not by code edits.
4. **Dependencies point at interfaces.** Before an integration checkpoint, no task waits for another role's implementation; it uses the port and its fake. Real pieces meet only at IC-1, IC-2, and IC-3.
5. **No shared files between roles.** Migrations use numbered ranges per role (A `1000–1999`, B `2000–2999`); proto is split into per-role files; i18n keys are grouped per view; CSS lives beside each component.
6. **Branches and merges.** One branch and one pull request per task, named `feat/<area>-task-NNN` (for example `feat/harness-task-013`), squash-merged to `main` per `AGENTS.md`. `main` stays green: every pull request runs the checks in [Commands](#commands).
7. **Integration checkpoints.** All four join at IC-1, IC-2, and IC-3, switch fakes to real pieces, run the checkpoint script, and fix forward before the next phase.

## Lanes

After kickoff, each role works down its own lane without waiting for another role until the next checkpoint.

| | Kickoff (together) | Phase 1 · Proofs | Phase 2 → IC-1 | Phase 3 → IC-2 | Phase 4 → IC-3 (today) |
| --- | --- | --- | --- | --- | --- |
| **A** | TASK-001 scaffold; TASK-002 (A files) | TASK-003 fakes; TASK-004 Claude Code control | TASK-010 service; TASK-011 store; TASK-012 agent folders; TASK-013 Claude Code agents | TASK-022 session info for ask | TASK-024 run requests in forks |
| **B** | TASK-002 (B files) | TASK-042 fakes; TASK-005 permission routing; TASK-006 forked answers; TASK-007 two-Mac identity | TASK-017 team (start early, against fakes) | TASK-018 peer protocol; TASK-019 sharing, routing, answers | TASK-023 requests; TASK-025 approvals; TASK-044 permission gate |
| **C** | TASK-002 (C files) | TASK-043 fake-formationd; TASK-008 Tauri shell; TASK-009 LaunchAgent from the app | TASK-014 app store; TASK-015 my-agent screens | TASK-020 asking screens | TASK-026 request and approval screens |
| **D** | Reads M3; sets up styles once TASK-001 lands | TASK-016 tokens, avatars, copy for IC-1 | TASK-016 (continued) | TASK-021 | TASK-027 |

## Commands

Only the documentation check exists today; the rest are added by TASK-001 and must then work.

| Command | Purpose | Status |
| --- | --- | --- |
| `node scripts/check-documentation.mjs` | Markdown links and anchors | Exists |
| `cargo test --workspace` | Rust unit and integration tests | Proposed (TASK-001) |
| `cargo fmt --all -- --check` | Rust formatting | Exists |
| `cargo clippy --workspace -- -D warnings` | Rust lint | Proposed (TASK-001) |
| `pnpm -C apps/desktop test` | Vitest | Proposed (TASK-001) |
| `pnpm -C apps/desktop lint` and `pnpm -C apps/desktop typecheck` | ESLint and TypeScript | Proposed (TASK-001) |
| `pnpm -C apps/desktop tauri dev` | Run the app in development | Proposed (TASK-001) |
| `cargo run -p formationd` | Run the service in the foreground for development | Proposed (TASK-010) |

## Phases

| Phase | Tasks | Ends with |
| --- | --- | --- |
| 0 · Kickoff (together, 30–45 min) | TASK-001, TASK-002 | Contract merged; every role can build |
| 1 · Fakes and proofs (parallel lanes) | TASK-003 to TASK-009, TASK-042, TASK-043 | Each role can work alone; tech spec risks answered |
| 2 · Chat with my agent | TASK-010 to TASK-016 | **IC-1** |
| 3 · Ask a teammate's agent | TASK-017 to TASK-022 | **IC-2** |
| 4 · Work requests and approvals | TASK-023 to TASK-027, TASK-044 | **IC-3 = today's milestone** |
| 5 · Complete the first version | TASK-028 to TASK-040 | **IC-4** |
| 6 · Codex | TASK-041 | **IC-5** |

## Tasks

### Phase 0 · Kickoff

### TASK-001: Repository scaffold builds and runs an empty app
- Status: complete
- References: [tech spec · starter](4_techspec.md#starter), [code layout](4_techspec.md#code-layout)
- Prerequisites: none
- Change locations: proposed `Cargo.toml` (workspace), `crates/{formation-proto,formation-core,formationd,formation-cli}/` (empty crates; `formationd` prints its version), `apps/desktop/` (from `pnpm create tauri-app`, TypeScript, React, TypeScript), scripts, `.github/workflows/` (Rust and frontend checks beside `documentation.yml`), `THIRD_PARTY_NOTICES.md`, ownership table in `AGENTS.md`
- Work: create the workspace and empty crates; scaffold the Tauri app; confirm the template uses Vite; remove the sample command; add lint, test, and typecheck scripts; set the macOS deployment target to 13 (D-047); create `THIRD_PARTY_NOTICES.md` with the format for copied code (D-051); add the role ownership table to `AGENTS.md`.
- Acceptance criteria: `pnpm -C apps/desktop tauri dev` opens an empty window; all commands in [Commands](#commands) run green; CI runs them on pull requests.
- Verification: run each command; open a draft pull request and see CI pass.
- Owner: A (about 15 minutes; B and C draft their proto files meanwhile)

### TASK-002: The contract exists as types and ports
- Status: in progress
- References: [UI ↔ formationd](4_techspec.md#ui--formationd), [peer protocol](4_techspec.md#peer-protocol-over-the-tailnet), [CLI and MCP](4_techspec.md#cli-formation-and-mcp-server), [error codes](6_backend_architecture.md#error-codes), [data model](4_techspec.md#data-model)
- Prerequisites: TASK-001
- Change locations: `crates/formation-proto/src/` (per-role files listed in [Roles and ownership](#roles-and-ownership)), `crates/formation-core/src/ports.rs` and `ports/`, TypeScript generated by `ts-rs` for `apps/desktop/src/ipc/`
- Work: each role writes its commands, events (payloads with per-stream sequence numbers), snapshot parts, peer payloads, CLI JSON, error codes, and port traits; C writes the snapshot and event envelope; generate TypeScript with `ts-rs` (chosen in #18).
- Interfaces: produces every name used by later tasks.
- Acceptance criteria: one pull request co-authored by A, B, and C; types compile; TypeScript is generated in the build; all three approve.
- Verification: `cargo test -p formation-proto` (serialization round-trips); `pnpm -C apps/desktop typecheck`.
- Owner: A, B, C together

### Phase 1 · Fakes and proofs

### TASK-003: A's fakes: fake Claude, FakeHarness, in-memory store
- Status: complete (#41)
- References: [integration contract](#how-the-parts-join-integration-contract)
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/testkit/` (`fake-claude` replay binary, `FakeHarness`, in-memory `Store`, recording `EventSink`)
- Work: `fake-claude` replays recorded `stream-json` (seeded with a hand-written transcript until TASK-004 records real ones); `FakeHarness` exposes session IDs, transcript paths, and scripted turns; the in-memory `Store` implements A's repositories; the recording `EventSink` keeps published events for assertions.
- Acceptance criteria: B can run ask and team code against `FakeHarness` and the in-memory store without A's real modules.
- Verification: `cargo test -p formation-core --features testkit`.
- Owner: A

### TASK-042: B's fakes: stub WhoIs, fake policy, peers, and request queue
- Status: complete
- References: [integration contract](#how-the-parts-join-integration-contract)
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/testkit/` (`stub-whois`, `FakePolicy`, `FakePermissionGate`, `FakePeers`, `FakeRequestQueue`, in-memory `CollabStore`)
- Work: fakes with scriptable answers (allow, deny, unsure routing, queued requests).
- Acceptance criteria: A can run the request executor and C's fake service can simulate approvals without B's real modules.
- Verification: `cargo test -p formation-core --features testkit`.
- Owner: B

### TASK-043: C's fake-formationd serves the mockup's world
- Status: complete
- References: [data flow](5_frontend_architecture.md#data-flow), mockup M3 sample data
- Prerequisites: TASK-002
- Change locations: `tools/fake-formationd/` with fixtures
- Work: a small server on the same local socket protocol that returns a snapshot built from M3's sample data and plays scripted event sequences (send and stream a reply, queue, ask flow, approval).
- Acceptance criteria: every M3 state for SC-003, SC-005, SC-010, SC-011, SC-015, SC-016 can be reached from scripted events.
- Verification: run the app against it (after TASK-008).
- Owner: C

### TASK-004: Claude Code process control is proven
- Status: complete
- References: [Claude Code integration](4_techspec.md#claude-code-integration-first-harness), FR-003, FR-022
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/harness/claude/` (spike), recorded fixtures in `testkit/fixtures/`
- Work: start `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --replay-user-messages --session-id <uuid>` in a folder; send a message; send a second while the first turn runs; stop a turn; resume with `--resume`; test whether a message can steer a running turn. Record outputs as fixtures for `fake-claude`.
- Acceptance criteria: a written result for each behavior (works, does not, workaround) in the pull request; fixtures committed.
- Verification: the spike's integration test against the real `claude` (ignored in CI).
- Results: see [Claude Code integration](4_techspec.md#claude-code-integration-first-harness) for the proven queue, steer, and stop behavior (TASK-039).
- Owner: A

### TASK-005: Permission routing covers every tool call
- Status: complete
- References: NFR-004, FR-015, FR-022, [tech spec assumptions](4_techspec.md#assumptions-to-verify-early)
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/permission/` (spike), `crates/formation-cli/` (`formation mcp --permission` stub)
- Work: run Claude Code with `--permission-prompt-tool mcp__formation__permission` and an injected `PreToolUse` hook via `--settings`; try each permission mode and tool type (read, edit, bash, network); log which calls reach the tool and which reach the hook. Hand the exact flags to A for TASK-013.
- Acceptance criteria: a coverage table showing that every write, command, and network call reaches OpenGrokBot before it runs; the flag set A must pass when spawning.
- Verification: the spike's logged run.
- Owner: B

### TASK-006: Forked answers do not change the original session
- Status: complete
- References: [ask pipeline](4_techspec.md#ask-pipeline), FR-012, NFR-002
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/ask/` (spike)
- Work: answer a question with `--resume <session> --fork-session --tools ""`; compare the original transcript before and after; measure time to first token and to the full answer.
- Acceptance criteria: the original transcript is byte-identical afterwards; the answer cites only conversation content; timings recorded.
- Verification: the spike's test.
- Owner: B

### TASK-007: Two Macs reach each other with verified identity
- Status: complete
- References: [identity](6_backend_architecture.md#identity-and-team), D-045, D-046
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/peer/` (spike)
- Work: bind an HTTP server to the Mac's Tailscale address on port 7413; call it from another Mac; resolve the caller with `tailscale whois --json <addr>` (or the LocalAPI); read the GitHub login from `gh api user`.
- Acceptance criteria: the server logs the caller's Tailscale login and node; a request from outside the tailnet cannot connect.
- Verification: manual run on two Macs.
- Owner: B

### TASK-008: The Tauri shell shows the tray, the window, and fake data
- Status: in progress
- References: [windows](5_frontend_architecture.md#windows), SC-002, SC-003
- Prerequisites: TASK-043
- Change locations: `apps/desktop/src-tauri/`, `apps/desktop/src/main.tsx`, `menubar.tsx`
- Work: tray icon (check for a duplicate icon on macOS 13 and the latest macOS), main window with traffic lights and sidebar vibrancy, menu bar panel under the tray icon, connection to `fake-formationd` over the local socket.
- Acceptance criteria: one tray icon; the panel opens under it; the main window shows fixture data.
- Verification: run on macOS 13 and the latest macOS.
- Owner: C

### TASK-009: The app registers formationd as a LaunchAgent that outlives it
- Status: complete
- References: D-048, [system overview](4_techspec.md#system-overview)
- Prerequisites: TASK-001 (uses the placeholder `formationd` binary)
- Change locations: `apps/desktop/src-tauri/` (registration), bundled LaunchAgent plist
- Work: register the bundled `formationd` as a per-user LaunchAgent from the app (SMAppService, or a plist with `launchctl`); `KeepAlive`; confirm it survives quitting the app and restarts after a crash; let formationd open the app hidden.
- Acceptance criteria: quitting the app leaves formationd running; killing formationd restarts it; the app can be opened hidden.
- Verification: manual on macOS 13.
- Owner: C

### Phase 2 · Chat with my agent (IC-1)

### TASK-010: formationd serves snapshots and events over the local socket
- Status: complete (#43, #87, #101)
- References: [UI ↔ formationd](4_techspec.md#ui--formationd), NFR-007
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/src/service/`, `crates/formationd/`
- Work: JSON-RPC 2.0 over a Unix socket with owner-only permissions; snapshot builder; event bus with sequence numbers; wiring of real or fake adapters by configuration.
- Acceptance criteria: a client gets a snapshot and then ordered events; a dropped client reconnects and resumes from a new snapshot; B's modules can be plugged in as fakes until IC-2.
- Verification: `cargo test -p formation-core service`.
- Owner: A

### TASK-011: Storage with per-role migrations
- Status: complete (#53)
- References: [data model](4_techspec.md#data-model), [storage](6_backend_architecture.md#storage-and-data-ownership)
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/src/store/`, migration runner, A's migrations `1000–1999` (agents, sessions, schedules)
- Work: SQLite in WAL mode at the documented path; migration runner that applies both ranges; A's repositories behind the `Store` port. B writes its own migrations `2000–2999` and its repositories behind the `CollabStore` port in its modules (TASK-017, TASK-019, TASK-023, TASK-025) using the runner.
- Acceptance criteria: a fresh start creates the schema; re-running is a no-op; adding B's migration files needs no change to A's files.
- Verification: `cargo test -p formation-core store`.
- Owner: A

### TASK-012: Agent folders are created and edited
- Status: complete (#58); app check at IC-1 pending (#46)
- References: FR-001, FR-004, US-002, US-004, SC-004, SC-006
- Prerequisites: TASK-010, TASK-011
- Change locations: `crates/formation-core/src/agents/`
- Work: create the folder layout (`AGENTS.md`, `CLAUDE.md` with `@AGENTS.md`, `.claude/skills/`, `formation.toml` with name, role, harness, avatar); list agents; read and write files; watch for outside edits; harness readiness check.
- Acceptance criteria: US-002 and US-004 criteria, including "harness not ready" still creating the folder.
- Verification: unit tests; create an agent from the app at IC-1.
- Owner: A

### TASK-013: Claude Code agents run, queue, and stop under formationd
- Status: complete (#56; real Claude run in #21)
- References: FR-003, FR-022, US-003, US-024, [Claude Code integration](4_techspec.md#claude-code-integration-first-harness), TASK-004 and TASK-005 results
- Prerequisites: TASK-004, TASK-010, TASK-012
- Change locations: `crates/formation-core/src/harness/claude/`
- Work: one supervisor per running agent; send; queue while working; stop (after this turn or now); resume on restart; emit `agent.state`, `agent.stream`, `agent.queue`; stalled after 10 minutes; pass the permission flags from TASK-005 (pointing at `FakePermissionGate` until IC-3). Reuse Reason Network `src/claude.rs` where it fits and record it in `THIRD_PARTY_NOTICES.md`.
- Acceptance criteria: US-003 and US-024 criteria, except steering (TASK-039) and permission cards (TASK-044).
- Verification: tests with `fake-claude`; manual run with real Claude Code.
- Owner: A

### TASK-014: The app's store mirrors formationd
- Status: complete
- References: [data flow](5_frontend_architecture.md#data-flow)
- Prerequisites: TASK-008
- Change locations: `apps/desktop/src/ipc/`, `apps/desktop/src/store/`, `apps/desktop/src-tauri/` (socket client)
- Work: typed command calls; snapshot then events; gap detection; pending states within 100 ms; reconnect banner.
- Acceptance criteria: works against `fake-formationd`; at IC-1, works against the real service unchanged.
- Verification: reducer unit tests; manual reconnect test.
- Owner: C

### TASK-015: Sidebar and conversation with my agent
- Status: in progress
- References: SC-003, SC-004, SC-005, SC-020; M3 states for those screens; US-001, US-002, US-024
- Prerequisites: TASK-014
- Change locations: `apps/desktop/src/views/{my-conversation,new-agent}/`, `apps/desktop/src/components/{sidebar,info-panel}/`
- Work: chat list with state dots and the Needs-you line; new agent sheet with harness and avatar pickers; conversation with bubbles, step groups, queued chip, composer with Stop; info panel basics. Built against `fake-formationd`.
- Acceptance criteria: the screens match M3 states working, idle, queued, stopped, error, stop-confirm.
- Verification: component tests; D's visual check on the preview page.
- Owner: C

### TASK-016: Tokens, avatars, and copy for IC-1 screens
- Status: pending
- References: [design system](3_screen_list_and_user_flows.md#visual-direction-and-design-system), [design system in code](5_frontend_architecture.md#design-system-in-code), D-042, D-050
- Prerequisites: TASK-001
- Change locations: `apps/desktop/src/styles/tokens.css`, component stylesheets, `apps/desktop/src/i18n/{ko,en}.json`, `apps/desktop/src/preview/`, `components/AgentAvatar`
- Work: port the mockup tokens (light and dark); style IC-1 components; Korean and English strings; preview page with each IC-1 component state; avatar stills via `open-bot-avatar`.
- Acceptance criteria: the preview page shows IC-1 components in all M3 states in both themes and both languages.
- Verification: D's review against M3 screenshots.
- Owner: D

**IC-1 · Chat with my agent (one Mac):** switch C's app from `fake-formationd` to the real formationd. Create an agent, start it, chat, send a second message while it works (queued), stop after this turn; quit the app (⌘Q) and check the agent keeps running; reopen and check the conversation and state are restored.

### Phase 3 · Ask a teammate's agent (IC-2)

### TASK-017: Team creation and joining
- Status: complete
- References: FR-008, US-008, US-020, SC-001, SC-013, D-046, TASK-007 results
- Prerequisites: TASK-007, TASK-042 (uses the in-memory `CollabStore` from TASK-042 until TASK-011 merges)
- Change locations: `crates/formation-core/src/team/`, B's migrations
- Work: create team; expiring invitation code; join over Tailscale with GitHub login (MVP: from `gh api user`); roster; WhoIs-to-member mapping; removal.
- Acceptance criteria: US-008 and US-020 criteria.
- Verification: two-Mac manual test; unit tests for expiry and roster.
- Owner: B (can start during Phase 2)

### TASK-018: Peer protocol with outbox
- Status: complete
- References: [peer protocol](4_techspec.md#peer-protocol-over-the-tailnet), FR-016
- Prerequisites: TASK-017
- Change locations: `crates/formation-core/src/peer/`
- Work: `GET /v1/agents`, `POST /v1/ask`, `WS /v1/events`; outbox with retry; presence events.
- Acceptance criteria: requests from non-members get `403`; offline peers queue and deliver on return.
- Verification: two-instance loopback test with `stub-whois`.
- Owner: B

### TASK-019: Sharing, routing, and answers from the session conversation
- Status: complete
- References: FR-009 to FR-013, US-009, US-013 to US-017, [ask pipeline](4_techspec.md#ask-pipeline), D-025 to D-027, TASK-006 results
- Prerequisites: TASK-006, TASK-018 (uses `FakeHarness` from TASK-003 until IC-2)
- Change locations: `crates/formation-core/src/{policy,ask}/`
- Work: share settings with defaults (private; answer and excerpts; requests off); share preview; routing with confident and unsure outcomes; forked answer with excerpts, `as_of`, staleness, masking; follow-ups and refresh.
- Acceptance criteria: US-009 and US-013 to US-017 criteria; private agents never appear; the original session is unchanged.
- Verification: unit tests (routing thresholds, masking, visibility); two-Mac test at IC-2.
- Owner: B

### TASK-020: Asking screens
- Status: in progress
- References: SC-008, SC-013, SC-014, SC-015, SC-001; M3 states
- Prerequisites: TASK-014
- Change locations: `apps/desktop/src/views/{ask-conversation,member,team,sharing,welcome}/`
- Work: asking conversation (all ten M3 states), member's agents, team page, sharing section with preview and unshare confirmation, welcome and join. Built against `fake-formationd`.
- Acceptance criteria: matches M3 for those screens.
- Verification: component tests; D's visual check.
- Owner: C

### TASK-021: Styles and copy for IC-2 screens
- Status: pending
- References: as TASK-016
- Prerequisites: TASK-016
- Change locations: as TASK-016
- Work: answer bubble with source footer and excerpts, choice card, waiting card, teammate avatars with owner badge, copy, preview entries.
- Acceptance criteria: the preview page covers IC-2 components.
- Verification: D's review.
- Owner: D

### TASK-022: The real harness exposes session info for asking
- Status: complete (#69)
- References: FR-012, [ask pipeline](4_techspec.md#ask-pipeline)
- Prerequisites: TASK-013
- Change locations: `crates/formation-core/src/harness/claude/`
- Work: implement the `Harness` port's session ID and transcript path queries on the real supervisor, matching `FakeHarness`.
- Acceptance criteria: B's ask code runs on the real harness at IC-2 with no change.
- Verification: the same tests B runs against `FakeHarness`, run against the real harness with `fake-claude`.
- Owner: A

**IC-2 · Ask a teammate's agent (two Macs):** switch B's modules and A's harness from fakes to real. Minsu joins Dohyeon's team; Dohyeon shares Backend; Minsu asks Dohyeon "how far is the login work?" and gets an answer naming Backend with `as_of` and excerpts; Minsu asks an ambiguous question and picks from the choice card; a private agent never appears; Dohyeon's Backend transcript is unchanged.

### Phase 4 · Work requests and approvals (IC-3, today's milestone)

### TASK-023: Work request lifecycle
- Status: complete
- References: FR-014, US-019, SC-016
- Prerequisites: TASK-018
- Change locations: `crates/formation-core/src/requests/`, peer endpoints `POST /v1/requests`, `GET /v1/requests/{id}`, `POST /v1/requests/{id}/cancel`
- Work: create with idempotency; status machine; "accepts requests" check; cancel before start; implement the `RequestQueue` port that A's fork executor consumes, carrying requester and request ID for the per-request policy context (see [Claude Code integration](4_techspec.md#claude-code-integration-first-harness)).
- Acceptance criteria: US-019 criteria; a repeated idempotency key returns the original request.
- Verification: unit tests (idempotency under concurrency); two-Mac test at IC-3.
- Owner: B

### TASK-024: Requests run in forks
- Status: complete (#104)
- References: FR-014, FR-022
- Prerequisites: TASK-013, TASK-042 (uses `FakeRequestQueue` until IC-3)
- Change locations: `crates/formation-core/src/harness/claude/` (executor)
- Work: take requests from the `RequestQueue` port after the current turn and run each in its own fork of the agent's session (FR-014); bind its requester and request ID at fork startup per [Claude Code integration](4_techspec.md#claude-code-integration-first-harness); report progress and results back through the port. File isolation and delivery to the owner and main session remain [open](4_techspec.md#open-decisions).
- Acceptance criteria: queued requests run in separate forks in order after the current turn; the main session transcript is unchanged; status events reach the requester.
- Verification: tests with `fake-claude` and `FakeRequestQueue`.
- Owner: A

### TASK-025: Approvals: allowed reads, allow once, deny, expiry
- Status: complete
- References: FR-015, US-011, D-028, SC-011
- Prerequisites: TASK-019
- Change locations: `crates/formation-core/src/policy/`
- Work: always-allowed reads inside the agent folder and assigned repositories; raise approvals; decide allow once or deny; 24-hour expiry; notify requester. (Saved rules come in TASK-028.)
- Acceptance criteria: US-011 criteria except "keep allowing similar" and "allow all".
- Verification: table-driven tests, including path escapes.
- Owner: B

### TASK-044: The permission gate decides every tool call
- Status: complete (#124); manual run with real Claude Code at IC-3 pending
- References: FR-015, FR-022, NFR-004, TASK-005 results, proposed default 11
- Prerequisites: TASK-005, TASK-025
- Change locations: `crates/formation-core/src/permission/`, `crates/formation-cli/` (`formation mcp --permission`)
- Work: implement `PermissionGate`: the permission tool and the `PreToolUse` hook endpoint both call `Policy`; teammates' requests follow rules and approvals; the owner's own prompts become `approval.raised` events shown as cards in the conversation.
- Acceptance criteria: no write, command, or network call from a teammate's request runs without a decision; the owner's own prompts appear in the conversation.
- Verification: tests with `fake-claude`; manual run with real Claude Code at IC-3.
- Owner: B

### TASK-026: Request and approval screens
- Status: in progress
- References: SC-005 (approval and permission states), SC-010, SC-011, SC-016; M3 states
- Prerequisites: TASK-014
- Change locations: `apps/desktop/src/views/{work-request,inbox,approval,my-conversation}/`
- Work: request compose and status sheets; request and permission cards; inbox approvals group; approval sheet (default, deny-confirm, expired) with no Return binding and Esc asking first. Built against `fake-formationd`.
- Acceptance criteria: matches M3 for those states; keyboard rules hold.
- Verification: component tests for keyboard rules; D's visual check.
- Owner: C

### TASK-027: Styles and copy for IC-3 screens
- Status: pending
- References: as TASK-016
- Prerequisites: TASK-021
- Change locations: as TASK-016
- Work: approval sheet, request and permission cards, inbox, status timeline; copy; preview entries.
- Acceptance criteria: the preview page covers IC-3 components.
- Verification: D's review.
- Owner: D

**IC-3 · Today's milestone (two Macs):** switch remaining fakes to real pieces. After IC-2, Minsu sends "Add token expiry tests" to Dohyeon's Backend; it waits for the current turn, then runs in its own fork (FR-014), leaving the main session transcript unchanged; when it needs `npm test`, Dohyeon sees the approval and allows once; Minsu sees the status move to completed. Repeat with Deny and check the command never runs. Quit Dohyeon's app during the request and confirm it continues.

### Phase 5 · Complete the first version (IC-4)

| Task | Outcome | Owner | References | Prerequisites |
| --- | --- | --- | --- | --- |
| TASK-028 | Saved rules: keep allowing similar, allow all, limits, revoke | B | FR-015, US-012 | TASK-025 |
| TASK-029 | Rules page and keep-similar state of the approval sheet | C | SC-011, SC-012 | TASK-026 (fake data until TASK-028) |
| TASK-030 | Schedules with missed-on-wake runs | A | FR-005, US-005 | TASK-013 |
| TASK-031 | Schedules panel and sheet | C | SC-007 | TASK-014 (fake data until TASK-030) |
| TASK-032 | Detect and connect agents started elsewhere, Hermes and OpenClaw capability report (reuse Reason Network `src/daemon/sessions.rs`, `src/setup/hermes.rs`) | A | FR-006, US-006 | TASK-013 |
| TASK-033 | Connect agents sheet and read-only conversation | C | SC-009, SC-005 detected | TASK-014 (fake data until TASK-032) |
| TASK-034 | Monitoring and notifications; open the app hidden for alerts | A | FR-007, US-007 | TASK-013 |
| TASK-035 | Menu bar panel, notifications bridge, settings window | C | SC-002, SC-018, SC-019 | TASK-009, TASK-014 |
| TASK-036 | CLI and MCP teammate tools with chain limits | B | FR-017 to FR-019, US-022, US-023, IF-001, IF-002 | TASK-023 |
| TASK-037 | Offline outbox re-checks, deletion propagation, activity record | B | FR-013, FR-016, FR-020 | TASK-018 |
| TASK-038 | History page and Inbox "questions sent to me" (answer, hand to agent) | C | SC-010, SC-017, US-010 | TASK-014 (fake data until TASK-037) |
| TASK-039 | Steer now, edit and withdraw queued messages per [TASK-004 results](4_techspec.md#claude-code-integration-first-harness) | A, then C for UI | FR-022, TASK-004 results | TASK-013 |
| TASK-040 | Preview page complete, Korean and English pass, live avatars, VoiceOver pass | D with C | NFR-009, D-033, D-042, D-050 | TASK-026 |

Phase 5 status for A's tasks: TASK-030 complete (#92); TASK-032 complete (#84); TASK-034 complete (#70); TASK-039 complete (#94).

Each Phase 5 task uses the same fields as the tasks above when it starts: change locations from the [ownership table](#roles-and-ownership), acceptance criteria from its stories, verification by tests plus the matching M3 states. UI tasks start against `fake-formationd` and switch to real data when the backend task merges.

**IC-4 · First version complete:** every story US-001 to US-024 passes its Given/When/Then on two Macs; every M3 state exists in the app and the preview page.

### Phase 6 · Codex (IC-5)

### TASK-041: Codex agents through app-server
- Status: pending
- References: D-002, [Codex note](4_techspec.md#codex-integration-second-harness), Reason Network `src/codex.rs`
- Prerequisites: IC-4
- Change locations: `crates/formation-core/src/harness/codex/`
- Work: prove `codex app-server` turns, interrupts, and approvals; implement the `Harness` port for Codex; route approvals through `PermissionGate`.
- Acceptance criteria: US-024 and US-011 pass for a Codex agent.
- Verification: tests with recorded app-server traffic; manual run.
- Owner: A

## Traceability

| Requirement | Stories | Screens (M3 states) | Design | Tasks | Verification |
| --- | --- | --- | --- | --- | --- |
| FR-001 Create an agent | US-002 | SC-004 | Backend `agents` | 012, 015, 016 | Unit tests; IC-1 |
| FR-002 See all agents | US-001 | SC-003, SC-002 | Frontend sidebar | 010, 015, 035 | IC-1, IC-4 |
| FR-003 Start, stop, restart | US-003 | SC-005 | `harness::claude` | 004, 013, 015 | IC-1 |
| FR-004 Instructions and skills | US-004 | SC-006 | `agents` | 012, 015 | Unit tests; IC-4 |
| FR-005 Schedules | US-001, US-005 | SC-007 | `schedules` | 030, 031 | IC-4 |
| FR-006 Detect and connect | US-006 | SC-009 | `harness::detect` | 032, 033 | IC-4 |
| FR-007 Monitoring | US-001, US-007 | SC-002, SC-019 | `notify` | 034, 035 | IC-4 |
| FR-008 Team | US-008, US-020 | SC-001, SC-013 | `team` | 007, 017, 020 | IC-2 |
| FR-009 Share | US-009 | SC-008 | `policy` | 019, 020 | IC-2 |
| FR-010 Ask a person | US-010, US-013, US-014, US-017 | SC-015, SC-010 | `ask` routing | 019, 020, 038 | IC-2, IC-4 |
| FR-011 Ask an agent | US-015 | SC-014, SC-015 | `peer` | 018, 020 | IC-2 |
| FR-012 Answer from the conversation | US-013, US-015, US-017 | SC-015 | `ask` runner | 006, 019, 022 | IC-2 transcript check |
| FR-013 Follow-ups and history | US-012, US-016, US-017, US-021 | SC-015, SC-017 | `ask`, `store` | 019, 037, 038 | IC-2, IC-4 |
| FR-014 Request work | US-019 | SC-016 | `requests` | 023, 024, 026 | IC-3 |
| FR-015 Approve actions | US-011, US-012, US-019 | SC-011, SC-012 | `policy`, `permission` | 005, 025, 026, 028, 029, 044 | IC-3, IC-4 |
| FR-016 Owner offline | US-018 | SC-015 offline-waiting | `peer` outbox | 018, 037 | IC-4 |
| FR-017 CLI | US-022 | IF-001 | `formation-cli` | 036 | CLI tests; IC-4 |
| FR-018 MCP | US-023 | IF-002 | `formation-cli` (rmcp) | 036 | MCP tests; IC-4 |
| FR-019 Agent-to-agent safeguards | US-022, US-023 | IF-001, IF-002 | `policy::limits` | 036 | Unit tests |
| FR-020 Activity record | US-012 | SC-017 | `store` | 037, 038 | IC-4 |
| FR-021 Channel-ready design | — (design requirement) | — | Agent folder model leaves room for channels | 012 | Review of the folder layout |
| FR-022 Chat with my agent | US-024 | SC-005 | `harness::claude`, `permission` | 004, 013, 015, 024, 039, 044 | IC-1, IC-3 |
| NFR-001 Answer time | US-013 | SC-015 | `ask` | 006, 019 | Timings from TASK-006; IC-2 |
| NFR-002 No interruption | US-013 | — | Fork with no tools | 006, 019 | Transcript comparison |
| NFR-003 Sensitive content | US-009 | SC-008 preview | Masking | 019 | Unit tests |
| NFR-004 Execution safety | US-011 | SC-011 | Permission gate and hook | 005, 025, 044 | Coverage table; IC-3 deny check |
| NFR-005 Freshness | US-013 | SC-015 stale | `ask` | 019 | Unit tests |
| NFR-006 Connection security | — | — | Tailscale plus roster | 007, 017, 018 | Non-member `403` test |
| NFR-007 Reliability | — | — | Snapshot and events; recovery | 009, 010, 023 | Recovery tests |
| NFR-008 Resource use | — | — | Background service; light UI | 009, 010 | Measure at IC-4 |
| NFR-009 Accessibility | — | All | Radix, labels, keyboard | 015, 020, 026, 040 | VoiceOver pass |

## Execution instructions for the next AI

1. Read the documents in the [order above](#read-first-in-this-order) and the approved mockup M3. Check your role in [Roles and ownership](#roles-and-ownership) and your lane in [Lanes](#lanes); change only the paths your role owns.
2. Pick the first task in your lane whose prerequisites are complete. Prerequisites name interfaces and fakes, not other roles' implementations, so you should not need to wait between checkpoints. Set the task's `Status` line to `in progress` (status-only edits do not reopen this plan's review).
3. Before writing a module from scratch, check the [reuse plan](6_backend_architecture.md#reuse-plan-d-051) and the [frontend reuse list](5_frontend_architecture.md#reuse-from-open-source-competitors-d-051). Copy only from MIT or Apache-2.0 sources, keep their notices, and add each copied file to `THIRD_PARTY_NOTICES.md`. Never copy from HAPI, Claude Squad, or Flowpad.
4. Work on a branch named `feat/<area>-task-NNN`, commit with the format in `AGENTS.md`, and open a pull request using the template's sections.
5. Verify with the task's verification steps and the [commands](#commands). Do not mark a task complete on unrun checks; record what you ran.
6. If a contract in `formation-proto` must change, open a separate pull request for the contract and get approval from all four roles first.
7. If a change alters behavior or screens in the approved documents or mockup, stop and ask the user before continuing; update the owning document in the same change when they agree.
8. Set `Status` to `complete` when the acceptance criteria pass, and note the verification in the pull request.
