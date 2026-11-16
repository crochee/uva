# Uva — AI Agent Framework

<p align="left">
  <img src="uva-web/public/logo.svg" alt="Uva" height="56">
</p>

<p align="left">
  <a href=".github/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/badge/CI-passing-2ea043?style=flat-square&logo=github-actions&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square"></a>
  <a href="https://www.rust-lang.org"><img alt="Rust 1.98+" src="https://img.shields.io/badge/Rust-1.98%2B-orange.svg?style=flat-square&logo=rust&logoColor=white"></a>
  <a href="https://github.com/crochee/uva/releases/latest"><img alt="Releases" src="https://img.shields.io/badge/release-v0.1.0-blueviolet?style=flat-square&logo=github-releases"></a>
  <a href="https://github.com/crochee/uva/discussions"><img alt="Discussions" src="https://img.shields.io/badge/discussions-GitHub-ff7043.svg?style=flat-square&logo=github"></a>
  <a href="SECURITY.md"><img alt="Security policy" src="https://img.shields.io/badge/security-policy-blue.svg?style=flat-square&logo=security&logoColor=white"></a>
  <a href="deny.toml"><img alt="cargo-deny" src="https://img.shields.io/badge/cargo--deny-enforced-brightgreen.svg?style=flat-square&logo=rust"></a>
</p>

**Uva** is a modular AI agent framework for Rust. It implements the ReAct (reason + act) loop, keeps every part of an agent replaceable — provider, tools, context window, steering, session sink, cancellation — and ships a **runtime-neutral public API**: no `tokio` type appears in any library signature, so the agent you assemble runs on the executor you already have.

It is for teams embedding an agent into a product — a service, a CLI, an IDE extension, a device — who want the agent's parts to be swappable and its dependency tree to stay honest.

> **5-minute hello-world** — the command is right below. No API key, no network.
> **New here?** [`MINIMAL.md`](MINIMAL.md) — the 4-step MVP guide (provider → registry → steering → agent).
> **Replacing a piece?** [`SEAMS.md`](SEAMS.md) — every trait a consumer can substitute, what ships for it, and the line that installs yours.
> **Decisions and reasons?** [`docs/architecture.md`](docs/architecture.md) — the crate map and the invariants behind it.

## Status

| | |
|---|---|
| **License** | MIT ([LICENSE](LICENSE)) |
| **Language / MSRV** | Rust, edition 2024, `rust-version = "1.98"` |
| **Version** | `0.1.0` — pre-1.0, semver applies from here ([RELEASE.md](RELEASE.md)) |
| **Distribution** | Git tags + multi-arch container images. **Not yet on crates.io** — publishing starts with the `uva` facade and `uva-core` (see [RELEASE.md](RELEASE.md)); until then depend on the git or path dependency. |
| **CI** | `.github/workflows/rust-quality.yml` — five jobs (`gates`, `tests`, `examples`, `web`, `bench`), **no API key required**: the suite and every example are credential-free by construction. Plus `ci.yml` (MSRV + cross-platform smoke), `docs.yml`, `release.yml`, `security-audit.yml`, `markdown-lint.yml`, `shellcheck.yml`, `desktop.yml`, `stale.yml`. |
| **Tests** | `cargo test -p <crate>` per member (`make test-crates`) — `cargo test --workspace` is deliberately absent, so a failure reports one crate at a time |
| **Static analysis** | `cargo clippy -D warnings --all-targets --all-features` plus ~20 ratchet gates (public API runtime-freedom, published surface, wall-clock discipline, stdout discipline, …) — see `make help` |
| **Security** | [`SECURITY.md`](SECURITY.md) · `cargo-audit` on every push and PR · `cargo deny` for licences, sources and bans |
| **Supply chain** | `Cargo.lock` committed and releases are reproduced from it · Dependabot opens PRs · container images carry OCI labels with build identity (SBOM and in-toto provenance are on the [roadmap](docs/ROADMAP.md)) |
| **Governance** | BDFL today with an explicit review surface, council path documented — [`GOVERNANCE.md`](GOVERNANCE.md) · [`MAINTAINERS.md`](MAINTAINERS.md) · [`.github/CODEOWNERS`](.github/CODEOWNERS) |
| **Contribution** | [`CONTRIBUTING.md`](CONTRIBUTING.md) — DCO sign-off (`git commit -s`), PR title conventions, review routing |
| **Code of conduct** | [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) |

## Quick start

One dependency, one script, one proof line — offline, no API key, no environment:

```bash
git clone https://github.com/crochee/uva && cd uva
cd docs/examples/minimal-consumer && cargo run
# → MVP-OK
```

That runs the whole chain in one file — scripted provider → tool registry → steering → ReAct loop → session writer — and asserts the tool really was called. The index of all 41 runnable examples plus the standalone consumer crates is [`docs/examples/README.md`](docs/examples/README.md).

To depend on Uva from your own crate:

```toml
[dependencies]
uva = { git = "https://github.com/crochee/uva", tag = "v0.1.0" }
# or, from a checkout: uva = { path = "path/to/uva/crates/uva" }
```

The `uva` facade re-exports every piece behind a feature flag and curates a `prelude`, so one dependency is enough to assemble an agent:

```rust,ignore
use uva::prelude::*;   // ReActAgent, ModelProvider, Tool, Steering, …

let agent = ReActAgent::new(provider, registry).with_steering(steering);
```

The crate docs of `uva` are the assembly tutorial (`cargo doc -p uva --open`); the same assembly spelled out brick by brick lives in [`docs/examples/`](docs/examples/README.md).

## Why Uva

- **Runtime neutrality is structural, not aspirational.** The agent takes `Arc<dyn uva_core::CancelToken>`, streams are `futures::Stream`, the typed-event channel is `futures::channel::mpsc`, and detached work goes through the `Spawner` trait. A gate (`make check-public-api-runtime`) fails the build if a library's public API ever names a runtime type. Proof: `cargo run --example runtime_agnostic -p uva-harness` runs a full turn, tool call included, on `std::thread` + `futures::executor`.
- **Every part is replaceable, and the seams say where yours goes.** Seven components — provider, tool, context manager, steering, session sink, cancel token, agent — are traits with default implementations; [`SEAMS.md`](SEAMS.md) names each one.
- **One dependency, not twenty-seven.** The facade is feature-gated; the MVP feature set pulls **no HTTP client at all** (`make check-mvp-deps` asserts `reqwest` / `hyper` / `rustls` / `h2` / `tower` stay out).
- **Plugins, not built-ins.** Tools and policies live in plugin-family crates — one module, one feature each (`read` `write` `shell` `todo` `web` `task` `scheduler` `search` `memo`) — so a consumer assembles the set it needs instead of inheriting one.
- **The dependency tree is a feature, not an accident.** Feature-gated transports, per-crate runtime declarations, `cargo deny` on licences and sources, `cargo audit` on advisories, and a committed lockfile.

## Architecture

```text
┌──────────────────────────────────────────────────────────────────────┐
│                       Consumer Application                           │
│            (HTTP server, CLI, embedded widget, robot …)              │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ depends on ONE crate: `uva`
                               ▼
┌──────────────────────────────────────────────────────────────────────┐
│  uva (facade: feature-gated re-exports + curated `prelude`)         │
└──┬──────────┬──────────┬──────────┬──────────┬──────────┬──────────┬──┘
   ▼          ▼          ▼          ▼          ▼          ▼          ▼
┌──────┐ ┌────────┐ ┌──────┐ ┌────────┐ ┌──────┐ ┌──────────┐ ┌────────┐
│Provider│ │  Tool  │ │Context│ │Steering│ │Session│ │Scheduler │ │  MCP   │
│ OpenAI │ │read,…  │ │window │ │ guard/ │ │lifecycle│ │ cron    │ │ client │
│ Anthro │ │ plugins│ │policy │ │hook/hint│ │ sink   │ │ interval│ │ tools  │
└──────┘ └────────┘ └──────┘ └────────┘ └──────┘ └──────────┘ └────────┘
   └────────┬────────┴─────┬─────┴────┬─────┴─────┬──────┴──────┬─────┘
            └──────────────┴──────────┴───────────┴─────────────┘
                                       ▼
                ┌────────────────────────────────────────┐
                │     uva-harness: ReAct loop            │
                │  (Agent + tool registry + strategy)   │
                └─────────────────────┬──────────────────┘
                                      ▼
                ┌────────────────────────────────────────┐
                │      uva-core: primitives              │
                │  Clock · IdGen · CancelToken · Spawner │
                │  Registry · paths · schemas · errors   │
                └────────────────────────────────────────┘
```

## Workspace

31 workspace members under `crates/`, plus `desktop/` (a Tauri shell, its own workspace) and `uva-web/` (the React front-end).

| Layer | Crates |
|---|---|
| Foundation | `uva-core` · `uva-core-tokio` |
| Paradigms | `uva-provider` · `uva-tool` · `uva-context` · `uva-steering` · `uva-session` · `uva-skill` |
| Approval | `uva-approval` |
| Plugins | `uva-tool-plugins` · `uva-tool-task` · `uva-policy-plugins` · `uva-hook-plugins` |
| Assembly | `uva-kernel` · `uva-runtime` |
| Runtime | `uva-events` · `uva-agent` · `uva-hook` · `uva-harness` |
| Domain crates | `uva-search` · `uva-scheduler` · `uva-eval` · `uva-attachment` · `uva-mcp` · `uva-telemetry` · `uva-test-support` |
| Hosts | `uva-app-server` · `uva-tui` · `uva-gateway` · `uva-mcp-server` |
| Facade | `uva` |

One line per crate, the invariants behind the layering, and where the runtime boundary sits: [`docs/architecture.md`](docs/architecture.md).

## Running

One binary, two modes — `uva-app-server`:

```bash
# the gateway (the human entry point): spawns a core subprocess and
# serves it — WebSocket /rpc, probes, and the web front-end
cargo run -p uva-app-server -- serve --port 8080

# the headless core: JSON-RPC 2.0 on stdin/stdout (Content-Length
# framed), for a TUI / desktop shell / any host that wants a sidecar
cargo run -p uva-app-server -- core --directory .
```

`make dev` boots the server and the front-end together (`:8080` and `:5173`). The terminal projection drives the same gateway: `UVA_APP_SERVER=target/debug/uva-app-server cargo run -p uva-tui -- --directory .`.

The root `Makefile` is the single entry point for development, build, test, format, lint and Docker operations — `make help` lists every target.

## Documentation

| If you want | Start here |
| :--- | :--- |
| The 4-step MVP guide | [`MINIMAL.md`](MINIMAL.md) |
| The seven replaceable components | [`SEAMS.md`](SEAMS.md) |
| The crate map and the invariants | [`docs/architecture.md`](docs/architecture.md) |
| Runnable examples (41 + standalone consumers) | [`docs/examples/README.md`](docs/examples/README.md) |
| A full agent conversation walkthrough | [`docs/tutorial/complete-agent.md`](docs/tutorial/complete-agent.md) |
| The plugin and policy catalogue | [`docs/plugins.md`](docs/plugins.md) |
| The capability matrix (host by host) | [`docs/host-capabilities.md`](docs/host-capabilities.md) |
| The public API surface, frozen | [`docs/api-surface.md`](docs/api-surface.md) · [`docs/api-frozen.md`](docs/api-frozen.md) |
| The whole docs tree | [`docs/README.md`](docs/README.md) |
| What is being worked on now | [`docs/ROADMAP.md`](docs/ROADMAP.md) |
| What changed, and when | [`CHANGELOG.md`](CHANGELOG.md) |
| Deployment, images, environment | [`DEPLOYMENT.md`](DEPLOYMENT.md) |
| Release process | [`RELEASE.md`](RELEASE.md) |

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) is the contribution contract: DCO sign-off (`git commit -s`), branch and PR-title conventions, the review routing in [`.github/CODEOWNERS`](.github/CODEOWNERS), and the local gate sequence (`make fmt`, `make lint-rust`, `make test-crates`, `make ci`).

The rules an agent working in this repository must follow live in [`AGENTS.md`](AGENTS.md).

## License

[MIT](LICENSE) © 2026 uva contributors.
