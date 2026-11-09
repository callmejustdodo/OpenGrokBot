# OpenGrokBot — Product requirements

Rev 2 · 2026-11-09 · Based on the [one-pager](0_one_pager.md) (Rev 6). Rev 2 adds FR-022, chatting with one's own agent (D-035). Decision IDs (D-xxx) refer to the [planning progress](0_one_pager.md#planning-progress) record.

## Goals and boundaries

OpenGrokBot is a Mac app that runs and manages a person's role-based agents and lets teammates ask those agents directly. The first version serves one team of four, each member running several agents on their own Mac.

- **Goal 1:** a teammate gets the state of someone's work from the agent doing it, without waiting for the person (target: about 5 minutes instead of about 2 hours, D-018).
- **Goal 2:** an owner can run, watch, and shape all of their agents in one place, including giving them work by chat.
- **Boundary:** every capability below is required for the first version (D-023). The order of delivery is set in the implementation plan. Items under [Exclusions](#exclusions) are out.

## Terms

| Term | Meaning |
| --- | --- |
| Owner | The person whose Mac runs an agent. Every team member is the owner of their own agents and a teammate to the others. |
| Teammate | Another member of the same team, asking or requesting work from an owner's agents. |
| Teammate's agent | An agent acting for a teammate through the CLI or MCP server. |
| Agent | A role agent defined by one dedicated folder (instructions, skills, memory, schedules), run by Claude Code or Codex (D-009, D-012). Roles are owner-defined and not limited to coding (D-021). |
| Detected agent | An agent OpenGrokBot did not create: a Claude Code or Codex session started elsewhere, or a Hermes or OpenClaw instance on the Mac (D-010). |
| Session | One conversation of an agent with its runtime. An agent can have many sessions over time; its main session is the one currently doing work. |
| Side conversation | A separate conversation that answers a teammate from an agent's session conversation, without touching the main session. |
| Work request | A teammate's request for an agent to do something in a fork of its session (FR-014). |

## Platform

- **App:** a macOS desktop app installed on all four Macs, used both as owner and as teammate (D-001, D-007). The minimum macOS version is set in the tech spec.
- **Agent interfaces:** a CLI and an MCP server for teammates' agents (D-022).
- **Runtimes:** Claude Code and Codex must be installed and signed in on the owner's Mac. OpenGrokBot does not handle provider sign-in, and provider credentials never leave that Mac.
- **Availability:** an owner's agents run and answer only while the owner's Mac is awake and online.
- **macOS permissions:** notifications, for monitoring alerts. Any other permission is justified in the tech spec.
- **Distribution:** direct to the team, outside the Mac App Store; details in the tech spec.

## Roles and permissions

| Role | Can | Cannot |
| --- | --- | --- |
| Owner | Everything for their own agents: create, run, edit, schedule, share, set visibility, approve, revoke, delete history. | Act on another member's agents except as a teammate. |
| Teammate | See an owner's shared agents and their state; ask; send work requests to agents that accept them; see and delete their own question history. | See private agents; see more than the agent's visibility setting allows; approve anything on the owner's Mac. |
| Teammate's agent | Everything a teammate can do through the CLI or MCP, on behalf of that teammate. | Grant permissions; turn a remote answer into an instruction for the owner's agents. |

Only the owner's Mac decides what runs on it. A teammate's app, CLI, or MCP client cannot create or forge an approval.

## Functional requirements

All requirements are **Must** for the first version.

### Managing agents (owner)

**FR-001 Create an agent.** The owner creates an agent by giving a name, a role in free text, a runtime (Claude Code or Codex), and optional starting instructions. OpenGrokBot creates the agent's dedicated folder with its instructions, skills, memory, and schedules in a documented layout. *Proposed:* a coding agent's folder also lists the repositories it works in.
- A new agent appears in the overview as stopped and private (D-026).
- The folder opens in Finder from the app, and its instructions file contains what the owner entered.
- Any role can be created, for example marketing or customer support, not only coding.

**FR-002 See all agents.** The overview lists every agent on the Mac: those OpenGrokBot created, detected Claude Code and Codex sessions, and detected Hermes and OpenClaw instances. Each row shows name, role, runtime or harness, state (working, idle, waiting for approval, stopped, error, unknown), last activity time, shared or private, and pending teammate requests.
- A state change appears in the overview within 5 seconds (assumed target).
- A detected agent is labeled as detected, and states OpenGrokBot cannot read show as unknown rather than guessed.

**FR-003 Start, stop, restart.** The owner starts, stops, and restarts agents OpenGrokBot created.
- Start runs the agent's runtime in the agent's folder.
- Stopping an agent in the middle of a task asks whether to stop now or after the current turn.
- Restart continues the latest session when the runtime supports it, and says so when it starts a new one.
- For detected agents, these controls appear only where the harness supports them; otherwise the app explains why they are unavailable.

**FR-004 Edit instructions and skills.** The owner edits an agent's instructions and adds, edits, or removes its skills in the app. Changes are written to the agent's folder.
- The app states when a change takes effect (for example, from the next session).
- Edits made directly in the folder outside the app appear in the app.

**FR-022 Chat with my agent.** The owner opens an agent's conversation and sends messages that go to the agent's main session, giving it work directly (D-035). The agent's replies and the steps it takes (files changed, commands run, approvals asked) appear in the same conversation, so the conversation is also the agent's activity view.
- Sending to an idle agent starts a turn with the message, and the reply streams into the conversation.
- A message sent while the agent is working waits and is delivered when the current turn ends; the waiting message is shown as queued and can be edited or withdrawn until it is delivered.
- The owner can instead choose **Steer now**, which delivers the message into the running turn at the next point the runtime allows, or **Stop**, which ends the turn (FR-003).
- *Proposed:* permission requests that the runtime raises for the owner's own work appear in the conversation for the owner to answer. Teammates' requests still follow FR-015.
- For a detected session, the conversation shows its activity read-only; work through OpenGrokBot uses forks (FR-006).
- Teammates never see this conversation; they see only side-conversation answers within the agent's visibility setting (FR-009, FR-012).

**FR-005 Schedule work.** The owner gives an agent a recurring task: a prompt and a schedule (for example, every weekday at 09:00).
- Each run is recorded with its start time, result, and any approvals it needed.
- Scheduled runs follow the same approval rules as other work.
- *Proposed:* a run missed because the Mac was asleep or off is marked missed and runs once when the Mac wakes.

**FR-006 Detect and connect other agents.** OpenGrokBot finds Claude Code and Codex sessions started outside OpenGrokBot, and Hermes and OpenClaw instances on the Mac. The owner can connect a detected agent, which adds it to the overview and makes it shareable.
- OpenGrokBot reads detected sessions and works only in forks; it never resumes or writes to the original session (D-056).
- OpenGrokBot does not create new agents on Hermes or OpenClaw (D-012).
- For each detected agent, the app shows which operations work (state, reading the conversation, answering teammates, receiving work requests) and which do not, with the reason.
- Detection does not change the detected agent's own configuration without the owner's consent.

**FR-007 Monitoring.** OpenGrokBot alerts the owner only when something needs them (D-019, delegated):
- an agent is waiting for approval;
- an agent hits an error or exits unexpectedly;
- an agent is working but has shown no activity for 10 minutes (assumed threshold);
- a teammate's work request is waiting;
- a scheduled run failed or was missed.

Everything else stays visible in the overview without alerts. Alerts use macOS notifications and a badge, and the owner can mute an agent.

### Team and sharing

**FR-008 Team membership.** A member creates the team and invites others with an invitation link that expires. An invitee installs the app, accepts, and appears in the member list with an online or offline indicator. *Proposed:* the member who created the team can remove members; removal ends access at once.

**FR-009 Share an agent.** Each agent is private until its owner shares it with the team (D-026). Per agent, the owner sets:
- **Visibility:** answer only, or answer plus the conversation excerpts it drew on. Default: answer plus excerpts. The same setting applies to every teammate (D-025).
- **Work requests:** accepted or not. *Proposed default:* not accepted until the owner turns it on.
- **Preview:** before sharing, the owner sees an example of what a teammate would receive from this agent.

Unsharing an agent blocks new questions and requests at once and cancels pending ones. A task already running continues unless the owner stops it. Content already delivered cannot be recalled.

### Asking

**FR-010 Ask a person.** A teammate picks a member and asks a question, for example "how far is the login work?". OpenGrokBot on the owner's Mac picks the shared agent most able to answer, using the agents' roles, instructions, and recent session content (D-015, D-016).
- When confident, the chosen agent answers and the answer names it.
- When not confident, the asker chooses (D-027) from one prompt that shows the best guess first, the other candidate agents, and "ask the owner."
- "Ask the owner" sends the question to the owner, who can answer it or hand it to an agent.
- Only shared agents are considered.

**FR-011 Ask an agent.** A teammate browses a member's shared agents (name, role, state, last activity) and asks one directly.

**FR-012 Answer from the session conversation.** The answer is produced on the owner's Mac in a side conversation that reads the agent's session conversation (D-017).
- The main session's conversation and work queue are not changed, and its current task is not delayed (D-024).
- The side conversation cannot write files, run commands, or use tools that change anything.
- Every answer states the agent, the time of the latest conversation content it used, and, if visibility allows, the excerpts it relied on.
- When the conversation does not establish something, the answer says so rather than guessing (D-024).
- When the agent's latest activity is older than 1 hour (assumed threshold), the answer is marked as possibly out of date.

**FR-013 Follow-ups and history.** The asker can continue in the same side conversation and refresh it to the agent's latest content. Askers see their own questions; owners see every question asked of their agents. Questions and answers are kept until deleted (D-029).
- *Proposed:* askers can delete their own history; owners can delete any question and answer about their agents.

### Work requests and approvals

**FR-014 Request work.** A teammate sends a work request to a shared agent that accepts requests, either written fresh or built from selected messages of a side conversation.
- The teammate sees the exact prompt that will be delivered before sending.
- The request waits until the agent's current turn ends, then runs in its own fork of the agent's session (D-056).
- The requester sees its status: received, waiting for approval, queued, running, completed, denied, failed, canceled, or expired.
- Actions the work leads to follow FR-015.

**FR-015 Approve actions.** Actions from a teammate's request need the owner's approval, except low-risk actions that run without asking (D-028). *Proposed boundary:* reading files inside the agent's folder and its assigned repositories. Writing, deleting, running commands, and using the network always ask unless a saved rule covers them.
- The approval prompt shows the requesting person and agent, the purpose, the target agent, the exact command or file change, and the scope of the choice.
- Choices (D-003): **allow once**; **keep allowing similar commands** (a saved rule matching requester, agent, program, allowed arguments, folder, and expiry; never text similarity); **allow all** within a chosen requester and agent.
- A rule never permits command chaining or another folder that it does not name.
- Saved rules are listed, editable, and revocable, with optional time and use limits.
- *Proposed:* an unanswered approval waits and the request expires after 24 hours.

**FR-016 Owner offline.** Teammates see whether each member and agent is online. *Proposed:* a question or request sent while the owner's Mac is offline waits and is processed when it returns, after OpenGrokBot checks again that sharing and permissions still allow it; the asker is notified.

### Agent interfaces

**FR-017 CLI.** A teammate's agent can list members and their shared agents, ask a person or an agent, send a work request, get a request's status, and cancel it.
- Responses are structured JSON with stable fields and exit codes.
- A request waiting for approval returns its ID and status immediately; an optional wait has a timeout.
- Repeating a request with the same idempotency key returns the existing request instead of running it twice.

**FR-018 MCP server.** The same operations are available as MCP tools, under the same permissions and request IDs (D-022).

**FR-019 Agent-to-agent safeguards.** Every request records the human teammate, the agent acting for them, and the chain of requests. OpenGrokBot limits chain depth (3, assumed), request rate, and run time so agents cannot keep querying each other. A remote answer can never grant a permission or act as an instruction from the owner.

### Records and future channels

**FR-020 Activity record.** The owner's Mac keeps a local record of questions, work requests, approval decisions, and results, viewable in the app.

**FR-021 Channel-ready design.** The agent folder model leaves room for channels, so that a later version can connect an agent to Slack (D-014) or to customers, for example a customer support agent answering customers (D-021). No channel ships in the first version.

## Non-functional requirements

| ID | Requirement | Target or check |
| --- | --- | --- |
| NFR-001 | Answer time (D-018) | With the owner's Mac online, at least 9 of 10 pilot questions get an answer or the FR-010 choice prompt within 5 minutes. The asker sees that the question was received within 5 seconds. The 9-in-10 share is an assumption. |
| NFR-002 | No interruption (D-024) | Side conversations and routing add nothing to, reorder nothing in, and do not delay the main session. Checked by comparing the main session's conversation and queue before and after a question. |
| NFR-003 | Sensitive content (D-024) | Nothing from a private agent leaves the Mac. Excerpts and answers pass secret masking before leaving; masking supplements, and does not replace, the owner's sharing choices and preview. |
| NFR-004 | Execution safety (D-024) | No action from a teammate's request runs outside FR-015, including when the runtime has its own auto-approval settings. Approval decisions are made only on the owner's Mac. |
| NFR-005 | Freshness (D-024) | Every answer carries the time of its source content; stale or unsupported claims are labeled (FR-012). |
| NFR-006 | Connection security | Only team members connect; connections are authenticated and encrypted. |
| NFR-007 | Reliability | Questions, requests, and statuses survive app restarts and network drops; a retried request never runs twice. |
| NFR-008 | Resource use | OpenGrokBot's own idle use stays small; assumed target under 1% CPU and 200 MB memory, measured separately from the agents' runtimes. |
| NFR-009 | Accessibility | Standard macOS controls, VoiceOver labels on all actions, and keyboard operation for asking, approving, and starting or stopping agents. |

## Data and retention

| Data | Lives on | Kept |
| --- | --- | --- |
| Agent folders (instructions, skills, memory, schedules) | Owner's Mac | Until the owner deletes the agent. |
| Session conversations | Owner's Mac, in the runtime's own storage | As the runtime keeps them; OpenGrokBot does not copy them off the Mac except as permitted excerpts. |
| Questions, answers, excerpts | Owner's Mac and the asker's Mac | Until deleted (D-029, FR-013). |
| Work requests, approvals, rules, activity record | Owner's Mac; request status also on the requester's Mac | Until deleted. |
| Team membership and invitations | Each member's Mac; anything shared between Macs is decided in the tech spec | Invitations expire; membership until removed. |

## Exclusions

Not in the first version: a per-agent usage and cost view; Slack; any customer-facing channel; personal cloud agents such as Instinct and OpenAI Dots; a web UI; Windows and Linux; billing; administration for large teams.

## Dependencies

- Claude Code and Codex installed on each owner's Mac, with ways to read session conversations, start and stop runs, deliver prompts, steer a running turn, and intercept actions for approval. Each must be proven early (one-pager risk "Integration").
- Hermes and OpenClaw installed where they are to be detected.
- A way for the four Macs to reach each other (decided in the tech spec).

## Open decisions

- Which runtime is supported first, Claude Code or Codex (D-005).
- Build OpenGrokBot or extend an existing product such as Reason Network (D-006).
- How the Macs connect, and the remaining defaults (D-008).
- The minimum macOS version and the distribution method (tech spec).
- Request-fork file isolation and result delivery to the owner and main session: see the [tech spec's open decisions](4_techspec.md#open-decisions).

## Proposed defaults to confirm in this review

Items 1 to 10 were accepted with Rev 1. Item 11 is new in Rev 2 and is not yet the user's decision; confirming this revision accepts it.

1. A coding agent's folder lists the repositories it works in (FR-001).
2. A missed scheduled run runs once when the Mac wakes (FR-005).
3. Assumed thresholds: overview update within 5 seconds (FR-002), stalled after 10 minutes (FR-007), out of date after 1 hour (FR-012), chain depth 3 (FR-019), 9 of 10 answers within 5 minutes (NFR-001), resource targets (NFR-008).
4. The team creator removes members (FR-008).
5. Work requests are off for an agent until its owner turns them on (FR-009).
6. Askers delete their own history; owners delete any history about their agents (FR-013).
7. Low-risk actions are only file reads inside the agent's folder and assigned repositories (FR-015).
8. An unanswered approval expires after 24 hours (FR-015).
9. Questions and requests sent while the owner is offline wait and run when the owner returns (FR-016).
10. Customer-facing channels are designed for but not shipped in the first version (FR-021, D-021).
11. New in Rev 2: the runtime's permission requests for the owner's own work appear in the agent's conversation (FR-022).
