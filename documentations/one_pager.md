# Formation

**Don’t ask your teammate. Ask their agent.**

An open-source macOS app and CLI for collaborating through local Claude Code and Codex sessions. **Status: planning stage; no app or CLI has been implemented.**

## Problem

Coding agents accumulate useful context: implementation decisions, failed approaches, investigation results, and unfinished work. That context stays inside individual sessions. When a teammate needs it, the session owner must explain, copy, or relay messages between agents—interrupting their work and becoming the coordination bottleneck.

## Product

Formation lets an owner share selected session context with chosen collaborators and their agents. A collaborator can ask a question in a separate conversation or submit a work request to the main session. The agent runs on the owner’s machine, and provider credentials stay there.

The initial audience is small development teams already using local coding agents. Owners control access and execution; teammates retrieve context directly; their agents use a stable CLI with structured responses.

## Core experience

1. **Share:** The owner selects a session, previews the exposed context, and grants access through an expiring invitation.
2. **Ask:** A teammate or their agent asks, “Why did you choose JWT?” A separate conversation answers from approved context without changing the main session’s history or queue. Answers identify the source session and context revision.
3. **Request work:** The teammate asks, “Add token expiration tests.” Formation previews the prompt and queues it for the main session under the owner’s policy. Accepting a prompt does not approve every resulting tool action.
4. **Control:** The owner can allow once, save a deterministic rule for similar commands, or allow supported actions within an explicit scope. Grants can be inspected and revoked.

Side questions initially use shared history without tools and cannot write to the source workspace. Main-session requests normally wait until the current turn finishes. Revocation blocks new access and pending work; interrupting an already-running task is a separate choice.

## Differentiation to validate

Formation’s proposed advantage is the complete workflow: **ask a teammate’s agent without disrupting their main task, then turn an answer into authorized work.** Restricted side conversations, visible context revisions, collaborator-scoped execution rules, and consistent permissions across the app and CLI support that experience.

Sharing, approvals, and a CLI alone are insufficient differentiation. The existing research identifies Happier as the closest broad comparison, with Flowpad and Agent Relay also overlapping. Whether their side-question workflows already meet this need requires hands-on testing.

## MVP and approach

Start with one provider, two Macs, and one collaborator. Deliver session-sharing previews, expiring invitations, isolated side questions, CLI JSON responses, approvals, revocation, and local audit records. Extend this with ordered main-session requests, structural permission rules, idempotency, cancellation, and the second provider.

A SwiftUI app sits above a local service that owns policy, queues, and provider adapters. Begin with executions Formation manages. Reading or resuming stored history does not establish that an arbitrary running Claude Code or Codex session can accept external prompts; compatibility and approval enforcement need integration proofs.

## Validation and next decision

Run the same two-person workflow in Happier, Flowpad, and Agent Relay before committing to a full app. Compare setup friction, main-task disruption, permission granularity, existing-session compatibility, and host resource use. In a small-team pilot, measure fewer repeated explanations, faster question resolution, approval burden, and repeat usage against a baseline.

Choose the first provider, default sharing scope, execution defaults, and transport after these tests. If an existing product sufficiently supports the workflow, consider an extension. Web UI, MCP, billing, and large-team administration are outside the initial MVP.

Further detail: [product plan](product-plan.md) · [competitive landscape](competitive-landscape.md).
