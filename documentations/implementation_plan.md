# Formation — Implementation plan

Rev 1 · 2026-11-09 · For the coding agents of a team of three developers and one designer. Decision IDs refer to the [planning progress](0_one_pager.md#planning-progress) record.

## Goal

Build Formation, a macOS app that runs role agents on Claude Code (later Codex) and lets a four-person team ask each other's agents, as specified in the documents below. **First milestone (today, D-052):** on two Macs on the team's tailnet, a member can chat with their own Claude Code agent, a teammate can ask that member's agent and get an answer from its session conversation, and a teammate can send a work request that runs after the owner approves its command.

## Read first, in this order

1. [One-pager](0_one_pager.md): why, for whom, scope.
2. [Product requirements](1_product_requirements_documentation.md): FR and NFR IDs with acceptance criteria.
3. [User stories](2_user_stories.md): US IDs with Given/When/Then.
4. [Screens and flows](3_screen_list_and_user_flows.md) and the approved **[mockup M3](mockups/index.html)** (open it in a browser; every SC ID and state is selectable).
5. [Tech spec](4_techspec.md): stack, Claude Code process contract, data model, interfaces.
6. [Frontend architecture](5_frontend_architecture.md) and [backend architecture](6_backend_architecture.md): modules, ownership, enforcement points, reuse plan.

## Current state and exclusions

- **Code:** none yet. The repository has the documents, the mockup, `.claude/skills/` (macOS skills; Tauri was chosen over Swift, so they matter mainly for native details), `AGENTS.md` conventions, and one check: `node scripts/check-documentation.mjs`.
- **Excluded from the first version:** see the [PRD exclusions](1_product_requirements_documentation.md#exclusions) (usage and cost view, Slack, customer-facing channels, personal cloud agents, web UI, Windows and Linux, billing, large-team administration).
- **Not built here:** signing, notarization, and an updater (before wider release); a non-Tailscale transport (after the MVP).

## Roles and ownership

Each role is one person and their coding agent. A role changes only the paths it owns; anything else goes through a pull request reviewed by the owner.

| Role | Area | Owns (paths) |
| --- | --- | --- |
| **A · Agent runtime** | formationd service, Claude Code (later Codex) harness, permission bridge, agent folders, schedules, detection, notifications | `crates/formationd/`, `crates/formation-core/src/{service,harness,agents,schedules,notify}/`, migrations `1000–1999`, **contract owner** of `crates/formation-proto/` and `crates/formation-core/src/ports.rs` |
| **B · Collaboration core** | Team and identity, Tailscale peer protocol, policy and approvals, ask pipeline, work requests, CLI and MCP teammate tools | `crates/formation-core/src/{team,peer,policy,ask,requests}/`, `crates/formation-cli/`, migrations `2000–2999` |
| **C · App** | Tauri client, tray, menus, windows, notifications bridge, IPC, store, view logic | `apps/desktop/src-tauri/`, `apps/desktop/src/{ipc,store,views}/`, window entries |
| **D · Design** | Design tokens, component styles, avatars, Korean and English copy, preview page, visual QA against M3 | `apps/desktop/src/styles/`, `apps/desktop/src/**/*.css`, `apps/desktop/src/i18n/`, `apps/desktop/src/preview/` |

Shared component `.tsx` files in `apps/desktop/src/components/` and `ui/` are co-owned by C and D; each change is reviewed by the other.

## How the parts join (integration contract)

1. **Contract first (TASK-002).** Before feature work, A writes `formation-proto`: every JSON-RPC method, event, peer endpoint payload, and error code in the tech spec, as Rust types with generated TypeScript. After it merges, changes are additive only; a breaking change needs approval from all four roles.
2. **Ports and adapters.** `formation-core/src/ports.rs` defines the traits that modules use to talk to each other: `Harness` (start, stop, send, steer, subscribe), `PermissionGate` (decide a tool call), `Policy` (access and visibility checks), `Peers` (send and receive), `Store` (repositories), `Notifier`. A module never imports another module's internals; `formationd` wires the real adapters.
3. **Fakes from day one (TASK-003).** `fake-formationd` serves recorded snapshots and events so C and D can build every screen before the core exists; `fake-claude` replays recorded `stream-json` so A can test supervision offline; `stub-whois` lets B test peers on one Mac. Each real piece replaces its fake by configuration (`FORMATION_FAKE=…`), not by code edits.
4. **No shared files between roles.** Migrations use numbered ranges per role (A `1000–1999`, B `2000–2999`); i18n keys are grouped per view; CSS lives beside each component.
5. **Branches and merges.** One branch and one pull request per task, named `feat/<area>-task-NNN` (for example `feat/harness-task-013`), squash-merged to `main` per `AGENTS.md`. `main` stays green: every pull request runs the checks in [Commands](#commands).
6. **Integration checkpoints.** All four join at IC-1, IC-2, and IC-3, run the checkpoint script below on real builds, and fix forward before starting the next phase.

## Commands

Only the documentation check exists today; the rest are added by TASK-001 and must then work.

| Command | Purpose | Status |
| --- | --- | --- |
| `node scripts/check-documentation.mjs` | Markdown links and anchors | Exists |
| `cargo test --workspace` | Rust unit and integration tests | Proposed (TASK-001) |
| `cargo clippy --workspace -- -D warnings` | Rust lint | Proposed (TASK-001) |
| `pnpm -C apps/desktop test` | Vitest | Proposed (TASK-001) |
| `pnpm -C apps/desktop lint` and `pnpm -C apps/desktop typecheck` | ESLint and TypeScript | Proposed (TASK-001) |
| `pnpm -C apps/desktop tauri dev` | Run the app in development | Proposed (TASK-001) |
| `cargo run -p formationd` | Run the service in the foreground for development | Proposed (TASK-010) |

## Phases

| Phase | Tasks | Parallel work | Ends with |
| --- | --- | --- | --- |
| 0 · Scaffold and contract | TASK-001 to TASK-003 | TASK-001 first (A); then TASK-002 (A) alongside TASK-003 (A for fakes in Rust, C for `fake-formationd` wiring) | Everyone can build against fakes |
| 1 · Integration proofs | TASK-004 to TASK-009 | All in parallel (A: 004, 005, 009; B: 006, 007; C: 008) | Risks in the tech spec answered |
| 2 · Chat with my agent | TASK-010 to TASK-016 | A, B, C, D in parallel | **IC-1** |
| 3 · Ask a teammate's agent | TASK-017 to TASK-022 | A, B, C, D in parallel | **IC-2** |
| 4 · Work requests and approvals | TASK-023 to TASK-027 | A, B, C, D in parallel | **IC-3 = today's milestone** |
| 5 · Complete the first version | TASK-028 to TASK-040 | Per role | **IC-4** |
| 6 · Codex | TASK-041 | A | **IC-5** |

## Tasks

### Phase 0 · Scaffold and contract

### TASK-001: Repository scaffold builds and runs an empty app
- Status: complete
- References: [tech spec · starter](4_techspec.md#starter), [code layout](4_techspec.md#code-layout)
- Prerequisites: none
- Change locations: proposed `Cargo.toml` (workspace), `crates/{formation-proto,formation-core,formationd,formation-cli}/`, `apps/desktop/` (from `pnpm create tauri-app`, TypeScript, React, TypeScript), `justfile` or `package.json` scripts, `.github/workflows/` (add Rust and frontend checks beside `documentation.yml`)
- Work: create the workspace; scaffold the Tauri app; confirm the template uses Vite; remove the sample command; add lint, test, and typecheck scripts; set the macOS deployment target to 13 (D-047).
- Acceptance criteria: `pnpm -C apps/desktop tauri dev` opens an empty window; all commands in [Commands](#commands) run green; CI runs them on pull requests.
- Verification: run each command; open a draft pull request and see CI pass.
- Owner: A

### TASK-002: The contract exists as types (formation-proto and ports)
- Status: pending
- References: [UI ↔ formationd](4_techspec.md#ui--formationd), [peer protocol](4_techspec.md#peer-protocol-over-the-tailnet), [CLI and MCP](4_techspec.md#cli-formation-and-mcp-server), [error codes](6_backend_architecture.md#error-codes), [data model](4_techspec.md#data-model)
- Prerequisites: TASK-001
- Change locations: `crates/formation-proto/src/`, `crates/formation-core/src/ports.rs`, generated `apps/desktop/src/ipc/generated.ts`
- Work: define every command, event (with a per-stream sequence number), snapshot shape, peer request and response, CLI JSON output, and error code; define the port traits; generate TypeScript (choose `ts-rs` or `tauri-specta`).
- Interfaces: produces all names used by every later task.
- Acceptance criteria: types compile; TypeScript is generated in the build; B and C review and approve.
- Verification: `cargo test -p formation-proto` (serialization round-trips); `pnpm -C apps/desktop typecheck`.
- Owner: A (reviewers B, C)

### TASK-003: Fakes let every role work without the others
- Status: pending
- References: [integration contract](#how-the-parts-join-integration-contract)
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/testkit/` (`fake-claude` replay, `stub-whois`), proposed `tools/fake-formationd/` with fixtures from the mockup's sample data, `THIRD_PARTY_NOTICES.md`, ownership map added to `AGENTS.md`
- Work: build the three fakes; create `THIRD_PARTY_NOTICES.md` with the format for copied code (D-051); add the role ownership table and the branch rule to `AGENTS.md`.
- Acceptance criteria: the app started with `FORMATION_FAKE=daemon` shows the M3 sample sidebar and a conversation from fixtures; `fake-claude` replays a recorded session.
- Verification: run the app against the fake; `cargo test -p formation-core testkit`.
- Owner: A (fixtures for the UI with C)

### Phase 1 · Integration proofs

### TASK-004: Claude Code process control is proven
- Status: pending
- References: [Claude Code integration](4_techspec.md#claude-code-integration-first-harness), FR-003, FR-022
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/harness/claude/` (spike), recorded fixtures in `testkit/fixtures/`
- Work: start `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --replay-user-messages --session-id <uuid>` in a folder; send a message; send a second while the first turn runs; stop a turn; resume with `--resume`; test whether a message can steer a running turn. Record outputs as fixtures.
- Acceptance criteria: a written result for each behavior (works, does not, workaround) in the task's pull request; fixtures committed.
- Verification: the spike's integration test against the real `claude` (marked ignored in CI).
- Blockers/assumptions: steering may not work in print mode; fallback is stop-then-send (TASK-039).
- Owner: A

### TASK-005: Permission routing covers every tool call
- Status: pending
- References: NFR-004, FR-015, FR-022, [tech spec assumptions](4_techspec.md#assumptions-to-verify-early)
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/harness/permission/` (spike), `crates/formation-cli/` (`formation mcp --permission` stub)
- Work: run Claude Code with `--permission-prompt-tool mcp__formation__permission` and an injected `PreToolUse` hook via `--settings`; try each permission mode and tool type (read, edit, bash, network); log which calls reach the tool and which reach the hook.
- Acceptance criteria: a coverage table showing that every write, command, and network call reaches Formation before it runs.
- Verification: the spike's logged run.
- Owner: A

### TASK-006: Forked answers do not change the original session
- Status: pending
- References: [ask pipeline](4_techspec.md#ask-pipeline), FR-012, NFR-002
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/ask/` (spike)
- Work: answer a question with `--resume <session> --fork-session --tools ""`; compare the original transcript before and after; measure time to first token and to the full answer.
- Acceptance criteria: the original transcript is byte-identical afterwards; the answer cites only conversation content; timings recorded.
- Verification: the spike's test.
- Owner: B

### TASK-007: Two Macs reach each other with verified identity
- Status: pending
- References: [identity](6_backend_architecture.md#identity-and-team), D-045, D-046
- Prerequisites: TASK-001
- Change locations: `crates/formation-core/src/peer/` (spike)
- Work: bind an HTTP server to the Mac's Tailscale address on port 7413; from another Mac call it; resolve the caller with `tailscale whois --json <addr>` (or the LocalAPI); read the GitHub login from `gh api user`.
- Acceptance criteria: the server logs the caller's Tailscale login and node; a request from outside the tailnet cannot connect.
- Verification: manual run on two Macs.
- Owner: B

### TASK-008: The Tauri shell shows the tray, the window, and fake data
- Status: pending
- References: [windows](5_frontend_architecture.md#windows), SC-002, SC-003
- Prerequisites: TASK-003
- Change locations: `apps/desktop/src-tauri/`, `apps/desktop/src/main.tsx`, `menubar.tsx`
- Work: tray icon (check for a duplicate icon on macOS 13 and latest), main window with traffic lights and sidebar vibrancy, menu bar panel window under the tray icon, connection to `fake-formationd` over the local socket.
- Acceptance criteria: one tray icon; the panel opens under it; the main window shows fixture data.
- Verification: run on macOS 13 and the latest macOS.
- Owner: C

### TASK-009: formationd runs as a LaunchAgent and outlives the app
- Status: in progress
- References: D-048, [system overview](4_techspec.md#system-overview)
- Prerequisites: TASK-001
- Change locations: `crates/formationd/`, `apps/desktop/src-tauri/` (registration call)
- Work: register `formationd` as a per-user LaunchAgent from the app (SMAppService, or a plist with `launchctl`); `KeepAlive`; confirm it survives quitting the app and restarts after a crash; open the app hidden from formationd.
- Acceptance criteria: quitting the app leaves formationd running; killing formationd restarts it; the app can be opened hidden.
- Verification: manual on macOS 13.
- Owner: A

### Phase 2 · Chat with my agent (IC-1)

### TASK-010: formationd serves snapshots and events over the local socket
- Status: pending
- References: [UI ↔ formationd](4_techspec.md#ui--formationd), NFR-007
- Prerequisites: TASK-002, TASK-009
- Change locations: `crates/formation-core/src/service/`, `crates/formationd/`
- Work: JSON-RPC 2.0 over a Unix socket with owner-only permissions; snapshot builder; event bus with sequence numbers; module wiring through ports.
- Acceptance criteria: a client gets a snapshot and then ordered events; a dropped client reconnects and resumes from a new snapshot.
- Verification: `cargo test -p formation-core service`.
- Owner: A

### TASK-011: Storage with per-role migrations
- Status: pending
- References: [data model](4_techspec.md#data-model), [storage](6_backend_architecture.md#storage-and-data-ownership)
- Prerequisites: TASK-002
- Change locations: `crates/formation-core/src/store/` (B), migrations `2000–2999` (B) for team, questions, answers, requests, approvals, rules, activity; migrations `1000–1999` (A) for agents, sessions, schedules
- Work: SQLite in WAL mode at the documented path; migration runner; repositories behind the `Store` port.
- Acceptance criteria: a fresh start creates the schema; re-running is a no-op; A and B add tables without touching each other's files.
- Verification: `cargo test -p formation-core store`.
- Owner: B (A adds its tables in TASK-012)

### TASK-012: Agent folders are created and edited
- Status: pending
- References: FR-001, FR-004, US-002, US-004, SC-004, SC-006
- Prerequisites: TASK-010, TASK-011
- Change locations: `crates/formation-core/src/agents/`, A's migrations
- Work: create the folder layout (`AGENTS.md`, `CLAUDE.md` with `@AGENTS.md`, `.claude/skills/`, `formation.toml` with name, role, harness, avatar); list agents; read and write files; watch for outside edits; harness readiness check.
- Acceptance criteria: matches US-002 and US-004 criteria, including "harness not ready" still creating the folder.
- Verification: unit tests; create an agent from the app (after TASK-015).
- Owner: A

### TASK-013: Claude Code agents run, queue, and stop under formationd
- Status: pending
- References: FR-003, FR-022, US-003, US-024, [Claude Code integration](4_techspec.md#claude-code-integration-first-harness), TASK-004 results
- Prerequisites: TASK-004, TASK-010, TASK-012
- Change locations: `crates/formation-core/src/harness/claude/`
- Work: one supervisor per running agent; send; queue while working; stop (after this turn or now); resume on restart; emit `agent.state`, `agent.stream`, `agent.queue`; stalled after 10 minutes. Reuse Reason Network `src/claude.rs` where it fits (record in `THIRD_PARTY_NOTICES.md`).
- Acceptance criteria: US-003 and US-024 criteria, except steering (TASK-039) and permission cards (TASK-024).
- Verification: tests with `fake-claude`; manual run with real Claude Code.
- Owner: A

### TASK-014: The app's store mirrors formationd
- Status: pending
- References: [data flow](5_frontend_architecture.md#data-flow)
- Prerequisites: TASK-002, TASK-008
- Change locations: `apps/desktop/src/ipc/`, `apps/desktop/src/store/`, `apps/desktop/src-tauri/` (socket client)
- Work: typed command calls; snapshot then events; gap detection; pending states within 100 ms; reconnect banner.
- Acceptance criteria: works against `fake-formationd` and, after TASK-010, the real service.
- Verification: reducer unit tests; manual reconnect test.
- Owner: C

### TASK-015: Sidebar and conversation with my agent work end to end
- Status: pending
- References: SC-003, SC-004, SC-005, SC-020; M3 states for those screens; US-001, US-002, US-024
- Prerequisites: TASK-014 (real data after TASK-013)
- Change locations: `apps/desktop/src/views/{my-conversation,new-agent}/`, `apps/desktop/src/components/{sidebar,info-panel}/`
- Work: chat list with state dots and Needs-you line; new agent sheet with harness and avatar pickers; conversation with bubbles, step groups, queued chip, composer with Stop; info panel basics.
- Acceptance criteria: the screens match M3 states working, idle, queued, stopped, error, stop-confirm.
- Verification: component tests; D's visual check on the preview page.
- Owner: C

### TASK-016: Tokens, avatars, and copy for IC-1 screens
- Status: pending
- References: [design system](3_screen_list_and_user_flows.md#visual-direction-and-design-system), [design system in code](5_frontend_architecture.md#design-system-in-code), D-042, D-050
- Prerequisites: TASK-001
- Change locations: `apps/desktop/src/styles/tokens.css`, component stylesheets, `apps/desktop/src/i18n/{ko,en}.json`, `apps/desktop/src/preview/`, `components/AgentAvatar`
- Work: port the mockup tokens (light and dark); style IC-1 components; Korean and English strings; preview page with each IC-1 component state; avatar stills via `open-bot-avatar`.
- Acceptance criteria: preview page shows IC-1 components in all M3 states in both themes and both languages.
- Verification: D's review against M3 screenshots.
- Owner: D

**IC-1 · Chat with my agent (one Mac):** create an agent, start it, chat, send a second message while it works (queued), stop after this turn; quit the app (⌘Q) and check the agent keeps running; reopen and check the conversation and state are restored.

### Phase 3 · Ask a teammate's agent (IC-2)

### TASK-017: Team creation and joining
- Status: pending
- References: FR-008, US-008, US-020, SC-001, SC-013, D-046, TASK-007 results
- Prerequisites: TASK-007, TASK-010, TASK-011
- Change locations: `crates/formation-core/src/team/`, B's migrations
- Work: create team; expiring invitation code; join over Tailscale with GitHub login (MVP: from `gh api user`); roster; WhoIs-to-member mapping; removal.
- Acceptance criteria: US-008 and US-020 criteria.
- Verification: two-Mac manual test; unit tests for expiry and roster.
- Owner: B

### TASK-018: Peer protocol with outbox
- Status: pending
- References: [peer protocol](4_techspec.md#peer-protocol-over-the-tailnet), FR-016
- Prerequisites: TASK-017
- Change locations: `crates/formation-core/src/peer/`
- Work: `GET /v1/agents`, `POST /v1/ask`, `WS /v1/events`; outbox with retry; presence events.
- Acceptance criteria: requests from non-members get `403`; offline peers queue and deliver on return.
- Verification: two-instance loopback test with `stub-whois`.
- Owner: B

### TASK-019: Sharing, routing, and answers from the session conversation
- Status: pending
- References: FR-009 to FR-013, US-009, US-013 to US-017, [ask pipeline](4_techspec.md#ask-pipeline), D-025 to D-027, TASK-006 results
- Prerequisites: TASK-013, TASK-018
- Change locations: `crates/formation-core/src/{policy,ask}/`
- Work: share settings with defaults (private; answer and excerpts; requests off); share preview; routing with confident and unsure outcomes; forked answer with excerpts, `as_of`, staleness, masking; follow-ups and refresh.
- Acceptance criteria: US-009 and US-013 to US-017 criteria; private agents never appear; the original session is unchanged.
- Verification: unit tests (routing thresholds, masking, visibility); two-Mac manual test.
- Owner: B

### TASK-020: Asking screens
- Status: pending
- References: SC-008, SC-013, SC-014, SC-015, SC-001; M3 states
- Prerequisites: TASK-014 (real data after TASK-019)
- Change locations: `apps/desktop/src/views/{ask-conversation,member,team,sharing,welcome}/`
- Work: asking conversation (all ten M3 states), member's agents, team page, sharing section with preview and unshare confirmation, welcome and join.
- Acceptance criteria: matches M3 for those screens.
- Verification: component tests; D's visual check.
- Owner: C

### TASK-021: Styles and copy for IC-2 screens
- Status: pending
- References: as TASK-016
- Prerequisites: TASK-016
- Change locations: as TASK-016
- Work: answer bubble with source footer and excerpts, choice card, waiting card, teammate avatars with owner badge, copy, preview entries.
- Acceptance criteria: preview page covers IC-2 components.
- Verification: D's review.
- Owner: D

### TASK-022: A's support for asking
- Status: pending
- References: FR-012, [ask pipeline](4_techspec.md#ask-pipeline)
- Prerequisites: TASK-013
- Change locations: `crates/formation-core/src/harness/claude/`
- Work: expose the agent's current session ID and transcript path through the `Harness` port; a helper that starts a forked, tool-less run for `ask`.
- Acceptance criteria: `ask` gets everything it needs through the port without reading harness internals.
- Verification: unit test with `fake-claude`.
- Owner: A

**IC-2 · Ask a teammate's agent (two Macs):** Minsu joins Dohyeon's team; Dohyeon shares Backend; Minsu asks Dohyeon "how far is the login work?" and gets an answer naming Backend with `as_of` and excerpts; Minsu asks an ambiguous question and picks from the choice card; a private agent never appears; Dohyeon's Backend transcript is unchanged.

### Phase 4 · Work requests and approvals (IC-3, today's milestone)

### TASK-023: Work request lifecycle
- Status: pending
- References: FR-014, US-019, SC-016
- Prerequisites: TASK-018, TASK-019
- Change locations: `crates/formation-core/src/requests/`, peer endpoints `POST /v1/requests`, `GET /v1/requests/{id}`, `POST /v1/requests/{id}/cancel`
- Work: create with idempotency; status machine; "accepts requests" check; cancel before start; deliver to A's executor through the port.
- Acceptance criteria: US-019 criteria; a repeated idempotency key returns the original request.
- Verification: unit tests (idempotency under concurrency); two-Mac test.
- Owner: B

### TASK-024: Requests run in the main session behind the permission gate
- Status: pending
- References: FR-014, FR-015, FR-022, NFR-004, TASK-005 results
- Prerequisites: TASK-013, TASK-023
- Change locations: `crates/formation-core/src/harness/{claude,permission}/`, `crates/formation-cli/` (`formation mcp --permission`)
- Work: request executor (after the current turn); permission tool and `PreToolUse` hook calling the `PermissionGate` port; the owner's own permission prompts surfaced as cards (proposed default 11).
- Acceptance criteria: no write, command, or network call from a teammate's request runs without a decision; the owner's own prompts appear in the conversation.
- Verification: tests with `fake-claude`; manual with real Claude Code.
- Owner: A

### TASK-025: Approvals: allowed reads, allow once, deny, expiry
- Status: pending
- References: FR-015, US-011, D-028, SC-011
- Prerequisites: TASK-019, TASK-023
- Change locations: `crates/formation-core/src/policy/`
- Work: always-allowed reads inside the agent folder and assigned repositories; raise approvals; decide allow once or deny; 24-hour expiry; notify requester. (Saved rules come in TASK-028.)
- Acceptance criteria: US-011 criteria except "keep allowing similar" and "allow all".
- Verification: table-driven tests, including path escapes.
- Owner: B

### TASK-026: Request and approval screens
- Status: pending
- References: SC-005 (approval and permission states), SC-010, SC-011, SC-016; M3 states
- Prerequisites: TASK-014 (real data after TASK-024 and TASK-025)
- Change locations: `apps/desktop/src/views/{work-request,inbox,approval,my-conversation}/`
- Work: request compose and status sheets; request and permission cards; inbox approvals group; approval sheet (default, deny-confirm, expired) with no Return binding and Esc asking first.
- Acceptance criteria: matches M3 for those states; keyboard rules hold.
- Verification: component tests for keyboard rules; D's visual check.
- Owner: C

### TASK-027: Styles and copy for IC-3 screens
- Status: pending
- References: as TASK-016
- Prerequisites: TASK-021
- Change locations: as TASK-016
- Work: approval sheet, request and permission cards, inbox, status timeline; copy; preview entries.
- Acceptance criteria: preview page covers IC-3 components.
- Verification: D's review.
- Owner: D

**IC-3 · Today's milestone (two Macs):** after IC-2, Minsu sends "Add token expiry tests" to Dohyeon's Backend; it waits for the current turn; when it needs `npm test`, Dohyeon sees the approval and allows once; Minsu sees the status move to completed. Repeat with Deny and check the command never runs. Quit Dohyeon's app during the request and confirm it continues.

### Phase 5 · Complete the first version (IC-4)

| Task | Outcome | Owner | References | Prerequisites |
| --- | --- | --- | --- | --- |
| TASK-028 | Saved rules: keep allowing similar, allow all, limits, revoke | B | FR-015, US-012 | TASK-025 |
| TASK-029 | Rules page and keep-similar state of the approval sheet | C | SC-011, SC-012 | TASK-028 |
| TASK-030 | Schedules with missed-on-wake runs | A | FR-005, US-005 | TASK-013 |
| TASK-031 | Schedules panel and sheet | C | SC-007 | TASK-030 |
| TASK-032 | Detect and connect agents started elsewhere, Hermes and OpenClaw capability report (reuse Reason Network `src/daemon/sessions.rs`, `src/setup/hermes.rs`) | A | FR-006, US-006 | TASK-013 |
| TASK-033 | Connect agents sheet and read-only conversation | C | SC-009, SC-005 detected | TASK-032 |
| TASK-034 | Monitoring and notifications; open the app hidden for alerts | A | FR-007, US-007 | TASK-009, TASK-013 |
| TASK-035 | Menu bar panel, notifications bridge, settings window | C | SC-002, SC-018, SC-019 | TASK-034 |
| TASK-036 | CLI and MCP teammate tools with chain limits | B | FR-017 to FR-019, US-022, US-023, IF-001, IF-002 | TASK-023 |
| TASK-037 | Offline outbox re-checks, deletion propagation, activity record | B | FR-013, FR-016, FR-020 | TASK-018 |
| TASK-038 | History page and Inbox "questions sent to me" (answer, hand to agent) | C | SC-010, SC-017, US-010 | TASK-037 |
| TASK-039 | Steer now (or the stop-then-send fallback), edit and withdraw queued messages | A, then C for UI | FR-022, TASK-004 results | TASK-013 |
| TASK-040 | Preview page complete, Korean and English pass, live avatars, VoiceOver pass | D with C | NFR-009, D-033, D-042, D-050 | TASK-026 |

Each Phase 5 task uses the same fields as the tasks above when it starts: change locations from the [ownership table](#roles-and-ownership), acceptance criteria from its stories, verification by tests plus the matching M3 states.

**IC-4 · First version complete:** every story US-001 to US-024 passes its Given/When/Then on two Macs; every M3 state exists in the app and the preview page.

### Phase 6 · Codex (IC-5)

### TASK-041: Codex agents through app-server
- Status: pending
- References: D-002, [Codex note](4_techspec.md#claude-code-integration-first-harness), Reason Network `src/codex.rs`
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
| FR-015 Approve actions | US-011, US-012, US-019 | SC-011, SC-012 | `policy`, `harness::permission` | 005, 024, 025, 026, 028, 029 | IC-3, IC-4 |
| FR-016 Owner offline | US-018 | SC-015 offline-waiting | `peer` outbox | 018, 037 | IC-4 |
| FR-017 CLI | US-022 | IF-001 | `formation-cli` | 036 | CLI tests; IC-4 |
| FR-018 MCP | US-023 | IF-002 | `formation-cli` (rmcp) | 036 | MCP tests; IC-4 |
| FR-019 Agent-to-agent safeguards | US-022, US-023 | IF-001, IF-002 | `policy::limits` | 036 | Unit tests |
| FR-020 Activity record | US-012 | SC-017 | `store` | 037, 038 | IC-4 |
| FR-021 Channel-ready design | — (design requirement) | — | Agent folder model leaves room for channels | 012 | Review of the folder layout |
| FR-022 Chat with my agent | US-024 | SC-005 | `harness::claude`, `harness::permission` | 004, 013, 015, 024, 039 | IC-1, IC-3 |
| NFR-001 Answer time | US-013 | SC-015 | `ask` | 006, 019 | Timings from TASK-006; IC-2 |
| NFR-002 No interruption | US-013 | — | Fork with no tools | 006, 019 | Transcript comparison |
| NFR-003 Sensitive content | US-009 | SC-008 preview | Masking | 019 | Unit tests |
| NFR-004 Execution safety | US-011 | SC-011 | Permission gate and hook | 005, 024, 025 | Coverage table; IC-3 deny check |
| NFR-005 Freshness | US-013 | SC-015 stale | `ask` | 019 | Unit tests |
| NFR-006 Connection security | — | — | Tailscale plus roster | 007, 017, 018 | Non-member `403` test |
| NFR-007 Reliability | — | — | Snapshot and events; recovery | 009, 010, 023 | Recovery tests |
| NFR-008 Resource use | — | — | Background service; light UI | 009, 010 | Measure at IC-4 |
| NFR-009 Accessibility | — | All | Radix, labels, keyboard | 015, 020, 026, 040 | VoiceOver pass |

## Execution instructions for the next AI

1. Read the documents in the [order above](#read-first-in-this-order) and the approved mockup M3. Check your role in [Roles and ownership](#roles-and-ownership) and change only the paths your role owns.
2. Pick the first task whose prerequisites are complete and whose owner is your role. Set its `Status` line to `in progress` (status-only edits do not reopen this plan's review).
3. Before writing a module from scratch, check the [reuse plan](6_backend_architecture.md#reuse-plan-d-051) and the [frontend reuse list](5_frontend_architecture.md#reuse-from-open-source-competitors-d-051). Copy only from MIT or Apache-2.0 sources, keep their notices, and add each copied file to `THIRD_PARTY_NOTICES.md`. Never copy from HAPI, Claude Squad, or Flowpad.
4. Work on a branch named `feat/<area>-task-NNN`, commit with the format in `AGENTS.md`, and open a pull request using the template's sections.
5. Verify with the task's verification steps and the [commands](#commands). Do not mark a task complete on unrun checks; record what you ran.
6. If a contract in `formation-proto` must change, open a separate pull request for the contract and get approval from all four roles first.
7. If a change alters behavior or screens in the approved documents or mockup, stop and ask the user before continuing; update the owning document in the same change when they agree.
8. Set `Status` to `complete` when the acceptance criteria pass, and note the verification in the pull request.
