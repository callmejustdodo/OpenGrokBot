# Formation product plan

**Don’t ask your teammate. Ask their agent.**

Version 0.1 · October 9, 2026 · Planning stage

## Product definition

Formation is an open-source macOS app and CLI that lets a machine owner share selected local Claude Code and Codex sessions with collaborators and their agents. Collaborators can read approved context, ask questions in a separate side conversation, and submit follow-up prompts or work requests under the owner's permissions.

The agent runs on the owner's machine. Formation does not transfer the owner's provider credentials to collaborators. Publishing the source code does not make anyone's sessions public.

All features and commands in this document are proposals, not implemented functionality.

## Problem and audience

Coding sessions accumulate implementation decisions, unsuccessful approaches, investigation results, and project context. Teammates often need the owner to summarize or copy that context before they can continue their own work. Agents face the same problem when a person must relay questions between them.

Formation aims to reduce that coordination work. Whether it saves time and creates a recurring team habit remains a hypothesis to test.

| User | Need |
|---|---|
| Session owner | Choose sessions and collaborators; control local execution and revoke access. |
| Collaborator | Ask about implementation decisions and request follow-up work. |
| Collaborator's agent | Query another session through a stable interface and consume structured results. |

## Requirements and proposed choices

Established requirements:

- A local macOS app, with Swift preferred and Electron acceptable.
- Access to local Claude Code and Codex sessions.
- Owner permissions: allow once, keep allowing similar commands, or allow all.
- Side conversations inspired by sidechat and `/btw` experiences.
- A CLI that allows agents to query a teammate's sessions directly.
- An open-source repository.

Proposed choices:

- SwiftUI for the owner-facing app, backed by a local service.
- Side questions as the default interaction; main-session requests as a separate action.
- One shared policy engine for the app, CLI, and any future web or MCP interface.
- Managed executions first, followed by existing-session integrations where supported.
- An optional collaborator web UI after the first end-to-end workflow works.

## Interaction model

### Share a session

The owner selects a supported session, reviews the context and tool output that will be exposed, and chooses collaborators and access levels. Invitations expire. A new connection is visible to the owner.

Sharing a limited history range must also limit what the answering agent receives. Forking an unrestricted source session would defeat a restricted sharing boundary.

### Ask a side question

A collaborator asks, for example, “Why did you choose JWT for this authentication flow?” Formation starts a separate conversation using approved context from a specific revision. Follow-up questions stay in that conversation and do not automatically enter the main session's history or queue.

Responses identify their source session and context revision. Where available, they also identify a Git commit. The user can explicitly refresh context as the main session progresses.

The initial mode answers from shared history without tools. A later file-reading mode may read only approved project paths. Forking conversation history does not isolate the filesystem: side conversations must not write to the original working directory. Use a file snapshot or isolated worktree where consistent reads are necessary.

### Submit work to a session fork

“Add tests for token expiration” is a work request that runs in its own session fork ([D-056](0_one_pager.md#planning-progress)). Formation previews the prompt that will be delivered and submits it according to the owner's policy. By default, requests wait until the current turn finishes. Mid-turn intervention requires a separate explicit permission.

A side conversation can be promoted into a session-fork request by selecting the relevant messages. The entire side conversation is not attached automatically.

### Disconnect or revoke

Revocation blocks new access and pending requests. The owner separately chooses whether to interrupt an already-running task. Content already delivered to a collaborator cannot be recalled.

Sleeping or offline Macs cannot execute requests. Reconnection must recheck permissions and expiration rather than blindly replaying old work.

## Permission model

Access permissions and execution permissions are separate. Accepting a prompt does not approve every command the agent may generate from it.

| Access level | Capability |
|---|---|
| Read history | Read selected messages and results. |
| Ask side questions | Generate answers from approved context; tool access is controlled separately. |
| Submit work | Add requests to session forks, subject to execution policy ([D-056](0_one_pager.md#planning-progress)). |

| Approval choice | Meaning |
|---|---|
| Allow once | Approve one specific request or tool invocation. |
| Keep allowing similar commands | Save an owner-reviewed structural rule. |
| Allow all | Allow supported actions within an explicitly selected collaborator, session, and project scope. Machine-wide access requires a separate explicit scope. |

“Similar” must mean a deterministic rule, not model judgment or text similarity. A rule can include collaborator identity, session, executable, allowed arguments, working directory, file paths, network destination, and expiration. A permitted test command must not silently permit shell chaining or execution in another directory.

The approval UI shows the requesting person and agent, purpose, target session, actual command or file change, and permission scope. Grants are inspectable and revocable. Owners can set time and usage limits.

The owner's local service makes the final decision. A relay or collaborator client cannot manufacture approval. Apply Formation policy alongside provider policy, respecting the narrower restriction. Verify enforcement against provider auto-approval paths; do not assume an approval callback sees every tool call. The [Claude SDK permission documentation](https://code.claude.com/docs/en/agent-sdk/permissions) explicitly describes evaluation paths that can bypass `canUseTool`.

Preserve request provenance and record requests, decisions, and results locally. Secret detection is a supplementary check, not a substitute for sharing previews. Provider authentication tokens stay on the host.

## Proposed CLI

```sh
formation sessions list --peer minsu --json

formation ask minsu/auth "What are the login API error codes?" --json

formation request minsu/auth "Add token expiration tests" \
  --idempotency-key task-42 --json

formation requests get req_123 --json
formation requests cancel req_123 --json
```

`ask` targets a side conversation. `request` submits work to a session fork ([D-056](0_one_pager.md#planning-progress)). Approval-pending requests return an ID and `pending_approval`; agent callers do not hang on an interactive terminal prompt. An optional wait mode has a timeout.

Example completed response:

```json
{
  "request_id": "req_123",
  "status": "completed",
  "session_id": "auth",
  "mode": "side",
  "context_revision": "rev_42",
  "answer": "...",
  "sources": [],
  "usage": {}
}
```

Define stable schemas and exit codes. Distinguish received, awaiting approval, queued, running, completed, denied, failed, canceled, and expired requests. Distinguish partial streaming output from completion.

Repeated idempotency keys return the existing request instead of executing twice. Serialize writes to a main session. Preserve results across client disconnects. Record the human principal, agent identity, originating task, and call-chain ID.

Limit duration, cost, request count, and call-chain depth to prevent agents repeatedly querying each other. A remote response cannot grant permissions or become an owner instruction. Consider MCP after the CLI, using the same policy and service.

## Architecture

| Component | Responsibility |
|---|---|
| SwiftUI app | Session selection, sharing, approvals, connection status, request history, and revocation. |
| Local service | Authentication, policy, queues, audit records, provider adapters, and streaming. |
| Provider adapters | Normalize supported discovery, read, resume, fork, prompt, and approval operations. |
| CLI and optional web UI | Human and agent request interfaces. |
| Connection layer | Authenticated encrypted transport, discovery, and optional relay or self-hosting. |

SwiftUI fits the macOS-first direction. A Node or Python helper may be necessary for SDK integration; a native UI does not imply a single-runtime application. Electron remains an alternative if web UI reuse or cross-platform delivery becomes the priority. Resource efficiency must be measured, not assumed.

### Codex integration

The official [Codex App Server](https://learn.chatgpt.com/docs/app-server) provides thread operations, turn execution, and approval requests. It is a candidate integration surface.

Reading or resuming stored history is different from attaching to an execution already owned by another process. Do not assume Formation can inject prompts into any existing Codex desktop session. Verify process ownership, supported versions, and duplicate-execution prevention.

[HAPI documents shared Codex executions](https://hapi.run/docs/guide/codex-shared-sessions), where supported terminal and remote clients use one app-server. This is an implementation reference, not proof that arbitrary unmanaged sessions are attachable.

### Claude Code integration

The [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/sessions) supports session resume and fork. Permission rules, callbacks, and hooks provide integration points for execution control.

Injecting input into an arbitrary running CLI process is a separate compatibility question. Start with executions Formation manages and expose read or resume capabilities for other sessions only when supported.

### Transport and state

Prototype with authenticated communication between two machines. Choose direct connectivity, a private network, or a relay after testing installation and reachability. Keep policy decisions on the host. If using a third-party relay, specify end-to-end encryption and which routing metadata remains visible.

Use common request IDs and ordered events across clients. Reconnection and pending queues must not bypass revocation.

## MVP and acceptance criteria

1. **Integration proof:** test both providers for discovery, side questions, prompt delivery, approval interception, reconnection, and process ownership.
2. **First usable version:** two Macs and one collaborator; sharing preview, expiring invitation, side questions, CLI JSON responses, approvals, revocation, and audit records. Complete one provider's workflow first.
3. **Work-request extension:** main-session queue, three approval choices, structural rules, idempotency, cancellation, and a second provider adapter.
4. **Later expansion:** collaborator web UI, MCP, session search, multiple collaborators, self-hosting, and team policy.

Acceptance criteria:

- Side questions do not alter main-session history or its work queue.
- Side-question mode cannot write to the source working directory.
- Unauthorized collaborators cannot discover or read sessions.
- Tool execution can still wait for approval after prompt acceptance.
- Revocation prevents new and pending work from executing.
- Retries do not execute the same request twice.
- Concurrent main-session inputs preserve a visible order.
- Offline, sleep, and provider-exit states reflect reality.
- Unsupported attachment and resume operations explain their limitations.
- Formation policy remains effective on provider auto-approval paths.

## Validation and open decisions

In a small-team pilot, measure repeated context explanations, question resolution, main-task interruptions, approval burden, and repeat usage. Compare agent-to-agent queries with manual message relay. Establish metric targets after collecting a baseline.

Measure Formation separately from provider processes: idle memory and CPU, one question, streaming, and multiple sessions. No performance advantage has been demonstrated yet.

Decide the first provider, default history scope, execution defaults, transport, and applicable provider account and usage conditions. Billing and large-team administration are proposed exclusions from the first MVP.

Before building the full app, test the same workflow in Happier, Reason Network, Flowpad, Agent Relay, and Paperclip. If an existing product already meets the need, an extension may be a better starting point. See the [competitive landscape](competitive-landscape.md).
