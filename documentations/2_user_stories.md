# OpenGrokBot — User stories

Rev 2 · 2026-11-09 · Based on the [product requirements](1_product_requirements_documentation.md) (Rev 2). Rev 2 adds US-024 for chatting with one's own agent. Roles and terms follow that document. All stories are **Must** for the first version (D-023).

Examples use Dohyeon as an owner and Minsu as a teammate. Every team member plays both roles: owner of their own agents, teammate to the others.

## Owner stories

### US-001 Check that my agents are running well
**FR:** FR-002, FR-005, FR-007
As an owner, I want to see at a glance whether my agents are running well when I open OpenGrokBot, so that I can fix problems before they hold anyone up.

- **Precondition:** OpenGrokBot is installed and has at least one agent.
- **Given** Dohyeon opens OpenGrokBot in the morning, **when** the overview loads, **then** every agent shows its state, last activity time, and pending teammate requests, and anything that needs Dohyeon (approval waiting, error, stalled, failed or missed scheduled run) is listed first.
- **Given** a scheduled run happened overnight, **when** Dohyeon opens that agent, **then** the run's time and result are shown.
- **Given** nothing needs attention, **then** the overview says so, with no alerts.
- **Failure:** if a detected agent's state cannot be read, it shows as unknown with the reason, not as working or idle.

### US-002 Create an agent for any role
**FR:** FR-001
As an owner, I want to create an agent for a role I define, such as marketing or customer support, so that each kind of work has its own agent.

- **Given** Dohyeon enters the name "Marketing", a role description, the runtime Codex, and starting instructions, **when** they create the agent, **then** a dedicated folder is created with those instructions, the agent appears as stopped and private, and the folder can be opened in Finder.
- **Given** the agent is a coding agent, **when** Dohyeon creates it, **then** they can list the repositories it works in.
- **Failure:** if the chosen runtime is not installed or not signed in, creation still saves the folder but the app says the agent cannot start until the runtime is ready.
- **Cancel:** closing the form before creating leaves no folder behind.

### US-003 Start, stop, and restart an agent
**FR:** FR-003
As an owner, I want to start, stop, and restart my agents from one place, so that I don't juggle terminal windows.

- **Given** an agent is stopped, **when** Dohyeon starts it, **then** its runtime runs in the agent's folder and its state becomes working or idle.
- **Given** an agent is in the middle of a task, **when** Dohyeon stops it, **then** OpenGrokBot asks whether to stop now or after the current turn, and does what was chosen.
- **Given** Dohyeon restarts an agent, **then** it continues the latest session where the runtime supports it, or the app says a new session started.
- **Failure:** if the runtime exits with an error, the agent shows the error state and Dohyeon is alerted (US-007).
- **Detected agents:** controls a harness does not support are hidden, with an explanation.

### US-024 Give my agent work by chatting
**FR:** FR-022
As an owner, I want to chat with my agent to give it work and follow what it does in the same conversation, so that I can run all my agents from one place instead of separate terminals.

- **Given** the Backend agent is idle, **when** Dohyeon sends "로그인 에러 로그 정리해줘" ("Clean up the login error logs"), **then** the message starts a turn in Backend's main session, and the reply and its steps (files changed, commands run) stream into the conversation.
- **Given** Backend is working, **when** Dohyeon sends another message, **then** it shows as queued and is delivered when the current turn ends; until then Dohyeon can edit or withdraw it.
- **When** Dohyeon chooses "Steer now" on the queued message, **then** it is delivered into the running turn at the next point the runtime allows; **when** Dohyeon chooses "Stop," the turn ends.
- **Given** the runtime asks permission for an action in Dohyeon's own work, **then** the request appears in the conversation for Dohyeon to answer.
- **Given** a detected agent that does not accept input from OpenGrokBot, **then** its activity is visible but the message box is disabled with the reason.
- **Then** teammates never see this conversation.

### US-004 Edit an agent's instructions and skills
**FR:** FR-004
As an owner, I want to edit an agent's instructions and skills in the app, so that I can shape how it works without hunting for files.

- **Given** Dohyeon edits the instructions and saves, **then** the change is written to the agent's folder and the app states when it takes effect.
- **Given** Dohyeon adds, edits, or removes a skill, **then** the folder reflects it.
- **Given** a file in the agent's folder was changed outside the app, **when** Dohyeon opens the agent, **then** the app shows the current content.

### US-005 Schedule recurring work
**FR:** FR-005
As an owner, I want an agent to do a task on a schedule, so that routine work happens without me.

- **Given** Dohyeon adds "summarize yesterday's support tickets" every weekday at 09:00, **when** the time comes, **then** the agent runs it in a new session, and the run is recorded with its result.
- **Given** a scheduled run needs an action outside the approval rules, **then** it waits for approval like any other work.
- **Given** the Mac was asleep at 09:00, **when** it wakes, **then** the run is marked missed and runs once.
- **Failure:** a failed run is recorded with its error and alerts Dohyeon.

### US-006 Connect agents I already run elsewhere
**FR:** FR-006
As an owner, I want OpenGrokBot to find the Claude Code and Codex sessions I started elsewhere and my Hermes or OpenClaw instances, so that all my agents are in one place.

- **Given** a Claude Code session is running in Terminal, **when** OpenGrokBot detects it, **then** it appears in the overview labeled as detected.
- **When** Dohyeon connects a detected agent, **then** it can be shared like other agents, and the app lists which operations work for it and which do not, with reasons.
- OpenGrokBot reads detected sessions and works only in forks, never resuming or writing to the original (FR-006).
- OpenGrokBot does not change a detected agent's own configuration without Dohyeon's consent, and does not create new agents on Hermes or OpenClaw.

### US-007 Be alerted only when I'm needed
**FR:** FR-007
As an owner, I want alerts only when something needs me, so that monitoring does not become another interruption.

- **Given** an agent waits for approval, errors, exits unexpectedly, has worked with no activity for 10 minutes, receives a work request, or has a failed or missed scheduled run, **then** Dohyeon gets a macOS notification and the app badge updates.
- **Given** anything else happens, **then** it is visible in the overview with no notification.
- **Given** Dohyeon mutes an agent, **then** it raises no notifications until unmuted.

### US-008 Set up my team
**FR:** FR-008
As the member who creates the team, I want to invite my three teammates, so that we can ask each other's agents.

- **Given** Dohyeon creates a team and an invitation link, **when** a teammate accepts it before it expires, **then** they appear in the member list with an online indicator.
- **Failure:** an expired link is refused with a message explaining how to get a new one.
- **Given** Dohyeon removes a member, **then** that member loses access at once.

### US-009 Share an agent safely
**FR:** FR-009
As an owner, I want to choose which agents my teammates can ask and what they see, so that nothing private leaks.

- **Given** a new agent, **then** it is private and invisible to teammates.
- **When** Dohyeon shares it, **then** they first see a preview of what a teammate would receive, then choose visibility (answer only, or answer plus excerpts, default the latter) and whether work requests are accepted (off by default).
- **When** Dohyeon unshares it, **then** new questions and requests are refused at once, pending ones are canceled, and a running task continues unless Dohyeon stops it.

### US-010 Answer a question sent to me
**FR:** FR-010
As an owner, I want questions that OpenGrokBot could not route to reach me, so that I can answer or hand them to the right agent.

- **Given** Minsu chose "ask the owner," **then** Dohyeon receives the question with Minsu's name and context.
- **When** Dohyeon answers, **then** Minsu receives the answer in the same conversation.
- **When** Dohyeon hands it to an agent, **then** that agent answers as in US-013.

### US-011 Approve what my teammates' requests do
**FR:** FR-015
As an owner, I want to approve each action a teammate's request leads to, unless I have allowed it already, so that nothing runs on my Mac without my consent.

- **Given** an action is a file read inside the agent's folder or assigned repositories, **then** it runs without asking.
- **Given** any other action, such as a write, command, or network use, and no saved rule covers it, **then** Dohyeon sees who asked, through which agent, the purpose, the target agent, the exact command or file change, and the scope, and chooses allow once, keep allowing similar commands, allow all for this requester and agent, or deny.
- **Given** Dohyeon does not respond within 24 hours, **then** the request expires and the requester is told.
- **Denied:** the requester sees that the action was denied; the agent does not perform it.

### US-012 Review my rules and history
**FR:** FR-013, FR-015, FR-020
As an owner, I want to see and change the rules I saved and the questions and requests about my agents, so that I stay in control over time.

- **When** Dohyeon opens rules, **then** each rule shows who and what it covers and its limits, and can be edited or revoked; a revoked rule stops covering new actions at once.
- **When** Dohyeon opens history, **then** every question, request, approval decision, and result about their agents is listed, and Dohyeon can delete any question and answer.

## Teammate stories

### US-013 Ask a person about progress
**FR:** FR-010, FR-012
As a teammate, I want to ask a colleague's agents about their work in plain words, so that I get the answer without waiting for the colleague.

- **Precondition:** Minsu and Dohyeon are in the same team, and Dohyeon has at least one shared agent.
- **Given** Minsu asks Dohyeon "how far is the login work?", **when** OpenGrokBot is confident which agent can answer, **then** within 5 seconds Minsu sees that the question was received, and the answer names the agent, states the time of the latest conversation content it used, includes excerpts if the agent's visibility allows, and says plainly what the conversation does not establish.
- **Given** the agent's latest activity is older than 1 hour, **then** the answer is marked as possibly out of date.
- **Then** Dohyeon's agent keeps working: nothing is added to its main session or queue.

### US-014 Choose when OpenGrokBot is unsure
**FR:** FR-010
As a teammate, I want to decide what happens when OpenGrokBot is not sure which agent should answer, so that I don't get a confident answer from the wrong agent.

- **Given** OpenGrokBot is not confident, **then** Minsu sees one prompt with the best-guess agent first, the other candidate agents, and "ask Dohyeon."
- **When** Minsu picks an agent, **then** that agent answers as in US-013.
- **When** Minsu picks "ask Dohyeon," **then** the question goes to Dohyeon (US-010).

### US-015 Ask a specific agent
**FR:** FR-011, FR-012
As a teammate, I want to pick one of a colleague's agents and ask it directly, so that I can go straight to the right one.

- **When** Minsu opens Dohyeon's profile, **then** Minsu sees only shared agents, each with name, role, state, and last activity.
- **When** Minsu asks the "Backend" agent, **then** it answers as in US-013.

### US-016 Keep asking follow-up questions
**FR:** FR-013
As a teammate, I want to continue with more questions in the same conversation, so that I don't repeat context.

- **Given** Minsu received an answer, **when** Minsu asks a follow-up, **then** it is answered in the same side conversation with the earlier questions as context.
- **When** Minsu refreshes, **then** the next answer uses the agent's latest content and states its new time.

### US-017 Ask again when an answer is wrong, unclear, or missing
**FR:** FR-010, FR-012, FR-013
As a teammate, I want to ask again easily when an answer doesn't help, so that I still get what I need.

- **Given** an answer is unclear or wrong, **when** Minsu asks again, **then** Minsu can rephrase in the same conversation, refresh to the latest content, choose a different agent, or ask Dohyeon.
- **Given** no answer has arrived, **then** Minsu sees why (for example, Dohyeon is offline, or the question is waiting) and can cancel and ask again.

### US-018 Ask while the owner is offline
**FR:** FR-016
As a teammate, I want to leave a question when a colleague's Mac is offline, so that I don't have to remember to come back.

- **Given** Dohyeon's Mac is offline, **then** Minsu sees that before sending.
- **When** Minsu sends anyway, **then** the question waits; when Dohyeon's Mac returns, OpenGrokBot checks again that sharing and permissions still allow it, answers, and notifies Minsu.
- **Given** the agent was unshared in the meantime, **then** Minsu is told the question can no longer be answered.

### US-019 Request work and follow it
**FR:** FR-014, FR-015
As a teammate, I want to ask a colleague's agent to do something and see what happens to the request, so that I can plan around it.

- **Given** an agent accepts work requests, **when** Minsu writes a request or builds one from selected messages of a side conversation, **then** Minsu sees the exact prompt that will be delivered before sending.
- **Then** the request waits for the agent's current turn to end, runs in its own fork (FR-014), and Minsu sees its status move through received, waiting for approval, queued, running, and completed, denied, failed, canceled, or expired.
- **Given** an agent does not accept work requests, **then** the option is unavailable and says so.
- **Cancel:** Minsu can cancel a request that has not started.

### US-020 Join a team
**FR:** FR-008
As a teammate, I want to join my team from an invitation, so that I can start asking.

- **Given** Minsu has a valid invitation link, **when** Minsu installs OpenGrokBot and accepts, **then** Minsu sees the members and their shared agents.

### US-021 Delete my own questions
**FR:** FR-013
As a teammate, I want to delete questions I asked, so that I control my own history.

- **When** Minsu deletes a question from their history, **then** it no longer appears in Minsu's history.

## Teammate's agent stories

### US-022 Ask and request through the CLI
**FR:** FR-017, FR-019
As a teammate's agent, I want to query a colleague's agents and send work requests from the command line with structured results, so that I can use them inside my own work.

- **When** Minsu's agent lists members and shared agents, asks, requests, checks status, or cancels, **then** it gets JSON with stable fields and exit codes.
- **Given** a request waits for approval, **then** the call returns its ID and status immediately instead of blocking; an optional wait ends at its timeout.
- **Given** the same idempotency key is sent twice, **then** the second call returns the existing request and nothing runs twice.
- **Given** a chain of agent requests reaches depth 3 or the rate limit, **then** further requests are refused with a clear error.
- **Then** every request records Minsu as the human behind it and the agent that made it.

### US-023 Ask and request through MCP
**FR:** FR-018, FR-019
As a teammate's agent, I want the same operations as MCP tools, so that MCP clients can use OpenGrokBot without the CLI.

- **Then** the MCP tools behave exactly like US-022, with the same permissions and request IDs.

## Requirement coverage

| Requirement | Stories |
| --- | --- |
| FR-001 | US-002 |
| FR-002 | US-001 |
| FR-003 | US-003 |
| FR-004 | US-004 |
| FR-005 | US-001, US-005 |
| FR-006 | US-006 |
| FR-007 | US-001, US-007 |
| FR-008 | US-008, US-020 |
| FR-009 | US-009 |
| FR-010 | US-010, US-013, US-014, US-017 |
| FR-011 | US-015 |
| FR-012 | US-013, US-015, US-017 |
| FR-013 | US-012, US-016, US-017, US-021 |
| FR-014 | US-019 |
| FR-015 | US-011, US-012, US-019 |
| FR-016 | US-018 |
| FR-017 | US-022 |
| FR-018 | US-023 |
| FR-019 | US-022, US-023 |
| FR-020 | US-012 |
| FR-021 | No user story: a design requirement on the agent folder model, verified in the tech spec. |
| FR-022 | US-024 |

Non-functional requirements NFR-001 to NFR-009 are verified through technical checks in the implementation plan rather than separate stories; NFR-001, NFR-002, and NFR-005 also appear in the acceptance criteria of US-013.
