# Harness v4 acceptance — 2026-10-26

What this file is: the mechanical record of the `feature/harness-v4`
acceptance run. Every number and every quoted line was produced by a
command on this branch in this worktree; nothing here is copied from
the design or the plan.

* Worktree: `/home/crochee/workspace/synthia/.worktrees/harness-v4`
* Branch: `feature/harness-v4`, HEAD `e0892c50` at the start of the run
* Server binary: `target/debug/synthia-server` (`cargo build -p
  synthia-server`)
* Acceptance workspace: `/tmp/synthia-accept-t18` (server configs, the
  Python plugin, and the session logs live there and are **not**
  committed)
* Model provider: a real OpenAI-compatible endpoint
  (`https://api.deepseek.com/v1`, model `deepseek-flash`), key read
  from `DEEPSEEK_API_KEY`. The harness, the server, the tools, the log
  and the HTTP surface under test are this repo's; only the model is
  external.
* Web: assets built with `npm run build` in `synthia-web/`.

`$DIR` below means `/tmp/synthia-accept-t18`; `$API` means
`http://127.0.0.1:8099/api/v1`; a session log is
`$DIR/ws/sessions/dev/<session-id>/events.jsonl`.

**Status at a glance.** The three dependency counts are `0`. `make ci`,
`make check-web`, `make examples` and `make bench-check` each exit `0`.
`make test-crates` is red in four crates this branch does not touch, all
four red on `master` too (§4). Scenarios B–I pass end to end; scenario A
failed at the time of the run and **passes after the fix recorded in §6**
— the run that produced this file is not retroactively edited. The
remaining open items, with their mechanisms, are in §5.

## 1. The three dependency counts

```
$ cargo tree -p synthia-harness -e normal | grep -c tokio
0
$ cargo tree -p synthia-harness -e normal | grep -cE "reqwest|hyper|rustls|h2|tower"
0
$ cargo tree -p synthia-plugin-protocol -e normal | grep -c tokio
0
```

All three are `0`. (`grep -c` prints `0` and exits non-zero when it
matches nothing; the count is the claim, and no count was taken by
editing the tree.)

`make examples` adds a second, independent form of the same claim from
the consumer side: `docs/examples/harness-only-consumer` prints
`tokio in tree: 0` and then `HARNESS-ONLY: OK`.

## 2. Gates

Run one at a time, in this worktree. Exit status is the `make` exit
status.

| target | exit | what it said |
|---|---|---|
| `make ci` (first run) | 2 | Stopped in `doc-check`: `error: unresolved link to \`GATE\`` at `crates/synthia-plugin-protocol/src/wire.rs:34` |
| `make ci` (second run) | 2 | `doc-check` then reported two further branch-added links: `Tool::risk` at `crates/synthia-server/src/composition.rs:194` and `ControllerInner::awaiting_approval` at `crates/synthia-server/src/session/controller/mod.rs:441` |
| `make ci` (final run) | 0 | `fmt-check`, `lint-rust`, `doc-check`, `test-guides` (default and the seven-feature subset), all 15 check targets, and `cargo deny` (`bans ok, licenses ok, sources ok`) |
| `make check-web` | 0 | `tsc --noEmit`, `eslint`, `prettier --check` all clean (`All matched files use Prettier code style!`) |
| `make test-crates` | 2 | Stopped at `synthia-budget`: its crate-level doctest does not compile (see §4) |
| `make examples` | 0 | 55 example/consumer targets; the harness-only consumer printed `tokio in tree: 0` and `HARNESS-ONLY: OK` |
| `make bench-check` | 0 | `all performance invariants hold` (7 invariants OK; e.g. `descriptors_cached vs descriptors 1713.9x (min 100.0x)`) |

`make ci` runs `fmt-check lint-rust doc-check test-guides
check-mvp-deps check-no-runtime check-harness-features
check-public-api-runtime check-pub-surface check-pub-surface-strict
check-claim-language check-clock check-harness-shape
check-harness-docs check-single-writer check-plugin-shape
check-test-layout check-deny`. It does **not** run `make test-crates`,
which is why the doctest failures below are invisible to it.

Because the first `make ci` stopped at `doc-check`, the stages after it
were also run one at a time so each has its own status:

```
test-guides                  EXIT=0
check-mvp-deps               EXIT=0
check-no-runtime             EXIT=0
check-harness-features       EXIT=0
check-public-api-runtime     EXIT=0
check-pub-surface            EXIT=0
check-pub-surface-strict     EXIT=0
check-claim-language         EXIT=0
check-clock                  EXIT=0
check-harness-shape          EXIT=0
check-harness-docs           EXIT=0
check-single-writer          EXIT=0
check-plugin-shape           EXIT=0
check-test-layout            EXIT=0
check-deny                   EXIT=0
```

### `make test-crates` per crate

`make test-crates` is `set -e` over `cargo test -p <crate>`, so it
stopped at the third crate. The remaining crates were run one at a
time with the same command:

```
synthia                          EXIT=0   (from make test-crates)
synthia-attachment               EXIT=0   (from make test-crates)
synthia-budget                   EXIT=101 (from make test-crates; doctest)
synthia-context                  EXIT=0
synthia-core                     EXIT=0
synthia-eval                     EXIT=0
synthia-harness                  EXIT=0
synthia-harness-tokio            EXIT=0
synthia-hpc-batch                EXIT=0
synthia-macros                   EXIT=0
synthia-mcp                      EXIT=0
synthia-mcp-server               EXIT=0
synthia-plugin-host              EXIT=0
synthia-plugin-protocol          EXIT=0
synthia-provider                 EXIT=0
synthia-reflection               EXIT=0
synthia-resilience               EXIT=101 (compile error in the lib test)
synthia-safety                   EXIT=0
synthia-scheduler                EXIT=0
synthia-search                   EXIT=0
synthia-semantic-cache           EXIT=101 (crate-level doctest)
synthia-session                  EXIT=0
synthia-skill                    EXIT=0
synthia-steering                 EXIT=0
synthia-strategy-best-of-n       EXIT=0
synthia-strategy-cot             EXIT=0
synthia-strategy-plan-execute    EXIT=0
synthia-telemetry                EXIT=0
synthia-test-support             EXIT=0
synthia-tool                     EXIT=0
synthia-tool-memo                EXIT=101 (compile error in the lib test)
synthia-tool-read                EXIT=0
synthia-tool-scheduler           EXIT=0
synthia-tool-search              EXIT=0
synthia-tool-shell               EXIT=0
synthia-tool-task                EXIT=0
synthia-tool-todo                EXIT=0
synthia-tool-web                 EXIT=0
synthia-tool-write               EXIT=0
synthia-workflow                 EXIT=0
synthia-server                   EXIT=0
```

The three failures are reproduced and explained in §4. None of the
failing crates is touched by this branch.

## 3. Scenarios

Each scenario names the command that drives it and quotes the line that
proves it. The UI half of each scenario is discussed separately in
§3.10.

### A. A basic turn that uses two tools

Command (tutorial config, `context_window: 3000`):

```bash
SID=$(curl -s -X POST $API/chat/sessions -H 'content-type: application/json' -d '{}' | jq -r .session_id)
curl -s -X POST $API/chat/sessions/$SID/messages -H 'content-type: application/json' \
  -d '{"text":"Use the read tool to read notes.txt, then use the shell tool to run exactly: ls -la . Then summarise both results in two sentences."}'
```

**Result: FAILED when this run was taken — fixed afterwards; the same
run now completes (§6).** The original failure is kept below because it
is the evidence for the fix. The turn ended as an error:

```
{"type":"turn","seq":18,"ts":"2026-10-26T00:36:15.952014219+00:00","data":{"kind":"end","turn":0,"reason":"error"}}
```

with the provider call rejected:

```
sample_once returned Err; ending session iteration=2 reason=Error("LLM stream error: retry exhausted after 1 attempts: request failed with status 400: {\"error\":{\"message\":\"An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id'. (insufficient tool messages following tool_calls message) …")
```

The cause is in the log, not in the model: the automatic compaction
pass shadowed both tool results while leaving the assistant turn that
carried the `tool_use` blocks in place:

```
{"seq":15,"surface_op":{"start":2,"end":4,"source_event_seqs":[9,10]},"source_indices":[3,4]}   # compaction
{"seq":6,"parts":{"text":12,"tool_use":2}}                                                     # the assistant message it left behind
```

That is the same shape the tutorial's own `synthia-session`
`tool_pairing` module exists to prevent: a replace that straddles an
`assistant/tool_use` ↔ `tool/result` boundary produces an orphan call.
The automatic path lives in `synthia-context`'s summariser, which
splices only the tool-result run ("The assistant turn that contained
the original `tool_use` blocks stays untouched", `manager.rs:346`);
the manual/harness path in `synthia-session` applies
`balanced_cut_at_or_before` instead. The two rules disagree.

This is **not** caused by this branch: `crates/synthia-context` is
unchanged except for the removal of one unused constructor, and the
splice comment is present on `master`.

Re-run with the same commands and only `context_window: 128000` (every
other setting identical), the scenario passes:

```
{"type":"turn","data":{"kind":"end","turn":0,"reason":"completed"}}
tool_result #1: "   1 synthia tutorial\n"
tool_result #2: "stdout:\ntotal 4\ndrwxr-xr-x 5 … notes.txt …\n\n[exit code: 0]"
baseline: {"rows":16,"cursor":16,"live":"object","context":{"used":9801,"window":128000},"pending":0,"rsh":71,"header_state":"idle"}
```

The `follow` baseline carries 16 rows — one `UserInput`, one
`request_header`, one `request_snapshot`, four `step`, one
`tool_dispatch`, two `tool_result`, two `assistant_message`, two
`turn`, two `usage` — exactly the 16 rows the log holds, so a refresh
rebuilds the window without duplicating or dropping anything. The
assistant messages carry `reasoning` parts next to their `text` parts,
which is the data behind the collapsible thinking section.

### B. Queue and steer

**Queue.** Two commands, the second issued while the first turn's shell
call is in flight:

```bash
curl -s -X POST $API/chat/sessions/$SID/messages -d '{"text":"Use the shell tool to run exactly: sleep 15; echo done. Then report the output."}'
# after tool_dispatch appears:
curl -s -X POST $API/chat/sessions/$SID/messages -d '{"text":"Mention the token queue-marker in your final answer.","delivery":"queue"}'
```

Observed:

```
{"message_id":"e7ef88e3-…","queued":true,"delivery":"queue"}
turn end #1: {"type":"turn","seq":15,…,"data":{"kind":"end","turn":0,"reason":"completed"}}
user row:    {"type":"UserInput","seq":16,…,"data":{"text":"Mention the token queue-marker in your final answer."}}
sleep result:{"type":"tool_result","seq":9,…,"data":{"content":{"Single":{"type":"text","text":"stdout:\ndone\n\n\n[exit code: 0]"}}}}
answer:      "The token you asked me to include is `queue-marker`."
```

The queued message is an ordinary `UserInput` row at `seq` 16, after
the first turn's `end` at 15, and the `sleep` it did not interrupt
returned normally. **PASS.**

**Steer.**

```bash
curl -s -X POST $API/chat/sessions/$SID/messages -d '{"text":"Mention the token steer-marker in your final answer.","delivery":"steer"}'
```

```
{"message_id":"995617e1-…","queued":false,"delivery":"steer"}
{"type":"user_message","seq":9,…,"data":{"role":"user","content":{"Single":{"type":"text","text":"Mention the token steer-marker in your final answer."}}},"source":"steer"}
seq 10 tool_result   seq 11 step end   seq 12 step start
answer: "The process slept for 12 seconds as requested, then printed `done` with no errors.\n\nsteer-marker"
```

The steer row (`source: "steer"`) lands at 9, after the tool batch and
before the next `step` start at 12; the model answered with the token.
**PASS.**

**Stop.** `POST $API/chat/sessions/{id}/cancel` while a `sleep 30` tool
call is in flight, after a steer:

```
cancel HTTP 204
{"seq":9,"type":"user_message","source":"steer",…}   # still durable
{"seq":14,"type":"turn","data":{"kind":"end","reason":"cancelled"}}
```

The cancel is answered `204` and the turn closes as `cancelled`; the
steer row is still in the log, so it was not silently dropped. In both
stop runs a `step start` followed the steer before the cancel was
observed, so the chip would have advanced to `已注入`/`已消费` rather
than returning the text to the editor. Whether the text returns to the
editor in the state the tutorial describes is a browser check (§3.10);
this run only proves the API side.

### C. Approval

**High-risk call suspends.** Command:

```bash
curl -s -X POST $API/chat/sessions/$SID/messages -H 'content-type: application/json' \
  -d '{"text":"Use the shell tool to run exactly: echo synthia-approval-probe > /tmp/synthia-approval.txt ; then report the command'\''s output and exit status."}'
```

Rows, in order:

```
{"type":"approval_asked","seq":8,"id":"approval-call_00_OI2ihbBxDnN6nes5ReIV4040","call_id":"call_00_OI2ihbBxDnN6nes5ReIV4040","tool":"shell","arguments_hash":"sha256:acc956…","risk":"high","agent":"agent"}
{"type":"turn","seq":9,"data":{"phase":"suspended","status":"suspended","pending":["call_00_OI2ihbBxDnN6nes5ReIV4040"]}}
{"type":"step","seq":10,"data":{"phase":"end","status":"suspended"}}
{"type":"turn","seq":11,"data":{"kind":"end","reason":"suspended"}}
```

The `follow` baseline reports the same thing from the durable log:

```
{"state":"awaiting_approval","pending":[{"id":"approval-call_00_OI2…","call_id":"call_00_OI2…","tool":"shell","arguments_hash":"sha256:acc956…","risk":"high","agent":"agent"}],"rsh":"sha256:03f7a76e…"}
```

An ordinary `ls -la` call in the same deployment does not suspend (it
ran in scenario A without an `approval_asked` row, because `shell`
classifies that argv as low risk), so the gate is per call, not per
tool.

**Approve.** `POST $API/sessions/$SID/resume` with the token built from
the log tail and the baseline's fingerprint:

```
resume HTTP 202
{"type":"approval_decided","seq":13,"id":"approval-call_00_OI2…","call_id":"call_00_OI2…","status":"approved","by":"resume"}
{"type":"tool_dispatch","seq":14,"call_id":"call_00_OI2…","tool":"shell","arguments_hash":"sha256:acc956…"}
dispatches for that call: 1
$ cat /tmp/synthia-approval.txt
synthia-approval-probe
```

The turn then completes with an answer that correctly explains the
redirection. **PASS.**

**Deny.** Same prompt, `status: "rejected"`:

```
resume HTTP 202
{"type":"approval_decided","seq":13,"call_id":"call_00_PfxYc9uQJZBNLVbhLgee4180","status":"denied","by":"resume"}
tool_dispatch rows: 0
{"type":"tool_result","seq":14,…,"is_error":true}
$ ls /tmp/synthia-deny.txt
ls: cannot access '/tmp/synthia-deny.txt': No such file or directory
```

The answer acknowledges the denial ("Since a denial just came through
from the operator …"), and no side effect happened. **PASS.**

### D. Crash and restart

```bash
# sleep 20 in flight; snapshot the log; kill -9; restart the same command
cp "$LOG" /tmp/before-crash.jsonl          # 9 lines
pkill -9 -f 'synthia-server.*--directory'
curl -s "$API/sessions/$SID/follow" --max-time 4 -N   # cold open → repair
```

```
$ head -n "$(wc -l < /tmp/before-crash.jsonl)" "$LOG" | cmp - /tmp/before-crash.jsonl
(no output — the pre-crash prefix is byte-identical)

{"type":"tool_result","seq":9,"data":{"call_id":"call_00_ET_1fKO0NpWpUnSj6isKmJk8829","tool_name":"shell","error":"TOOL_OUTCOME_UNKNOWN","interrupted":true},"is_error":true}
{"type":"step","seq":10,"data":{"kind":"end"}}
{"type":"turn","seq":11,"data":{"kind":"end","reason":"interrupted"}}
```

Only closing rows were appended. A new message afterwards completes a
normal turn:

```
{"type":"turn","seq":20,"data":{"kind":"end","reason":"completed"}}
assistant rows: 2        (one per turn; no duplicated answer)
seq continuity: no gaps
```

**Suspension across a restart** (tutorial step 4): `pkill -9` while an
approval is pending, restart, open the session:

```
{"state":"awaiting_approval","pending":[{"id":"approval-call_00_dcnQ0eJNvz4OenAll4by6656","tool":"shell","risk":"high"}],"rsh":"sha256:03f7a76e…"}
resume HTTP 202
{"type":"approval_decided","seq":13,"call_id":"call_00_dcnQ0eJNvz4OenAll4by6656","status":"approved","by":"resume"}
{"type":"tool_dispatch","seq":14,"call_id":"call_00_dcnQ0eJNvz4OenAll4by6656","tool":"shell"}
$ cat /tmp/synthia-restart-approval.txt
restart-approval
```

The held call was dispatched exactly once; the second `tool_dispatch`
in that log (`seq` 21) carries a different `call_id` — a new call the
model made afterwards. A second `POST …/resume` with the same body is
refused:

```
HTTP 409
{"code":"not_suspended","message":"session '5e7d8dde-…' has no open interrupts"}
```

**PASS.**

### E. Reconnect

```bash
# sleep 12 in flight, then a refresh:
curl -s "$API/sessions/$SID/follow" --max-time 3 -N
```

Mid-run baseline:

```
{"keys":["context","cursor","header","live","pending_interrupts","request_snapshot_hash","rows"],
 "header_state":"running","rows":8,"cursor":8,
 "live_keys":["live_seq","reasoning","running","text","tool_calls","turn_id"],
 "context":{"used":9298,"window":128000},"pending":0,"rsh":true}
live: {"turn_id":"6d14583e-…","live_seq":35,"running":true,"text":"","reasoning":"The user wants me to run exactly `sleep 12; echo reconnect-probe` …"}
```

Baseline after the turn ends:

```
{"rows":15,"cursor":15}     log max seq: 15
tool_result rows containing "reconnect-probe": 1 (log) / 1 (baseline)
```

The whole window is replaced and nothing is duplicated. **PASS.**

### F. Compaction

Config as the tutorial gives it (`context_window: 3000`,
`compaction.enabled: true`, `reserve_tokens: 800`). Six long turns,
then:

```bash
curl -s -X POST $API/sessions/$SID/compact
```

First attempt after three long turns:

```
HTTP 200
{"committed":false,"start":null,"end":null,"shadowed_messages":0,"shadowed_tokens":0,"reason":"under_budget"}
```

This is honest rather than a defect: the deployment's fixed prompt
(system prompt + 8 tool schemas) already prices at ~9.3k tokens, so the
`context_window=3000` the tutorial uses is below the base cost; the
span the compactor may shadow is the *conversation*, and after three
turns that span was still under the `window - reserve` trigger. After
six turns (the tutorial says four to five; this deployment needs more
because its base is larger):

```
HTTP 200
{"committed":true,"start":0,"end":8,"shadowed_messages":8,"shadowed_tokens":1784,"summary":"## Summary\n\n**Goal:** The user asked, four times in a row, …"}
```

and the log carries the paired lifecycle plus the replace:

```
{"type":"compaction_start","seq":55,"token":"01M5TT0V5H5M3QJDWA6XNJVB52","data":{"reason":"manual","start":0,"end":8,"shadowed_token_count":1784}}
{"type":"compaction_summary","seq":56,"token":"01M5TT0V5H5M3QJDWA6XNJVB52","data":{"summary":"## Summary …","shadowed_positions":[…]},"…"}
{"type":"compaction","seq":57,"data":{"summary":"## Summary …","source_indices":[0,1,2,3,4,5,6,7]},"surface_op":{"start":0,"end":8,"source_event_seqs":[1,6,10,15,19,24,28,33]}}
{"type":"compaction_end","seq":58,"token":"01M5TT0V5H5M3QJDWA6XNJVB52","outcome":"committed","data":{"shadowed_messages":8,"shadowed_tokens":1784,"reason":"manual"}}
```

A follow-up question afterwards is answered from the summary:

```
You asked about "topic 1" through "topic 6" — six requests for a ~300-word explanation …
```

The two mutex rules hold:

```
POST …/compact while a turn runs   → HTTP 409 {"code":"session_busy","message":"session '…' is running; a compaction rewrites the surface a live request is built from"}
POST …/compact while suspended     → HTTP 409 {"code":"session_suspended","message":"session '…' is awaiting approval; resolve the interrupts before compacting"}
```

**PASS** (manual/committed path, rows, summary in history, both
mutexes). The automatic path is what fails in scenario A.

### G. Usage, fork, export

```bash
curl -s $API/sessions/$SID/usage
curl -s -X POST $API/sessions/$SID/fork -H 'content-type: application/json' -d '{"from_stream_index":12}'
curl -s -X POST $API/sessions/$SID/fork -H 'content-type: application/json' -d '{}'
curl -s $API/sessions/$SID/export -o export-G.jsonl
```

```
usage: {"session_id":"d7adff13-…","input_tokens":9627,"output_tokens":225,"cache_read_tokens":8960,"turns":1,"context_used":9474,"context_window":3000}
fork(from_stream_index 12): {"session_id":"87f03e75-…","forked_from":"d7adff13-…","rows":12}
fork(whole log):            {"session_id":"68d72a2c-…","forked_from":"d7adff13-…","rows":15}
```

(`context_window` reads 3000 there because that server process was
started from the tutorial config; the endpoint reports the live
policy.)

The child's rows are the parent's, and only its `_meta` line differs
(by session id):

```
$ tail -n +2 <child>/events.jsonl | head -n 14 | cmp - <(tail -n +2 <parent>/events.jsonl | head -n 14)
(no output — rows 2..15 byte-identical)
```

The child continues and the parent does not move:

```
child: UserInput seq 16 "One sentence: what did the shell command print?"
       turn end seq 24 "completed"
       answer "The command printed `reconnect-probe` (exit code 0, no stderr)."
parent lines: 16 (unchanged)
```

The export is byte-faithful NDJSON:

```
$ head -1 export-G.jsonl
{"_meta":{"kind":"synthia_session_meta","schema_version":1,"id":"d7adff13-…"}}
$ wc -l export-G.jsonl     → 16   (log: 16)
$ cmp export-G.jsonl "$LOG"                        → identical
$ jq -r '.seq' | awk 'NR>1 && $1!=p+1 {print "gap"}' → (no gap)
```

**PASS.**

### H. A remote plugin from a non-Rust process

A 60-line Python 3 plugin (`/tmp/synthia-accept-t18/records_plugin.py`,
stdlib only, not committed) answers `synthia/initialize`,
`synthia/tools/list` and `synthia/tools/call` over stdio. It was first
checked frame by frame without the host:

```
$ printf '%s\n%s\n%s\n' '<initialize>' '<tools/list>' '<tools/call records_lookup r-1>' | python3 records_plugin.py
{"jsonrpc": "2.0", "id": 1, "result": {"tools": true, "observe": false, "transform": false, "gate": false}}
{"jsonrpc": "2.0", "id": 2, "result": [{"name": "records_lookup", …}]}
{"jsonrpc": "2.0", "id": 3, "result": {"record": "r-1", "body": "acceptance-record-body", "writer": "python3"}}
```

It is then configured in the deployment:

```yaml
plugins:
  - name: records
    kind: remote
    command: ["python3", "/tmp/synthia-accept-t18/records_plugin.py"]
    transport: stdio
    capabilities: [tools]
    authority: reduced
    risk: unknown
```

`GET $API/plugins`:

```
{"resolved":[{"name":"records","kind":"remote","authority":"reduced","risk":"unknown","tools":["records_lookup"]}],"skipped":[]}
```

The tool is advertised (`GET $API/tools`):

```
["TodoWrite","read","records_lookup","search","shell","skill","web_fetch","write"]
```

An actual call, driven through a model turn:

```
{"type":"tool_dispatch","seq":8,"tool":"records_lookup","call_id":"call_00_ET_YCZyN3yx7YLC0y8SmMxx4294"}
{"type":"tool_result","seq":9,"data":{"content":{"Single":{"type":"text","text":"{\"record\":\"r-1\",\"body\":\"acceptance-record-body\",\"writer\":\"python3\"}"}}},"is_error":null}
answer: "The full record also shows `\"record\": \"r-1\"` and `\"writer\": \"python3\"`."
```

The result text is the Python process's object, serialised by the host.
**PASS.**

### I. Stateless restart: a parked prompt

```bash
# occupy the session, then park a second prompt behind it
curl -s -X POST $API/chat/sessions/$SID/messages -d '{"text":"Use the shell tool to run exactly: sleep 25; echo hold. Then report the output."}'
curl -s -X POST $API/chat/sessions/$SID/messages -d '{"text":"Reply with exactly the token parked-marker-18 and nothing else.","agent_name":"code"}'
# wait for the durable park, then crash and restart
pkill -9 -f 'synthia-server.*--directory'
```

The park is durable before the crash:

```
{"type":"parked_prompt","seq":9,"park_id":"3c973daa-a254-4e99-9620-a5a8e1e1674c","text":"Reply with exactly the token parked-marker-18 and nothing else.","agent_name":"agent"}
```

Opening the session against the restarted server re-queues it:

```
INFO … takeover: re-queued the prompts a previous owner parked user_id="dev" session_id="36b3c903-…" restored=1 state=Idle
```

and the log shows the whole story:

```
seq  9 parked_prompt   park_id 3c973daa-…
seq 10 tool_result     error TOOL_OUTCOME_UNKNOWN   (the crashed turn, repaired)
seq 11 step            end
seq 12 turn            end reason "interrupted"
seq 13 park_consumed   park_id 3c973daa-…           (the park spent, by id)
seq 14 UserInput       "Reply with exactly the token parked-marker-18 and nothing else."
seq 15 turn            start
seq 19 assistant_message  "parked-marker-18"
seq 22 turn            end reason "completed"
```

The parked prompt was not dropped: it was recovered from its own row and
ran to completion, and `park_consumed` names exactly that `park_id`, so
it cannot be spent twice. **PASS.**

### 3.10 What the UI half rests on

This repo verifies the UI in a browser, not with component specs, and
the browser check is not part of this record. What was checked here:

* `npm run build` in `synthia-web/` exits 0 (`✓ built in 685ms`).
* The built bundle carries the surfaces the scenarios exercise:
  `dist/assets/index-DRinsAsH.js` contains `历史已压缩`, `批准一次`,
  `拒绝`, `已注入 step`, `下一回合`, `/follow`, `/resume`, `/compact`;
  `dist/assets/SessionDetailPage-C4jWtx-p.js` contains `/api/v1/plugins`.
* The API facts those surfaces render were asserted above: the
  `follow` baseline (rows, `cursor`, `live`, `context`,
  `pending_interrupts`, `request_snapshot_hash`), the resume decisions,
  the compaction response, the usage numbers, and the plugin record.

A human must still eyeball, in a browser: that text arrives
incrementally with the `Thinking…` indicator and the `Stop` button;
that reasoning renders as a collapsible section; that the tool cards
show running → result in call order; that the approval card shows the
tool, `high` risk and the argument prefix and that its submit button is
disabled until every pending call has a decision; that the steer chip
walks `已落盘 → 已注入 step N → 已消费` (or `下一回合`), and that Stop
in the not-yet-injected state returns the text to the editor; that the
compaction marker renders `历史已压缩` with its shadowed range; that
the footer shows `上下文 N / W` and the session usage; and that the
session detail page lists the remote plugin with its authority and
tools. None of that was verified here.

## 4. What failed, and why

**1. Scenario A fails under the tutorial's own window.** With
`context_window: 3000` the automatic compaction pass shadows a tool
result and leaves its `tool_use` behind, so the provider rejects the
next request (`400 … insufficient tool messages following tool_calls
message`) and the turn ends `reason: "error"`. The deployment's fixed
prompt already costs ~9.3k tokens, so a 3000-token window forces that
pass on the first tool turn. The same scenario passes at
`context_window: 128000` with every other setting unchanged. This is a
pre-existing disagreement between the two compaction boundaries, not a
regression of this branch (§A). It was fixed after this run; §6 records
the fix and the re-run.

**2. `doc-check` was red on this branch, in three branch-added places.**
`crates/synthia-plugin-protocol/src/wire.rs:34` linked `[`GATE`]`
without the disambiguating target; `crates/synthia-server/src/composition.rs:194`
linked `[`Tool::risk`]` where `Tool` is not in scope;
`crates/synthia-server/src/session/controller/mod.rs:441` linked a
`pub(super)` method through a private module. The first stopped the
first `make ci`; the other two only appeared once it got past that
crate. All three were edited to resolve (two code spans, one explicit
target) in this task; the final `make ci` status is in §2. Before those
edits `make ci` exited 2.

**3. `make test-crates` is red, in four crates this branch does not
touch.**

* `synthia-budget` — its crate-level doctest still calls
  `LatencyBudget::new(one_arg)` and passes `Arc<Arc<StaticPricingTable>>`
  where `Arc<dyn PricingTable>` is wanted; both signatures already have
  that shape on `master`.
* `synthia-resilience` — the lib test does not compile:
  `error[E0433]: cannot find \`time\` in \`tokio\`` at
  `src/provider.rs:393`, while the crate declares `tokio` with
  `features = ["macros", "rt", "rt-multi-thread"]`.
* `synthia-tool-memo` — the same compile error, at
  `src/tests.rs:304`, with the same feature list on its `tokio`.
* `synthia-semantic-cache` — its crate-level doctest imports
  `synthia_provider::AnthropicProvider` under a feature set that has it
  off and calls `SemanticCacheProvider::new` with four of five
  arguments (`Arc<dyn Clock>` is missing).

For all four the branch's diff to the crate and to the workspace
`tokio` entry is empty, so these are red on `master` as well. They are
reported, not repaired, because they are outside this branch's change.
`make ci` cannot see any of them: it does not run doctests, and
`test-crates` is not part of it. `synthia-server` itself is green
(`cargo test -p synthia-server -q` → `EXIT=0`).

## 5. What remains

Known open items on this branch, deferred by the design:

* **M7 — the duplicated turn-boundary rule. Resolved by §6.** The
  automatic pass (`synthia-context::summarizing`) used to splice tool
  results without their call while the manual/harness pass
  (`synthia-session::compaction` + `tool_pairing`) balanced the cut;
  scenario A was that disagreement, observed end to end. The automatic
  pass now calls `tool_pairing::balanced_prefix` (the prefix half of
  `balanced_message_cut`) and widens its range to the tool-call group,
  so one predicate decides the boundary for both paths.
* **M8 — a distinct resume error code.** `stale_resume` currently
  covers three different causes: a token whose `last_seq` is behind the
  tail, a token whose `pending` set disagrees with the log, and a log
  that carries no request snapshot at all. The client cannot tell them
  apart.
* **`spawn_gates` / `active_sessions` share the session's lifetime.**
  Both maps are keyed by `(user_id, session_id)` and both are evicted by
  the one function `AppState::forget_session`; a session that leaves
  `active_sessions` for good must take its gate with it, or the gate map
  grows one entry per session the process ever opened. A later
  `Weak`-keyed refactor (gate owned by the controller) is meant to
  replace that function, not its call sites.
* **`LogWriter`'s flush-retry duplication.** `flush` and
  `append_copied` each drain the spill, write one batch and put refused
  rows back. A backend whose `fsync` fails after the bytes were written
  can therefore write a row twice on the retry, and the two paths have
  to stay in step by hand.
* **The automatic path's splice is in-memory only.** The durable
  `SessionEvent::Compaction` row is not written for it: its
  `source_keys` need one key per replaced message, and an assistant row
  has no ledger key, so `SurfaceLedger::resolve` returns `MissingKey`,
  the checkpoint parks and drops the record, and a replay folds the
  **un-compacted** (still valid) history. No provider error follows,
  because the pairing rule keeps both halves — but live and replay see
  different contexts, which is the divergence to close next. Giving
  assistant rows a durable ledger key is the prerequisite.
* **The pre-link `parked_prompt` upgrade re-run.** A park row written
  before the `park_consumed` link existed carries no `park_id`, so
  `repair::parked_prompts` keys it by its ordinal (`park_key`). Rewriting
  such rows to carry a real id — an explicit upgrade pass over old logs
  — is deferred; until then those parks are identified by position.

## 6. Follow-up (2026-10-26): the automatic compaction boundary is fixed

The summarising manager in `synthia-context` no longer splices the
tool-result run alone. Each run is widened into its tool-call group
before the splice: the start moves back to the nearest cut whose prefix
holds no unanswered `tool_use` — `synthia_session::tool_pairing::
balanced_prefix`, the prefix half of the same `balanced_message_cut`
the manual path applies — and the end moves forward until the range
answers every call inside it. The assistant turn the widening swallows
is archived (text and thinking) as a `ToolCallRecord` with
`ToolCallRecordKind::AssistantTurn`, and `CompactionRecord`'s
`start`/`end`/`source_indices` cover the extended span.

Re-run of scenario A, exactly as §3.A, in a fresh workspace
(`/tmp/synthia-accept-t19b`, `context_window: 3000`, `reserve_tokens:
800`, `compaction.enabled: true`, provider `openai` at
`https://api.deepseek.com/v1` with model `deepseek-flash`), same two
curl commands, on the rebuilt binary that carries the fix:

```
{"type":"turn","seq":19,"ts":"2026-10-26T13:01:30.673291091+00:00","data":{"kind":"end","turn":0,"reason":"completed"}}
```

The log holds the compaction as `shadowed_positions: [2,3,4]` (the
assistant turn plus both results) with `outcome: "committed"`; both
`tool_result` rows are present in the log, and the final assistant
answer quotes both outputs (`read` returned the tutorial note and
`ls -la .` listed the directory).

Known limitation recorded by the same run: the durable
`SessionEvent::Compaction` row is **not** written. Its
`source_keys` carry one key per replaced message, and a keyless
assistant row makes `SurfaceLedger::resolve` return `MissingKey`
(`compaction checkpoint: shadowed row 0 has no durable key`), so the
checkpoint parks and drops the record and the durable surface stays
foldable — the log replays the un-compacted (still valid) history.
That is safer than inventing provenance, but it means the automatic
path's splice is in-memory only for now.
