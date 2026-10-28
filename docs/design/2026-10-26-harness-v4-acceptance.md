# Harness v4 acceptance — 2026-10-26

What this file is: the mechanical record of the `feature/harness-v4`
acceptance run. Every number and every quoted line was produced by a
command on this branch in this worktree; nothing here is copied from
the design or the plan.

* Worktree: `/home/crochee/workspace/elysper/.worktrees/harness-v4`
* Branch: `feature/harness-v4`, HEAD `e0892c50` at the start of the run
* Server binary: `target/debug/elysper-server` (`cargo build -p
  elysper-server`)
* Acceptance workspace: `/tmp/elysper-accept-t18` (server configs, the
  Python plugin, and the session logs live there and are **not**
  committed)
* Model provider: a real OpenAI-compatible endpoint
  (`https://api.deepseek.com/v1`, model `deepseek-flash`), key read
  from `DEEPSEEK_API_KEY`. The harness, the server, the tools, the log
  and the HTTP surface under test are this repo's; only the model is
  external.
* Web: assets built with `npm run build` in `elysper-web/`.

`$DIR` below means `/tmp/elysper-accept-t18`; `$API` means
`http://127.0.0.1:8099/api/v1`; a session log is
`$DIR/ws/sessions/dev/<session-id>/events.jsonl`.

**Status at a glance (after the remainders round, 2026-10-27).** All
five gates exit `0` — `make ci` (which now includes `test-crates`),
`make check-web`, `make examples`, `make bench-check`, and
`make test-crates` on its own. Scenarios A–I were rerun end to end on
the v2 log format and all pass, including scenario A at the
tutorial's own `context_window: 3000` with the compaction surface now
durable and replay-equal. The remainders items are closed one by one
in §0 below; the original run's evidence (§1–§6) is kept unchanged
because it is the discovery record — §0's "now" column is the current
state.

## 0. Remainders closure (2026-10-27)

One row per plan task. "Evidence" names the test or command that
proves the invariant; the original discovery evidence stays in the
section named in the last column.

| # | item | invariant | evidence | result | was |
|---|---|---|---|---|---|
| 1 | Writer retry never double-writes | a batch the backend already wrote is not written again on retry, barrier or not | commit `a2b04375` + `5becc27d`; `writer/tests.rs`: `a_retry_after_a_failed_flush_does_not_duplicate_a_row`, `a_row_the_log_already_holds_is_not_written_again`, `a_flush_never_writes_past_a_reserved_row`, `a_failed_barrier_leaves_the_rows_written`; `make test-crates` exit 0 | CLOSED | §4 item 3 |
| 2 | Every shadowable row has a durable identity | the ledger can name every row a compaction replaces, assistant rows included | commit `7a386858`; `ledger_key/tests.rs` (`key_of`), `compaction_keys.rs` | CLOSED | §5 |
| 3 | Compaction writes a durable event; replay == live | the `SessionEvent::Compaction` row lands and a cold `follow` folds the same surface the live session shows | scenario F rerun: baseline 58 rows == log 58 rows, `compaction` rows 1 == 1; scenario A rerun: turn `completed`, baseline rows 62 == log rows 62 | CLOSED | §5, §6 |
| 4 | Log format v2; older/newer logs refused with an operator-actionable error | a v1 log is refused, never migrated, and the error names both versions and the way out | hand-crafted v1 log → `HTTP 500 {"code":"session_log_read", … "schema version 1 is older than this build reads (this build supports 2); export the session with a build that reads v1, or reopen it there after migrating the log"}` (§7) | CLOSED | — |
| 5 | Resume outcomes split and idempotent | `resume_conflict` / `resume_no_snapshot` are distinct codes; a replayed resume decision returns `200 OK` and never dispatches twice | commit `16824317` + `c36253d7`; `follow::a_resume_conflict_names_a_log_that_moved_on`, `…_a_pending_set_the_log_does_not_hold`, `a_resume_without_a_request_snapshot_is_refused`; scenarios C/D rerun: second identical `POST …/resume` → `HTTP 200`, dispatches still 1 | CLOSED | §5 |
| 6 | The spawn gate dies with its controller | `spawn_gates` holds no entry for a controller that is gone | commit `22d30024`; `sessions::the_spawn_gate_map_does_not_outlive_the_controllers_it_belongs_to` | CLOSED | §5 |
| 7 | Docs state the real dependency and public surface | consumer-facing counts match `cargo tree` and the curated re-exports | commits `dd0f9dd3`, `c2c46f7b`, `46265448`, `42b67d17`, `797d30b6`; `docs_truth` 9/9 green | CLOSED | §5 |
| 8 | One turn-boundary rule; keyless park rows retired | `follow` asks the harness's `suspended_segment`, and a `parked_prompt` without `park_id` is warned and skipped, not guessed by position | commit `42889677`; `the_window_folds_the_harnesss_suspended_segment`; `a_keyless_park_row_is_warned_once_and_skipped` | CLOSED | §5 |
| 9 | `elysper-resilience` / `elysper-tool-memo` green | the two crates compile and their suites pass | commit `0e0978f5`; `make test-crates` exit 0 (§7) | CLOSED | §4 item 3 |
| 10 | `elysper-budget` / `elysper-semantic-cache` doctests green | both crates' doctests compile against the real signatures | commit `6b6b80d3` + `8d895820`; `make test-crates` exit 0 (§7) | CLOSED | §4 item 3 |
| 11 | Curated public API frozen | a name appearing or disappearing without the baseline changing fails the gate (`scripts/check-frozen-api.sh` compares the live curated surface against `scripts/frozen-api/baseline`; moving `docs/api-frozen.md` in the same commit is the review convention the script's header states, not something the gate itself enforces) | commit `aec2999d`; `make check-frozen-api` exit 0; 435 frozen names | CLOSED | — |
| 12 | Handover docs exist | `docs/api-frozen.md`, `docs/api-surface.md`, `docs/log-format.md`, `docs/contributing/first-steps.md` all present and current | the four files on this branch; added by `aec2999d` (`docs/api-frozen.md`) and `bb5203b5` (the other three) | CLOSED | — |
| 13 | Acceptance rerun, record closed | five gates exit 0 and scenarios A–I rerun on the v2 format | §7 (this round) | CLOSED (this document) | — |

The design's eighth leftover — `awaiting_approval` swallowing errors —
was already fixed on this branch before the remainders plan
(`inner.rs` uses `try_open_interrupts().await?`; the status-only
`open_interrupts` has no error channel by design), so it is not a row
here; see the plan's self-check.

## 1. The three dependency counts

```
$ cargo tree -p elysper-harness -e normal | grep -c tokio
0
$ cargo tree -p elysper-harness -e normal | grep -cE "reqwest|hyper|rustls|h2|tower"
0
$ cargo tree -p elysper-plugin-protocol -e normal | grep -c tokio
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
| `make ci` (first run) | 2 | Stopped in `doc-check`: `error: unresolved link to \`GATE\`` at `crates/elysper-plugin-protocol/src/wire.rs:34` |
| `make ci` (second run) | 2 | `doc-check` then reported two further branch-added links: `Tool::risk` at `crates/elysper-server/src/composition.rs:194` and `ControllerInner::awaiting_approval` at `crates/elysper-server/src/session/controller/mod.rs:441` |
| `make ci` (final run) | 0 | `fmt-check`, `lint-rust`, `doc-check`, `test-guides` (default and the seven-feature subset), all 15 check targets, and `cargo deny` (`bans ok, licenses ok, sources ok`) |
| `make check-web` | 0 | `tsc --noEmit`, `eslint`, `prettier --check` all clean (`All matched files use Prettier code style!`) |
| `make test-crates` | 2 | Stopped at `elysper-budget`: its crate-level doctest does not compile (see §4) |
| `make examples` | 0 | 55 example/consumer targets; the harness-only consumer printed `tokio in tree: 0` and `HARNESS-ONLY: OK` |
| `make bench-check` | 0 | `all performance invariants hold` (7 invariants OK; e.g. `descriptors_cached vs descriptors 1713.9x (min 100.0x)`) |

`make ci` runs `fmt-check lint-rust doc-check test-guides
check-mvp-deps check-no-runtime check-harness-features
check-public-api-runtime check-pub-surface check-pub-surface-strict
check-claim-language check-clock check-harness-shape
check-harness-docs check-single-writer check-plugin-shape
check-test-layout check-deny`. It did **not** run `make test-crates`,
which is why the doctest failures below were invisible to it.
*(Changed in the remainders round: `ci` now ends with `test-crates` —
§0 row 13, §7.1.)*

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
elysper                          EXIT=0   (from make test-crates)
elysper-attachment               EXIT=0   (from make test-crates)
elysper-budget                   EXIT=101 (from make test-crates; doctest)
elysper-context                  EXIT=0
elysper-core                     EXIT=0
elysper-eval                     EXIT=0
elysper-harness                  EXIT=0
elysper-harness-tokio            EXIT=0
elysper-hpc-batch                EXIT=0
elysper-macros                   EXIT=0
elysper-mcp                      EXIT=0
elysper-mcp-server               EXIT=0
elysper-plugin-host              EXIT=0
elysper-plugin-protocol          EXIT=0
elysper-provider                 EXIT=0
elysper-reflection               EXIT=0
elysper-resilience               EXIT=101 (compile error in the lib test)
elysper-safety                   EXIT=0
elysper-scheduler                EXIT=0
elysper-search                   EXIT=0
elysper-semantic-cache           EXIT=101 (crate-level doctest)
elysper-session                  EXIT=0
elysper-skill                    EXIT=0
elysper-steering                 EXIT=0
elysper-strategy-best-of-n       EXIT=0
elysper-strategy-cot             EXIT=0
elysper-strategy-plan-execute    EXIT=0
elysper-telemetry                EXIT=0
elysper-test-support             EXIT=0
elysper-tool                     EXIT=0
elysper-tool-memo                EXIT=101 (compile error in the lib test)
elysper-tool-read                EXIT=0
elysper-tool-scheduler           EXIT=0
elysper-tool-search              EXIT=0
elysper-tool-shell               EXIT=0
elysper-tool-task                EXIT=0
elysper-tool-todo                EXIT=0
elysper-tool-web                 EXIT=0
elysper-tool-write               EXIT=0
elysper-workflow                 EXIT=0
elysper-server                   EXIT=0
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

That is the same shape the tutorial's own `elysper-session`
`tool_pairing` module exists to prevent: a replace that straddles an
`assistant/tool_use` ↔ `tool/result` boundary produces an orphan call.
The automatic path lives in `elysper-context`'s summariser, which
splices only the tool-result run ("The assistant turn that contained
the original `tool_use` blocks stays untouched", `manager.rs:346`);
the manual/harness path in `elysper-session` applies
`balanced_cut_at_or_before` instead. The two rules disagree.

This is **not** caused by this branch: `crates/elysper-context` is
unchanged except for the removal of one unused constructor, and the
splice comment is present on `master`.

Re-run with the same commands and only `context_window: 128000` (every
other setting identical), the scenario passes:

```
{"type":"turn","data":{"kind":"end","turn":0,"reason":"completed"}}
tool_result #1: "   1 elysper tutorial\n"
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

> **Update — 2026-10-28 (Elysper v5, task 8 "durable parks").** The
> evidence above is the frozen pre-task-8 observation and stays as it
> was recorded. The *row shape* it observed has since moved: a queued
> message is now parked durably before it is queued, so the same
> message writes **three** rows — `parked_prompt` (16), the
> `park_consumed` row that spends the park (17), and the turn's own
> `UserInput` (18) — where this record shows a single `UserInput` at
> 16. The turn boundary itself did not move: the queued message is
> still answered as its own turn, after the first turn's `end`, and
> the `sleep` it did not interrupt still returns normally. The rows
> the parked turn writes are what a takeover replays from, so the
> ordinals after 16 shift by two.

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
  -d '{"text":"Use the shell tool to run exactly: echo elysper-approval-probe > /tmp/elysper-approval.txt ; then report the command'\''s output and exit status."}'
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
$ cat /tmp/elysper-approval.txt
elysper-approval-probe
```

The turn then completes with an answer that correctly explains the
redirection. **PASS.**

**Deny.** Same prompt, `status: "rejected"`:

```
resume HTTP 202
{"type":"approval_decided","seq":13,"call_id":"call_00_PfxYc9uQJZBNLVbhLgee4180","status":"denied","by":"resume"}
tool_dispatch rows: 0
{"type":"tool_result","seq":14,…,"is_error":true}
$ ls /tmp/elysper-deny.txt
ls: cannot access '/tmp/elysper-deny.txt': No such file or directory
```

The answer acknowledges the denial ("Since a denial just came through
from the operator …"), and no side effect happened. **PASS.**

### D. Crash and restart

```bash
# sleep 20 in flight; snapshot the log; kill -9; restart the same command
cp "$LOG" /tmp/before-crash.jsonl          # 9 lines
pkill -9 -f 'elysper-server.*--directory'
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
$ cat /tmp/elysper-restart-approval.txt
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
{"_meta":{"kind":"elysper_session_meta","schema_version":1,"id":"d7adff13-…"}}
$ wc -l export-G.jsonl     → 16   (log: 16)
$ cmp export-G.jsonl "$LOG"                        → identical
$ jq -r '.seq' | awk 'NR>1 && $1!=p+1 {print "gap"}' → (no gap)
```

**PASS.**

### H. A remote plugin from a non-Rust process

A 60-line Python 3 plugin (`/tmp/elysper-accept-t18/records_plugin.py`,
stdlib only, not committed) answers `elysper/initialize`,
`elysper/tools/list` and `elysper/tools/call` over stdio. It was first
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
    command: ["python3", "/tmp/elysper-accept-t18/records_plugin.py"]
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
pkill -9 -f 'elysper-server.*--directory'
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

* `npm run build` in `elysper-web/` exits 0 (`✓ built in 685ms`).
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
`crates/elysper-plugin-protocol/src/wire.rs:34` linked `[`GATE`]`
without the disambiguating target; `crates/elysper-server/src/composition.rs:194`
linked `[`Tool::risk`]` where `Tool` is not in scope;
`crates/elysper-server/src/session/controller/mod.rs:441` linked a
`pub(super)` method through a private module. The first stopped the
first `make ci`; the other two only appeared once it got past that
crate. All three were edited to resolve (two code spans, one explicit
target) in this task; the final `make ci` status is in §2. Before those
edits `make ci` exited 2.

**3. `make test-crates` is red, in four crates this branch does not
touch.**

* `elysper-budget` — its crate-level doctest still calls
  `LatencyBudget::new(one_arg)` and passes `Arc<Arc<StaticPricingTable>>`
  where `Arc<dyn PricingTable>` is wanted; both signatures already have
  that shape on `master`.
* `elysper-resilience` — the lib test does not compile:
  `error[E0433]: cannot find \`time\` in \`tokio\`` at
  `src/provider.rs:393`, while the crate declares `tokio` with
  `features = ["macros", "rt", "rt-multi-thread"]`.
* `elysper-tool-memo` — the same compile error, at
  `src/tests.rs:304`, with the same feature list on its `tokio`.
* `elysper-semantic-cache` — its crate-level doctest imports
  `elysper_provider::AnthropicProvider` under a feature set that has it
  off and calls `SemanticCacheProvider::new` with four of five
  arguments (`Arc<dyn Clock>` is missing).

For all four the branch's diff to the crate and to the workspace
`tokio` entry is empty, so these are red on `master` as well. They are
reported, not repaired, because they are outside this branch's change.
`make ci` cannot see any of them: it does not run doctests, and
`test-crates` is not part of it. `elysper-server` itself is green
(`cargo test -p elysper-server -q` → `EXIT=0`).

## 5. What remained at the time of the original run

(Every item below is closed by the remainders round — see §0 for the
per-item closure with evidence. This list is kept as it stood.)

* **M7 — the duplicated turn-boundary rule. Resolved by §6.** The
  automatic pass (`elysper-context::summarizing`) used to splice tool
  results without their call while the manual/harness pass
  (`elysper-session::compaction` + `tool_pairing`) balanced the cut;
  scenario A was that disagreement, observed end to end. The automatic
  pass now calls `tool_pairing::balanced_prefix` (the prefix half of
  `balanced_message_cut`) and widens its range to the tool-call group,
  so one predicate decides the boundary for both paths.
* **M8 — a distinct resume error code.** *(Closed: commit `16824317`;
  `resume_conflict` and `resume_no_snapshot` are distinct codes, and a
  replayed decision now answers `200 OK` without re-dispatching —
  commit `c36253d7`, §7 scenarios C/D.)* `stale_resume` previously
  covered three different causes: a token whose `last_seq` is behind the
  tail, a token whose `pending` set disagrees with the log, and a log
  that carries no request snapshot at all. The client cannot tell them
  apart.
* **`spawn_gates` / `active_sessions` share the session's lifetime.**
  *(Closed: commit `22d30024` — the gate is owned by the controller.)*
  Both maps used to be keyed by `(user_id, session_id)` and evicted by
  the one function `AppState::forget_session`; a session that left
  `active_sessions` for good had to take its gate with it, or the gate
  map grew one entry per session the process ever opened.
* **`LogWriter`'s flush-retry duplication.** *(Closed: commits
  `a2b04375` + `5becc27d` — the fs layer reports whether a refused
  batch's bytes landed, and the writer skips rows it already wrote.)*
  `flush` and `append_copied` each used to drain the spill, write one
  batch and put refused rows back; a backend whose `fsync` failed after
  the bytes were written could write a row twice on the retry.
* **The automatic path's splice is in-memory only.** *(Closed: commit
  `7a386858` — every shadowable row has a durable ledger key, so the
  compaction checkpoint resolves and the durable `Compaction` row
  lands; the §7 rerun replays it: baseline rows == log rows with the
  compaction surface folded in both.)* The durable
  `SessionEvent::Compaction` row used not to be written: its
  `source_keys` needed one key per replaced message, an assistant row
  had no ledger key, `SurfaceLedger::resolve` returned `MissingKey`,
  and the checkpoint parked and dropped the record, so a replay folded
  the **un-compacted** history while the live surface showed the
  splice.
* **The pre-link `parked_prompt` upgrade re-run.** *(Closed as
  "retired, not migrated": commit `42889677` — a keyless park row is
  warned once and skipped, never guessed by position; v1 logs are
  refused outright by the v2 gate, so no pre-link row can reach the
  repair path on this build.)* A park row written before the
  `park_consumed` link existed carried no `park_id` and was keyed by
  its ordinal; rewriting such rows was deferred.

## 6. Follow-up (2026-10-26): the automatic compaction boundary is fixed

The summarising manager in `elysper-context` no longer splices the
tool-result run alone. Each run is widened into its tool-call group
before the splice: the start moves back to the nearest cut whose prefix
holds no unanswered `tool_use` — `elysper_session::tool_pairing::
balanced_prefix`, the prefix half of the same `balanced_message_cut`
the manual path applies — and the end moves forward until the range
answers every call inside it. The assistant turn the widening swallows
is archived (text and thinking) as a `ToolCallRecord` with
`ToolCallRecordKind::AssistantTurn`, and `CompactionRecord`'s
`start`/`end`/`source_indices` cover the extended span.

Re-run of scenario A, exactly as §3.A, in a fresh workspace
(`/tmp/elysper-accept-t19b`, `context_window: 3000`, `reserve_tokens:
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

*(Superseded: as of the remainders round every shadowable row has a
durable ledger key, the checkpoint resolves, and the durable
`Compaction` row lands — §0 row 3, §7.2 A/F.)*

## 7. Remainders acceptance rerun (2026-10-27)

HEAD before this round's commits: `bb5203b5`. This section's
workspace is `/tmp/elysper-accept-t20` — §3's `$DIR` (the original
run) was `/tmp/elysper-accept-t18`; the two are distinct and each
section's paths are its own. Two deployments: `ws` at
`context_window: 128000` for B–I, `ws-a` at `context_window: 3000`
for A and F; the Python plugin was recreated from the record — the
original is not committed — and its frame-by-frame smoke test answered
`elysper/initialize`, `elysper/tools/list`, `elysper/tools/call`.
Same provider and model as the original run. A mid-run `/tmp` wipe
after scenario H destroyed that deployment's logs: the B–H lines
below survive as quotes captured at run time, while A, F and I were
run (for A and F, rerun from scratch) on the recreated workspace and
are quoted from that state. Everything below was produced by a
command on this branch in this worktree.

### 7.1 Gates

| target | exit | what it said |
|---|---|---|
| `make test-crates` | 0 | every member crate green, the four §4 item 3 crates included |
| `make ci` (run 1) | 2 | `doc-check`: `fs.rs:33`/`:130` linked private items (`AppendFailure`, `Self::append_owned`) — committed code, caught by the gate |
| `make ci` (run 2) | 2 | `doc-check`: `harness_tokio.rs:3-6` linked unresolved `elysper::core::…` paths |
| `make ci` (final) | 0 | after the two doc-link fixes (plain code spans / correct `crate::core::…` paths); `test-crates` is now one of its prerequisites (Makefile `ci:` target) |
| `make check-web` | 0 | tsc / eslint / prettier clean |
| `make examples` | 0 | 55 targets + the three consumer proofs; `tokio in tree: 0`, `HARNESS-ONLY: OK` |
| `make bench-check` | 0 | `all performance invariants hold` |

The two non-zero `make ci` runs were real defects in committed doc
comments (Task 1's and the facade feature's); both were fixed in the
source, and no gate was edited to go green. `make ci` now lists
`test-crates` among its prerequisites (Makefile `ci:` target), so the
four red crates of §4 item 3 can no longer hide from it.

### 7.2 Scenarios A–I (v2 log format throughout)

A (two tools, `context_window: 3000`): turn end
`{"kind":"end","turn":0,"reason":"completed"}`; the log's rows by
type: 9 `tool_result`, 5 `compaction` (with 5 each of
`compaction_start` / `compaction_summary` / `compaction_end` — five
compaction passes under the 3000-token window); the `follow`
baseline holds **62 rows == the log's 62 rows**; the answer quotes
both outputs.

B (queue / steer / stop): queued message lands as `UserInput` at the
seq after the first turn's end and is answered (`queue-marker`);
steer lands as `user_message` with `source:"steer"` inside the turn
and the answer carries `steer-marker`; cancel during `sleep 30`
answers `204`, the turn ends `cancelled`, both steer rows durable.

> **Update — 2026-10-28 (Elysper v5, task 8 "durable parks").** The
> scenario is unchanged; the *row shape* it observed has moved. A
> queued message now writes three rows — `parked_prompt`, then the
> `park_consumed` row that spends the park, then the turn's own
> `UserInput` — where the frozen evidence in §B shows one `UserInput`
> at `seq` 16 (so the turn's `UserInput` is now at 18). The queued
> message is still its own turn, still answered, and still lands
> after the first turn's end.

C (approval): high-risk shell call suspends (`approval_asked`,
`risk:"high"`, turn `suspended`, baseline `awaiting_approval`);
`resume` with `status:"resolved"` → `202`, `approval_decided …
status:"approved"`, exactly one `tool_dispatch`,
`/tmp/elysper-approval.txt` written, turn `completed`; **the same
resume body replayed → `200`, still one dispatch.** Deny
(`status:"rejected"`) → `202`, zero dispatches, `is_error:true` tool
result, `/tmp/elysper-deny.txt` absent. (`status:"approved"` is not a
wire variant — the 422 lists `resolved|cancelled|rejected`.)

D (crash and restart): `kill -9` mid-`sleep 20`; cold `follow` after
restart — pre-crash prefix byte-identical (`cmp` clean), only closing
rows appended (`TOOL_OUTCOME_UNKNOWN`, `interrupted:true`; step end;
turn end `interrupted`); a later message completes normally with no
seq gaps. Suspension across restart: baseline still
`awaiting_approval` after the crash, `resume` → `202`, decided once,
one dispatch, file written; the same resume body replayed → `200`,
still one dispatch.

E (reconnect): mid-run baseline `state:"running"` with the live keys;
after the turn `rows == cursor == log max seq` (15/15/15); the window
is replaced whole, nothing duplicated.

F (compaction, `context_window: 3000`): six long turns then manual
`POST …/compact` → `200 {"committed":true,"start":0,"end":11,
"shadowed_messages":11,"shadowed_tokens":4057,…}`; the log carries
the four-row lifecycle (`compaction_start`/`compaction_summary`/
`compaction`/`compaction_end`, outcome `committed`); a follow-up
question is answered from the summary; replay == live (baseline 58
rows == log 58 rows, one `compaction` row on each side). Both mutexes
hold: `compact` while running → `409 session_busy`; while suspended →
`409 session_suspended`.

G (usage, fork, export): usage reports token counts, turns and the
live window; `fork` from index 12 and whole-log both return
`forked_from` + rows, the child's first rows are byte-identical to
the parent's (`cmp` clean), the child answers a follow-up (rows grow)
while the parent's log does not move; the export is byte-identical to
the log (`cmp` clean), `_meta` at `schema_version: 2`, seq-continuous.

H (remote Python plugin): `GET /plugins` resolves `records` with
`authority:"reduced"`, `risk:"unknown"`, `tools:["records_lookup"]`;
`GET /tools` advertises it; a model turn dispatches it and the answer
reports the Python process's object (`r-1`, `acceptance-record-body`,
`python3`).

I (stateless restart of a parked prompt): the park is durable before
the crash (`parked_prompt` with `park_id`); after `kill -9` and
restart, opening the session re-queues it (`takeover: re-queued the
prompts a previous owner parked … restored=1`), the crashed turn is
repaired (`TOOL_OUTCOME_UNKNOWN`, turn end `interrupted`), and the
park is spent by id (`park_consumed` naming that `park_id`) before
its `UserInput` runs; the final answer is exactly the parked token.

### 7.3 v1 log refusal (Task 4's acceptance point)

A hand-crafted `schema_version: 1` log under the sessions dir:

```
HTTP 500
{"code":"session_log_read","message":"session error: read session log: session log schema version 1 is older than this build reads (this build supports 2); export the session with a build that reads v1, or reopen it there after migrating the log"}
```

The error names the version read, the version this build supports,
and both ways out (export with an older build / reopen there after
migrating) — operator-actionable, and the log is refused, not
migrated. The refusal in the other direction is locked by
`jsonl.rs`'s tests: `jsonl_sink_refuses_a_v1_log`,
`jsonl_sink_refuses_a_newer_log` and
`jsonl_sink_refuses_a_headerless_v0_log`.
