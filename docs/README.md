# Documentation

The docs tree, in reading order. The repository's front door is the root [`README.md`](../README.md); this page is the index for everything under `docs/`.

## Start here

| If you want | Read |
| :--- | :--- |
| The 4-step MVP guide | [`../MINIMAL.md`](../MINIMAL.md) |
| The seven replaceable components | [`../SEAMS.md`](../SEAMS.md) |
| The crate map and the invariants | [`architecture.md`](architecture.md) |
| Runnable examples | [`examples/README.md`](examples/README.md) |
| A full agent conversation walkthrough | [`tutorial/complete-agent.md`](tutorial/complete-agent.md) |
| Your first contribution | [`contributing/first-steps.md`](contributing/first-steps.md) |

## Reference

| Document | What it holds |
| :--- | :--- |
| [`architecture.md`](architecture.md) | Crate map, the hard invariants behind [`../AGENTS.md` §3.7](../AGENTS.md), and the runtime boundary |
| [`plugins.md`](plugins.md) | The plugin and policy catalogue — one module, one feature each |
| [`host-capabilities.md`](host-capabilities.md) | Capability matrix, host by host (`full` / `adapted` / `n/a` / `planned`) |
| [`api-surface.md`](api-surface.md) | The public API surface of the paradigm crates |
| [`api-frozen.md`](api-frozen.md) | The frozen subset whose changes need an explicit decision |
| [`log-format.md`](log-format.md) | The session log's on-disk format |
| [`composition-primitives.md`](composition-primitives.md) | Round-by-round inventory of the composition primitives |
| [`../GOVERNANCE.md`](../GOVERNANCE.md) · [`../SECURITY.md`](../SECURITY.md) · [`../CONTRIBUTING.md`](../CONTRIBUTING.md) · [`../RELEASE.md`](../RELEASE.md) · [`../DEPLOYMENT.md`](../DEPLOYMENT.md) | Process documents — kept at the repository root, not here |

## Working material

`design/` holds the design docs and plans of the current round (one file per topic, dated); `research/` holds external comparisons and reading notes; `superpowers/` is a local, uncommitted scratch area.

Nothing under `design/`, `research/` or `superpowers/` is an API contract — the contracts live in the crate rustdoc, in [`api-surface.md`](api-surface.md) and in [`api-frozen.md`](api-frozen.md).

## Contributing to the docs

- Markdown follows `.markdownlint.json`; the crate `README.md`s and `CONTRIBUTORS.md` are linted in CI (`markdown-lint.yml`).
- `make check-claim-language` scans the current-state docs — including `README.md`, `MINIMAL.md`, `SEAMS.md` and `CHANGELOG.md`'s `[Unreleased]` section — for absolute claims that contradict the code.
- Every doc that names a file, target or method must name one that exists; the gates cannot check prose, so a link check is part of the review.
