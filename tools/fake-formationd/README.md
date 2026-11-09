# fake-formationd

`fake-formationd` serves the approved M3 mockup's world over formationd's local socket protocol, so the app can be built and reviewed before the real formationd exists ([TASK-043](../../documentations/implementation_plan.md#task-043-cs-fake-formationd-serves-the-mockups-world)). It speaks the same newline-delimited JSON-RPC 2.0 as the real service ([UI ↔ formationd](../../documentations/4_techspec.md#ui--formationd)):

1. The client calls `snapshot`.
2. The reply is a `Snapshot`, followed by `event` notifications with increasing `seq`.

Every value is a `formation-proto` type.

## Run it

```sh
cargo run -p fake-formationd                              # serve SC-005/working
cargo run -p fake-formationd -- serve --state SC-011/expiring
cargo run -p fake-formationd -- list                      # states and scripts, from the running fake
cargo run -p fake-formationd -- state SC-015/unshared     # switch the running fake; clients reconnect
cargo run -p fake-formationd -- play approval-raised      # play a script on the running fake
cargo run -p fake-formationd -- check                     # check the fixtures against the contract
```

- **Socket:** `~/Library/Application Support/Formation/fake-formationd.sock` by default (owner-only), or `--socket PATH`. It is not the real formationd's path, so both can run at once. The app connects here when started with `FORMATION_FAKE=daemon` (TASK-008).
- **Time:** fixture times are written with M3's "now" at 10:53 on 2026-11-09 in Seoul (`2026-11-09T01:53:00Z`). The fake moves every timestamp so that this instant becomes the moment it starts. "3 min ago" stays 3 minutes ago, and an approval with 23 h 57 min left still has them.

## How it works

- **No reducer.** A snapshot is the loaded world plus a replay of every event since it was loaded. A client that reconnects arrives at the same place.
- **Switching state** (`state`, or the `fake_state` method) loads a new world and disconnects every client. The app then reconnects and takes the new snapshot, which also exercises the reconnect path.
- **Sequence numbers.** `seq` increases by one per event across the fake's lifetime. `stream_seq` continues per agent from the conversation in the snapshot.
- **Fake-only methods.** The app never calls these: `fake_state { state }`, `fake_play { script }` and `fake_list`.

## Fixtures

All fixtures live in [`fixtures/`](fixtures/). `check` and `cargo test -p fake-formationd` decode each one into its contract type and encode it again. A missing, misspelled or extra field fails, so a contract change that breaks a fixture fails CI.

| File | What it holds |
| --- | --- |
| `worlds/m3.json` | The M3 world at SC-005 "working": Dohyeon's six agents, Minsu's and Jiwoo's shared agents, the conversations, questions, Minsu's queued request, the Inbox, rules, schedules, and harnesses. |
| `worlds/empty.json` | First run: a team of one and no agents (SC-003 "empty"). |
| `scripts/*.json` | Timed event sequences: `{ "description", "steps": [{ "after_ms", "event" }] }`. |
| `states.json` | Each M3 state: a world plus scripts applied at once. A `ui` note says what to do in the app for states the app shows by itself, such as dialogs, sheets and drafts. |
| `reactions.json` | What each command returns and which script it plays, with an `example` used for checking. The first reaction whose `when` fields match the params is used, for example `{ "choice": "deny" }` or `{ "target/kind": "person" }`. |

### Placeholders

Placeholders work in scripts and reactions:

| Placeholder | Replaced with |
| --- | --- |
| `{{params.field}}` | The command's params; nested fields use dots, as in `params.target.agent_id`. |
| `{{id:name}}` | A fresh UUIDv7, shared by a reaction's result and the script it plays. |
| `{{now}}` | The current time. |
| `{{result}}`, `{{result.field}}` | In a script a reaction plays: that reaction's result, for example to send a changed rule as `rule.update`. |

A string that is exactly one placeholder becomes that value with its JSON type.

### Commands the fake simulates

| Command | Result and script |
| --- | --- |
| `agent_message_send` | Queued, then a turn streams a reply (`send-reply`). |
| `agent_message_steer`, `agent_message_withdraw`, `agent_message_edit` | Changes the queue. |
| `ask` | To an agent: received, then answering, then answered (`ask-answer`). To a person: unsure, with candidates (`ask-unsure`). |
| `question_route_choose` | Answered by the chosen agent, or sent to the owner. |
| `question_cancel` | The question is canceled. |
| `request_create` | Received, then awaiting approval, queued, running and completed (`request-flow`). |
| `request_cancel` | The request is canceled. |
| `approval_decide` | Allow once or allow all, keep allowing similar (saves a rule), or deny. The owner's own permission prompt is handled separately. |
| `agents_detect` | Answers after 2.5 s, so the detecting state shows. |
| `settings_update` | Sends `settings.update`. |
| `rules_update`, `rules_revoke` | For the world's two rules: the changed rule, also sent as `rule.update` (`rule-changed`). A rule saved during the session is not simulated. |
| `team_leave` | Answers `null`. No event clears the team; the contract has none yet. |
| `agent_start`, `agent_stop`, `agent_turn_interrupt` | Built in: the named agent from the world with its new state. |
| `agents_list`, `teammate_agents`, `rules_list`, `harness_check`, `schedule_list` | Read from the world. |

Any other contract method answers with an `internal` error saying the fake does not simulate it.

## M3 states covered

Every M3 state of SC-003, SC-005, SC-010, SC-011, SC-012, SC-015 and SC-016 has an entry in `states.json`. A test fails if one goes missing. Run `list` for the full set with the app steps.

## Limits

These are contract gaps found while writing the fixtures:

- **No `AttentionKind` for "scheduled run missed → ran on wake".** M3's fourth Inbox row is therefore left out. The schedule still shows `last_run.result: "missed_ran"`.
- **`WorkRequest` has no per-step times.** SC-016's status timeline can show which steps are done, but not when each happened.
- **`RuleScope` only has commands.** M3's file-write rule for Jiwoo → Marketing (`write drafts/*`) is written as the program `write`.
