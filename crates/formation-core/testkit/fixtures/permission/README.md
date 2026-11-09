# Permission routing fixtures (TASK-005)

Logged runs of Claude Code 2.1.319 (`claude -p`, default model) with the permission spike attached as both the `--permission-prompt-tool` and a `PreToolUse` hook. Each folder is one run:

- `stream.jsonl`: the `--output-format stream-json --verbose` output.
- `spike.jsonl`: one line per call that reached the spike; `source` is `permission_tool` or `pre_tool_use_hook`.
- `side_effects.txt`: the mode, the decisions, and the files in the run folder after the run.

Home directory paths are replaced with `/Users/USER` and the macOS temporary folder hash with `XX/RANDOM`.

## Reading the results

- **P**: the call reached the permission tool (a `permission_tool` line with its `tool_use_id`).
- **H**: the call reached the `PreToolUse` hook (a `pre_tool_use_hook` line).
- **R**: the action ran, judged by its side effect: the file content in `side_effects.txt`, `curl.html` present, or a successful WebFetch `tool_result` in `stream.jsonl`.

| Folder | `--permission-mode` | `FORMATION_SPIKE_DECISION` | `FORMATION_SPIKE_HOOK_DECISION` |
| --- | --- | --- | --- |
| `A-manual` | `manual` | `deny` | `none` |
| `A-acceptEdits` | `acceptEdits` | `deny` | `none` |
| `A-auto` | `auto` | `deny` | `none` |
| `A-dontAsk` | `dontAsk` | `deny` | `none` |
| `A-bypassPermissions` | `bypassPermissions` | `deny` | `none` |
| `A-plan` | `plan` | `deny` | `none` |
| `B-bypassPermissions-hookdeny` | `bypassPermissions` | `deny` | `deny` |
| `B-auto-hookdeny` | `auto` | `deny` | `deny` |
| `C-manual-allow` | `manual` | `allow` | `none` |
| `D-manual-hookallow` | `manual` | `deny` | `allow` |

`FORMATION_SPIKE_DECISION` answers the permission tool (only `allow` allows). `FORMATION_SPIKE_HOOK_DECISION` answers the hook; `none` returns no decision, so Claude Code continues its normal permission flow. See `formation_core::harness::permission`.

## Reproducing a run

Build the spike, then run each case in a new empty folder outside the repository. Each run is a paid model call. The runs were recorded before the spike moved into this crate, with the same subcommands and environment variables.

```sh
cargo build -p formation-core --examples
BIN="$PWD/target/debug/examples/permission_spike"
MODE=manual DEC=deny HOOKDEC=none      # one row of the table above

DIR=$(mktemp -d) && cd "$DIR"
echo "original note" > note.txt
echo "color=blue" > edit.txt
LOG="$DIR/spike.jsonl"
MCP=$(printf '{"mcpServers":{"formation":{"type":"stdio","command":"%s","args":["mcp","--permission"],"env":{"FORMATION_SPIKE_LOG":"%s","FORMATION_SPIKE_DECISION":"%s"}}}}' "$BIN" "$LOG" "$DEC")
SETTINGS=$(printf '{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"%s hook pre-tool-use"}]}]}}' "$BIN")
PROMPT='This is a permission test. Perform these steps in order, one tool call at a time. If a step is denied or fails, do not retry it and do not try any alternative; move on to the next step.
1) Use the Read tool to read note.txt.
2) Use the Read tool to read edit.txt, then use the Edit tool to change "color=blue" to "color=red" in edit.txt.
3) Use the Write tool to create write.txt containing exactly: written
4) Use the Bash tool to run exactly: python3 -c "open('"'"'bash.txt'"'"','"'"'w'"'"').write('"'"'ran'"'"')"
5) Use the WebFetch tool to fetch https://example.com and tell me the page title.
6) Use the Bash tool to run exactly: curl -s -o curl.html https://example.com
Finally, list each step with succeeded or denied/failed.'

FORMATION_SPIKE_LOG="$LOG" FORMATION_SPIKE_DECISION="$DEC" FORMATION_SPIKE_HOOK_DECISION="$HOOKDEC" \
  "$HOME/.local/bin/claude" -p "$PROMPT" --output-format stream-json --verbose \
  --permission-mode "$MODE" \
  --mcp-config "$MCP" --strict-mcp-config \
  --settings "$SETTINGS" --setting-sources project \
  --permission-prompt-tool mcp__formation__permission \
  --no-session-persistence --max-budget-usd 2 \
  > stream.jsonl < /dev/null
```

`--setting-sources project` keeps the user's own hooks, plugins and allow rules out of the run; the empty folder has no project settings. In `plan` mode the agent may write a plan file under `~/.claude/plans/`.
