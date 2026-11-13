# Roadmap

What Uva is working on, and what comes next. This page states intent, not dates — the dated artifacts are the design docs in [`design/`](design/), and the shipped history is [`../CHANGELOG.md`](../CHANGELOG.md).

## Now — the current round

One theme runs through the current round: **every host drives the same gateway**. The browser, the terminal and the desktop shell must reach the same session through the same path, so capability parity is a property of the server rather than of each front-end.

| Workstream | Design doc | Shape |
| :--- | :--- | :--- |
| Gateway crate | [`design/2026-11-09-gateway-crate-plan.md`](design/2026-11-09-gateway-crate-plan.md) | One shared crate owns process supervision (launch, `/readyz`, backoff, reap) and the gateway JSON-RPC client, so the TUI and the desktop shell stop growing their own |
| TUI on the gateway | [`design/2026-11-09-tui-gateway-alignment-plan.md`](design/2026-11-09-tui-gateway-alignment-plan.md) | The terminal projection becomes a conversation-first shell over `/rpc`, the same path the browser uses |
| TUI parity | [`design/2026-11-09-tui-parity-plan.md`](design/2026-11-09-tui-parity-plan.md) | Sessions and the seven domain directories reach the terminal, through one generic list renderer |
| Desktop shell | [`design/2026-11-09-desktop-shell-plan.md`](design/2026-11-09-desktop-shell-plan.md) | A Tauri 2 shell that launches `serve` as a sidecar, waits for readiness, and opens the same SPA — its own workspace, its own CI |
| UI tree and slots | [`design/2026-11-09-ui-tree-slots-plan.md`](design/2026-11-09-ui-tree-slots-plan.md) | A slot contract (`version` / `slot` / `kind` / `props`) shared by the browser and the terminal, with no remote code |
| Profile lifecycle | [`design/2026-11-09-profile-lifecycle-plan.md`](design/2026-11-09-profile-lifecycle-plan.md) | An `enabled` tri-state per profile line, transactional dynamic changes, and built-in MCP |
| SPA information architecture | [`design/2026-11-09-spa-ia-plan.md`](design/2026-11-09-spa-ia-plan.md) | The front-end reorganised around the new prototype |
| Capability matrix | [`design/2026-11-09-capability-matrix-plan.md`](design/2026-11-09-capability-matrix-plan.md) | The matrix in [`host-capabilities.md`](host-capabilities.md) is derived from the profile, so it can only improve |
| Harness V5 closure | [`design/2026-11-07-harness-v5-closure-design.md`](design/2026-11-07-harness-v5-closure-design.md) | The gap ledger against the accepted V4 baseline |
| Verification matrix | [`design/2026-11-10-verification-matrix.md`](design/2026-11-10-verification-matrix.md) | How the ten delivered cuts satisfy the design |

## Next — supply chain and distribution

From the release plan in [`../RELEASE.md`](../RELEASE.md):

- **Image signing** — `cosign sign` per multi-arch manifest list, plus `actions/attest-build-provenance` for in-toto provenance, together forming a SLSA Build L3 identity.
- **Image SBOM** — `cosign attach sbom` pinning a `cargo-cyclonedx` SBOM to the manifest, for enterprise audit and EU CRA.
- **crates.io** — `cargo publish` starting with the `uva` facade and `uva-core`, wired into `release.yml`'s publish job.

## How to propose work

Open an issue (see [`docs/agents/issue-tracker.md`](agents/issue-tracker.md) for where issues live) or a discussion. Changes that touch the public API, the crate boundaries or the host contract go through [`../GOVERNANCE.md`](../GOVERNANCE.md); the day-to-day contract is [`../AGENTS.md`](../AGENTS.md).
