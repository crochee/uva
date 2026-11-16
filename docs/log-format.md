# The session log format

A session's memory is one append-only file, `events.jsonl`, inside
the session's directory. Everything a run said — user messages,
assistant messages, streamed deltas, tool calls and results,
compaction checkpoints — is a JSON object on its own line, in the
order it happened. This page states the on-disk shape, what the
schema version guarantees, what happens when this build meets a
file it cannot read, and the path for changing the format.

The code behind every claim here is
[`crates/uva-session/src/jsonl/`](../crates/uva-session/src/jsonl/);
the events themselves are `SessionEvent` in
[`crates/uva-session/src/events/`](../crates/uva-session/src/events/).

## What a v2 file looks like

The first non-empty line is the **header**; every line after it is
an event row:

```json
{"_meta":{"kind":"uva_session_meta","schema_version":2,"id":"<session id>"}}
{"type":"user_message","seq":1,"ts":"2026-10-27T09:14:02Z","data":{…}}
{"type":"assistant_message","seq":2,"ts":"2026-10-27T09:14:03Z","data":{…}}
{"type":"tool_result","seq":5,"ts":"2026-10-27T09:14:04Z","data":{…}}
```

- The header's only job is to carry the file-level facts: the
  `schema_version` that wrote the file and the session `id`
  (mirroring the folder name, so a stray file still says whose it
  is).
- Each row carries its event kind in `type` (snake_case), a
  monotonic `seq` stamped by the sink at append time, and an
  ISO-8601 `ts`. Rows are chronological; readers must not pick
  `seq` values themselves.
- Surface rows (messages, tool results) may carry a `surface_op`
  saying how the row joins the model-visible transcript, and a
  tool-result row carries `is_error: true` only when the tool
  failed — a successful one omits the flag, so a reloaded log
  says what the live event said. A compaction row carries a
  `replace` op naming the span it shadows, so replaying the log
  reproduces the post-compaction transcript instead of the
  pre-compaction one. The span is a position in the
  **model-visible** surface — the order the next LLM pass sees,
  in which a user turn that landed between a `tool_use` and its
  `tool_result` waits for that result — and the op's
  `source_event_seqs` names the rows the span covers, one seq per
  row, so a reader can tell which rows the replace shadows. The compaction lifecycle
  (`compaction_start` / `compaction_summary` / `compaction_end`)
  brackets a summariser call; an unmatched `start` is the crash
  marker the repair path detects.
- A `user_message` row may carry `attachments`: one re-readable
  reference per attachment the turn carried (`kind`, optional `name`,
  `mime_type` and `byte_len`, plus the content `hash` the deployment's
  attachment store resolves). The bytes never ride the log — a
  multimodal payload stays in the store — and a text-only turn omits
  the field entirely, so the common row shape is unchanged. `hash` is
  optional: `unstored` says why when it is absent (the payload was
  never local, or the store refused it), so "attachment unavailable"
  can tell a reference that will never resolve from a store that
  dropped it.
- A `feedback` row records the client's rating of one message
  (`message_id` + `thumbs_up`). It is **log-only**: nothing in the
  runtime reads it back and it never enters the transcript, so it is
  the audit record an analytics surface aggregates rather than a fact
  the model sees.
  `message_id` is the rated row's **ordinal** — that row's `seq`, in
  decimal. The ordinal is a row's identity in the log (there is no
  second id), so the server resolves the rating against the session's
  own rows and refuses a target that names no row, or names a row
  that is not an assistant message: a rating that names nothing is
  noise in an append-only audit log. The field keeps its name
  because the version gate below refuses an old log rather than
  migrating it — what changed is what the field holds, not the row's
  shape, so the schema version does not move. A log written before
  this change carries a client-side label in the same field (a
  browser-generated UUID, or the TUI's `msg-<n>` count); such a row
  names no ordinal and an analytics reader must not read it as one.
- A `warning` row records a fact the runtime could not report any
  other way. Its `data.kind` is a stable slug — `config_drift` for a
  resume that accepted a changed request envelope,
  `strict_fold_refused` for a session log the strict fold refused —
  and its `data.message` says what happened. A `warning` row is
  **log-only**: the fold ignores it, so the transcript the model sees
  is the transcript the log had without the row.
- A `hook_block` row records a policy that refused a tool call — a
  `Block` (the call returns an error result) or a `Terminate` (the run
  ends). Its `data.hook` names the policy that refused — the
  registered name of the hook, gate or guard that vetoed —
  `data.reason` says why it refused, and `data.tool` / `data.call_id`
  name the call it refused. A `hook_block` row is **log-only**: the fold ignores it,
  so the transcript the model sees is the transcript the log had
  without the row. Without it a veto's reason reaches the log only
  through the model-visible tool result, which says that *something*
  blocked the call but not which policy did.
- A `tool_output` row records one partial-output chunk of a
  **running** tool call: `call_id` names the call, `tool` names the
  tool, and `data.output` is the chunk's text. A streaming tool
  (today `shell`) writes one while it runs, so a reload replays the
  progress the live client already saw instead of losing it with the
  process that produced it. The row is **log-only**: the settled
  `tool_result` row is the model's copy, and the fold ignores this
  one. The row is additive — it does not change the shape of any row
  written before it, so the schema version does not move.
- A `tool_output` row is **bounded at the writer boundary**. Its
  `data.output` keeps at most `MAX_TOOL_OUTPUT_BYTES` (64 KiB, measured
  on the escaped form): an over-long chunk keeps its **tail** — the
  newest output is what a reload needs — and `data.original_bytes`
  records how many output bytes the row summarized. The bound is
  idempotent, so a rewritten row is never cut twice.
- The chunks of one call that arrive before a flush **merge into one
  row**: the writer holds one pending `tool_output` row per `call_id`
  (tail-window bounded) and the flush writes it under the first
  chunk's ordinal. A tool that prints a megabyte therefore costs one
  bounded row per flush, not one row per pipe read — the log is
  bounded by the flush cadence rather than by the tool's output rate,
  and the bytes are durable from that flush onward.
- A `turn` row brackets a run: `data.kind` is `start` or `end`,
  and the `end` row's `data.reason` names the outcome
  (`completed`, `error`, `cancelled`, …). A failed turn adds
  `data.message` with the text the run stopped with, so a reader
  (the page's banner, an operator reading the log) learns *why* the
  turn failed instead of inferring it from the word `error`. The key
  is absent on every other outcome.
- Durability: every append is followed by an `fsync` — an `Ok`
  from the write path means the bytes are on the disk. A retry
  after a failed flush does not duplicate a row: the sink skips
  any row whose `seq` it has already written.

## The artefact set

A delegated child — one `task` call — leaves one **artefact set**
next to its own session log, in `<session dir>/artifacts/`:

| file | what it is |
|---|---|
| `<id>.jsonl` | a frozen copy of the child's `events.jsonl` at the moment its round ended |
| `<id>.md` | the same transcript rendered as markdown: who spoke, which tools ran, how the round ended |
| `<id>.patch` | the diff an isolated round produced, when it produced one |

- `<id>` is the child session id — the same handle `agents_list`
  reports and `agents_message` takes.
- The set is a **projection**, never a second source: the child's
  log stays the durable truth. The `.jsonl` copy says what the round
  had produced when it ended; a follow-up or a cold wake appends to
  the log and rewrites the set.
- `.patch` is written only for a round that ran in a git worktree
  **and** changed something (`git diff <base sha>` before the
  worktree is cleaned up). An empty diff is not written: an empty
  patch would be a claim about a round that made none.
- A round without a durable session — a host that keeps no log —
  gets no artefact set. Writing is best-effort: the round already
  happened, so a failed write is a log line, not a failed result.
- Retention is the session's: the set is rewritten on every round and
  has no independent lifetime. Deleting a session deletes its
  artefacts with it.

The convention is implemented in
[`crates/uva-tool-task/src/control/artifacts.rs`](../crates/uva-tool-task/src/control/artifacts.rs);
the directory comes from the host through the control surface's
`SessionIndex::session_dir`, so a deployment that stores sessions
somewhere other than the filesystem writes none.

## The version gate

The schema version is a **file-level** fact, read once when the
file is opened, before a single row is returned. This build reads
exactly version 2 (`CURRENT_SCHEMA_VERSION`):

| The file says | The reader does |
|---|---|
| header with `schema_version: 2` | reads it |
| header with `schema_version: 1` | **refuses** — `UnsupportedSchemaVersion { found: 1, supported: 2 }` |
| no header at all (a pre-versioning file) | **refuses** — the implied version is 0, `found: 0` |
| header with any version above 2 | **refuses** — same variant, `found` carries the real number |

The refusal is a dedicated error, not a generic I/O failure, and
its message names the next step. An **older** file:

```text
session log schema version 1 is older than this build reads (this
build supports 2); export the session with a build that reads v1,
or reopen it there after migrating the log
```

A **newer** file gets the same shape with the other reason:

```text
session log schema version 3 is newer than this build reads (this
build supports 2); export or reopen the session with a build that
supports v3
```

Because the version lives in the header, a file that mixes rows
from two formats is caught at open time, not halfway through a
replay. The gate is pinned by tests in `jsonl.rs`
(`jsonl_sink_refuses_a_v1_log`, `jsonl_sink_refuses_a_newer_log`,
`jsonl_sink_refuses_a_headerless_v0_log`, `jsonl_sink_reads_a_v2_log`).

## Why there is no migration

Deliberate. The workspace is on the `0.1` line, the population of
v0/v1 logs is small, and migration code is more likely to
introduce a bug than reopening a session is. The deeper reason is
honesty about what a log *is*: if a migrated file opens but its
compaction history no longer proves what it shadowed, the reader
holds a transcript that looks fine and is not the one that ran.
A loud refusal — with the version it found, the version it
supports, and the next step — loses you a file. A silent downgrade
loses you the truth. This project takes the first.

Two supporting behaviours follow from the same decision:

- the writer never backfills or rewrites a header when reopening
  an existing file — that would change the row count and hide the
  version the file was written with;
- the *append* path is equally blind to versions it does not
  know: it keeps the file as it found it.

## Changing the format

The forward path, when a row shape or invariant changes in a way
older readers cannot safely consume:

1. Bump `CURRENT_SCHEMA_VERSION` in
   [`crates/uva-session/src/jsonl/`](../crates/uva-session/src/jsonl/).
   The gate needs no separate wiring — the reader compares
   against the constant, so every other version (older and newer)
   is refused from the moment the constant moves.
2. Regenerate the repository's own fixtures: anything that writes
   a header by hand must say the new version.
3. Update this page and the changelog **in the same commit** —
   the version bump, the gate, and the documentation of what the
   new version means move together, never separately.

That trio is the whole ceremony. A format change that skips step 3
leaves a reader who cannot know what the number in the header
means — which is the state this gate exists to prevent.
