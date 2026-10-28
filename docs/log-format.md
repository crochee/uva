# The session log format

A session's memory is one append-only file, `events.jsonl`, inside
the session's directory. Everything a run said — user messages,
assistant messages, streamed deltas, tool calls and results,
compaction checkpoints — is a JSON object on its own line, in the
order it happened. This page states the on-disk shape, what the
schema version guarantees, what happens when this build meets a
file it cannot read, and the path for changing the format.

The code behind every claim here is
[`crates/elysper-session/src/jsonl.rs`](crates/elysper-session/src/jsonl.rs);
the events themselves are `SessionEvent` in
[`crates/elysper-session/src/events.rs`](crates/elysper-session/src/events.rs).

## What a v2 file looks like

The first non-empty line is the **header**; every line after it is
an event row:

```json
{"_meta":{"kind":"elysper_session_meta","schema_version":2,"id":"<session id>"}}
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
  pre-compaction one. The compaction lifecycle
  (`compaction_start` / `compaction_summary` / `compaction_end`)
  brackets a summariser call; an unmatched `start` is the crash
  marker the repair path detects.
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
   [`crates/elysper-session/src/jsonl.rs`](crates/elysper-session/src/jsonl.rs).
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
