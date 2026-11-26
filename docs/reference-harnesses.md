# The reference harnesses: what was compared, and where it lives

Four harnesses are the reference for the headless control plane:
**DeepSeek Harness** (`dsh`), **pi** (`pi-mono`), **Codex**
(`openai/codex`) and **omp** (`can1357/oh-my-pi`). They are not
dependencies — they are the *comparison*: when this repository answers
an ACP client, the answer must be the answer those harnesses give.

The round that added **omp** is written up separately, as a gap
ledger: [`design/2026-11-26-headless-parity-dsh-omp-design.md`](design/2026-11-26-headless-parity-dsh-omp-design.md)
— the behaviour-by-behaviour table against dsh **and** omp, the twelve
gaps it found (G1–G12), and the three cuts this repository took.

This page records what was read, where it lives in those trees, and
which of it this repository already carries. The evidence was
gathered with `omp-re` (the semantic search over the cloned trees),
so every claim below points at a file and a line.

## The method

```bash
git clone --depth 1 https://github.com/deepseek-ai/deepseek-harness dsh
git clone --depth 1 https://github.com/earendil-works/pi pi
git clone --depth 1 https://github.com/openai/codex codex

# In each tree: ask where a behaviour lives, then read the file.
omp find "the permission request offering allow-once and reject-once choices to the client" .
omp read packages/acp/acp/src/index.ts:1-184
```

The searches were run per behaviour, not per file: the question is
"where does this harness do X?", and the answer is a path plus the
lines that do it.

## The ACP behaviours

The behaviour an ACP client can observe is the same in all three
harnesses, and it is the behaviour this repository's ACP adapter
reproduces.

| behaviour | dsh | pi | codex |
|---|---|---|---|
| the permission request and its answers | `packages/acp/acp/src/index.ts:1-184` | `packages/durable/src/harness/tool.ts:178-488` (the `beforeTool` gate) | `codex-rs/core/src/tools/approvals.rs:392-583` |
| `session/new` and `session/load` | `packages/acp/acp/src/session.ts:1-185` | `packages/coding-agent/src/core/agent-session.ts:722-1081` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json:2725-3450` |
| the turn loop that ends when the model asks for no tools | `packages/core/agent-loop/src/agent.ts:533-690` | `packages/agent/src/agent-loop.ts:1-952` | `codex-rs/core/src/session/turn.rs:163-340` |
| the cancellation that aborts the live turn | `packages/acp/acp/src/index.ts:185-510` | `packages/durable/src/harness/generation.ts:340-638` | `codex-rs/core/src/session/extension_interruption.rs:1-129` |
| the durable transcript | — (the log is the session's own) | `packages/coding-agent/src/core/agent-session.ts:209-395` | `codex-rs/rollout/src/recorder.rs:1-455` |

Two details worth naming, because they are the ones a client notices:

- **The permission question is the client's to answer.** dsh offers
  allow-once and reject-once (`packages/acp/acp/src/index.ts:1-184`);
  pi pauses the tool call at a `beforeTool` gate
  (`packages/durable/docs/suspendable/spec.md:1-163`); codex asks the
  client for a decision and reads the review decision back
  (`codex-rs/core/tests/suite/approvals.rs:3942-4130`). All three
  keep the call *held* until the answer arrives.
- **A refusal is an answer.** Each harness treats "the operator said
  no" as a normal outcome that the model sees, not as an error the
  turn dies on. The adapter's `PermissionOutcome::Rejected` carries
  that.

## The plugin tree

dsh's plugin tree is a *document* (the profile), a *tree* of entries
(each entry a runtime fiber), and a *diff* (what changed when the
document changed). The references:

| behaviour | where it lives |
|---|---|
| the entry model — what a row loads and what it depends on | `packages/client/modules/src/client/manifest.ts:57-114` (`WebBootEntry`: `id`, `inject`, `config`) |
| the group entry — a nested set of entries mounted as one runtime | `vendor/loader/src/config/group.ts:1-127` |
| the mount — resolve the dependencies, start the rows, audit them | `packages/preset/agent-preset-registry/src/mount.ts:134-216` |
| the update — a diff by id, not a remount of everything | `vendor/loader/src/config/group.ts:86-104` |
| the hot reload | `vendor/hmr/src/index.ts:1-422` |
| the self-disable write-back | `packages/boot/plugin-manager/README.md:1-53` |

### The four rules that came out of it

1. **An entry declares itself.** The row carries its own id, its
   configuration and its dependency edges (`inject`). Nothing else
   says what a row needs.
2. **A group is a runtime plugin.** `Group` mounts a nested entry set
   under its own fiber and updates that set when the document changes
   (`vendor/loader/src/config/group.ts:113-127`).
3. **An update is a diff by id.** `EntryGroup.update` compares the old
   and the new configuration by id and creates or removes only what
   changed (`vendor/loader/src/config/group.ts:86-104`).
4. **A row has three states, not two.** The audit separates *failed*
   (never started, or the activation rejected), *pending* (waiting
   for a service the composition does not supply — the row stays
   mounted and activates when the provider arrives) and *usable*
   (`packages/preset/agent-preset-registry/src/mount.ts:134-158`).

### Where this repository carries them

| dsh | this repository |
|---|---|
| the entry model | the profile document's row: `crates/uva-app-server/src/profile/model.rs` (`id`, `name`, `config`, `capabilities`, `group`, `disabled`, `inject`, `intercept`, `isolate`) |
| the group entry | the `group = true` row: it mounts its members as one fiber under its own context |
| the mount | `crates/uva-kernel/src/mount.rs`: `validate` → `topological_order` → mount each row, rolling back on failure |
| the diff | the HMR loop: only the `added` / `removed` / `remounted` rows are unloaded and remounted |
| the three states | `uva_kernel::PluginState`: `Pending` (waiting for an injected service), `Loading`, `Active`, `Failed` — and the kernel's own comment says where the semantics come from: *"Pending is dsh's missing-dependency semantics: the fiber parks and activates when its provider appears."* |
| the isolate realms | `uva_kernel`'s `IsolateSpec` (`Shared(label)` / private) and the `ui.` namespace boundary in `crates/uva-kernel/src/scope.rs` |

So the plugin tree is already ported; the reverse engineering
confirmed the port rather than changing it. What it did *not* change:
the row audit is not exposed as a report (dsh's `auditRows` returns
`failed` and `pending` as two lists), and the mount does not reject a
subtree that leaks services into the root realm (dsh's
`mountPreset` does). Both are dsh's *preset* concepts — a preset is a
shareable subtree mounted into a host's root — and this repository has
no preset concept: its rows mount into the deployment's own tree, and
the `ui.` namespace boundary is the isolation rule it enforces
instead.

## What was compared, and not

The comparison is about *behaviour a client can observe*: the
handshake, the session, the turn, the held call, the cancellation,
the durable transcript. The internals differ on purpose — this
repository's core is its own (one event stream, one session log, one
plugin tree), and the adapters are where the protocols live.
