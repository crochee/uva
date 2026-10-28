# Elysper Deployment Guide

This document describes how to build, run, and deploy the
full Elysper stack — `elysper-server` (Rust), `elysper-web`
(React + Vite), and the supporting Nginx reverse proxy.

## Table of Contents

- [Architecture](#architecture)
- [Local development](#local-development)
- [Production deployment (separate)](#production-deployment-separate)
- [Docker Compose](#docker-compose)
- [Environment variables](#environment-variables)
- [Troubleshooting](#troubleshooting)

## Architecture

The stack uses separated frontend and backend services,
fronted by Nginx.

```
┌──────────────────────────────────────────────────────────┐
│   Browser (http://localhost)                            │
└────────────────────┬─────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────┐
│   Nginx (:80)                                            │
│   - Serves the built React app from `/`                  │
│   - Proxies `/api/*` to elysper-server                   │
│   - Proxies `/v1/*` (Anthropic Messages) to server       │
│   - Proxies `/api/v1/sessions/{id}/follow` (SSE)         │
└────────────────────┬─────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────┐
│   elysper-server (Rust + Axum, :8080 internal)           │
│   - Chat surface at `/api/v1/chat`                       │
│   - Session event stream at `/api/v1/sessions/{id}/follow`│
│   - Anthropic Messages at `/v1/messages`                 │
│   - Management API at `/api/v1/*`                        │
│   - Probes at `/livez`, `/readyz`, `/version`            │
└──────────────────────────────────────────────────────────┘
```

## Local development

For day-to-day development, use the unified `make` target.
This runs both the Rust server (with hot reload) and the Vite
dev server (with HMR) in parallel, with the Vite proxy
forwarding API calls so the browser can use the same origin.

```bash
make dev   # starts both backend (:8080) and frontend (:5173)
```

Open `http://localhost:5173` in your browser.

### One side at a time

```bash
make dev-server   # backend only
make dev-web      # frontend only
```

### Run all tests

```bash
make ci            # fmt --check, clippy -D warnings, dependency invariants
make test-crates   # every crate's suite, one at a time (never --workspace)
make test-sqlite   # the optional SQLite memory tier
make examples      # every example + both standalone consumer crates
make check-web     # frontend types + lint + formatting (no browser)
```

`.github/workflows/rust-quality.yml` runs exactly those targets on every
push and pull request — three jobs (`gates`, `tests`, `examples`), no
API keys required.

### Frontend verification

The frontend has **no automated test suite** by design (R72). The
former Playwright e2e tree took 13 minutes per run and needed
`cargo build` + Vite + Chromium, while its mocks rotted unnoticed. It is
gone, along with `playwright.config.ts`, `.github/workflows/e2e.yml`,
and an ad-hoc browser-script harness under `elysper-web/scripts/e2e/`.

What runs in CI is `make check-web` — `tsc --noEmit`, `eslint`, and
`prettier --check`. Behaviour is verified by driving the running app
with Playwright MCP (`AGENTS.md` §4.2):

```bash
make dev            # backend :8080 + Vite :5173
# then open the app through the MCP browser and exercise the change
```

The MCP browser is supplied by the harness, not by `elysper-web`'s
dependencies — it keeps working with `playwright` uninstalled.

## Production deployment (separate)

The recommended deployment is **split**: Nginx serves the
compiled React app and reverse-proxies `/api/*` and `/v1/*` to
`elysper-server`. This lets the two parts scale and ship
independently.

`/v1/*` is the Anthropic Messages surface (`POST /v1/messages`).
An Anthropic-protocol client points its base URL at the
**deployment root** — scheme and host, no path — and appends
`/v1/messages` itself, exactly as it would for `api.anthropic.com`.

### 1. Build the artifacts

```bash
make build-release
```

This produces:
- `target/release/elysper-server` — the server binary
- `elysper-web/dist/` — the React app's static bundle

### 2. Containerize (recommended)

```bash
make docker         # builds both images
make docker-prod-up # starts the production compose stack
```

The container topology:
- `elysper-server` listens on internal port 8080 (no host port)
- `elysper-web` (Nginx) listens on host port 80 and proxies
  `/api/*` and `/v1/*` to the server over the docker network

### 3. Bare-metal deployment

If you prefer to run on a single host without Docker, see
[nginx.conf](./nginx.conf) for the Nginx server block. Place
the React bundle in the configured `root` directory and run
the server directly:

```bash
./target/release/elysper-server &
nginx -g "daemon off;"
```

### Anthropic-protocol clients

Because the Messages surface is mounted at the server root, the base
URL a client is given is the deployment root — no path component:

```bash
# through Nginx (compose prod, or a bare-metal nginx on :80)
export ANTHROPIC_BASE_URL=http://localhost

# or straight to elysper-server, bypassing the proxy
export ANTHROPIC_BASE_URL=http://localhost:8080

# the SDKs insist on a key; which values the server accepts
# depends on the deployment's `auth` config
export ANTHROPIC_API_KEY=<a key the server accepts>
```

The client posts to `<base URL>/v1/messages`, which Nginx forwards
through its `location /v1/` block to the same upstream as `/api/`.
Auth is the protocol's own — the `x-api-key` header, or the
`Authorization: Bearer` token the web UI sends. The `/v1/` path only
matches that prefix, so it does not steal any client-side route from
the SPA fallback.

## Docker Compose

Two Compose files ship with the repo:

| File                   | Purpose                  |
|------------------------|--------------------------|
| `docker-compose.yml`     | dev (HMR, source mounts) |
| `docker-compose.prod.yml` | prod (separate, Nginx)   |

Common operations:

```bash
make docker-up            # start dev stack
make docker-down          # stop dev stack
make docker-prod-up       # start prod stack
make docker-prod-down     # stop prod stack
make clean-docker         # remove all containers + images
```

## Environment variables

`elysper-server` reads:

| Var                    | Default       | Meaning                          |
|------------------------|---------------|----------------------------------|
| `ELYSPER_HOST`         | `0.0.0.0`     | bind address                     |
| `ELYSPER_PORT`         | `8080`        | listen port                      |
| `RUST_LOG`             | `info`        | log level (`debug`/`info`/...)   |

Precedence for the address: `--host`/`--port` → `ELYSPER_HOST`/
`ELYSPER_PORT` → `host`/`port` in the deployment config → the binary's
own defaults (`127.0.0.1:8080`). The container images set the two
variables to `0.0.0.0:8080`, because a container that binds loopback is
unreachable through a published port.

`elysper-web` (Vite dev server only):

| Var                    | Default               | Meaning                |
|------------------------|-----------------------|------------------------|
| `VITE_API_URL`         | `/api` (proxied)      | REST base URL          |
| `VITE_CHAT_URL`        | `/api/v1/chat`        | Chat surface URL       |
| `VITE_STREAM_URL`      | `/api/v1` (proxied)   | Session follow stream  |

Both services can also be configured through their
respective `config.toml` files in the workspace root.

### The deployment configuration file

`elysper-server --config <path>` names the file that configures the
boot: `providers` (base_url / api_key / models), `agents`
(`max_steps`, `compaction`, `strategy`), `[tools]`, `[plugins]`,
`auth`, `cors`, `operations`, and `mcp_servers`. **The whole file
applies**, not just its provider section.

The parser is chosen by extension: a `.yaml` file is read as YAML,
every other extension — `.toml` and `.yml` included — as JSON. Name
the deployment file `.yaml`; a TOML document does not load.

```bash
# everything from this file: providers, agents, tools, cors, mcp, …
elysper-server --directory /srv/elysper --config /etc/elysper/deploy.yaml
```

Without `--config` the server tries `<directory>/config.toml`, then
`<directory>/.elysper/config.toml`, then its defaults, and the provider
additionally falls back to `<directory>/.agents/config.toml` and
`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`. A named file that is missing or
malformed is logged at `warn` and the next candidate is tried — a config
typo never blocks a boot.

`max_agents` caps registered agents: `POST /api/v1/agents` is refused
once the registry holds that many (the built-in `agent` counts), so a
deployment cannot grow its agent set without bound.

### What `agents.<name>` applies

The agent table is read for five keys:

| key | effect |
|---|---|
| `max_steps` | per-run iteration cap (clamped to `[1, 4096]`) |
| `compaction` | the run's context manager (LLM summarisation policy) |
| `strategy` | the reasoning loop: `react`, `chain-of-thought`, `best-of-n` |
| `allowed_tools` | allow-list: only these tools exist for this agent (empty = no allow-list) |
| `denied_tools` | deny-list: these tools do not exist for this agent (it trims the allow-list) |

`allowed_tools` / `denied_tools` are the agent's **own** scope and are
enforced twice: the model is not told about an excluded tool, *and* a
call to one is refused before dispatch with a tool result naming the
restriction, so `denied_tools = ["shell"]` is a control rather than a
suggestion. The synthetic `task` tool follows the same rule — a
restriction that does not permit `task` turns delegation off. A refusal
surfaces as a `ToolRestriction`-labelled `Guard` warning event and the
run continues, exactly like a steering guard denial.

The remaining keys an older file may carry — `description`, `model`,
`hidden`, `color` — are parsed and **not applied**; the boot logs one
`warn` per configured agent naming them, so a key that does nothing is
never mistaken for one that does. What a run may see and call *beyond*
the restriction comes from the `[tools]` section (groups, `hidden`,
`deferred`) and the registry's privacy flag, where `hidden` refuses
dispatch rather than merely unadvertising.

Two keys that older configs may carry are gone because nothing read
them: `rate_limit` (request limiting belongs at the proxy — see the
Nginx notes above — or in front of the server; an in-process limiter
would be wrong for a multi-replica deployment anyway) and `skills`
(skills are discovered from `<workspace>/.agents/skills/`, which is
also where the API and the `skill` tool read them). Both are ignored by
`serde`, so an existing file keeps working.

### `[plugins]` — runtime composition

Which plugins a deployment runs comes from this section, not from
cargo features:

```yaml
plugins:
  - name: redactor
    kind: remote
    command: ["python", "-m", "redactor"]
    transport: stdio        # accepted: stdio | http
    capabilities: [tools]   # tools | observe | transform | gate
    authority: reduced      # reduced | full
    risk: unknown           # low | high | unknown
```

Defaults are `transport: stdio`, `authority: reduced`, `risk: unknown`,
and no capabilities. The string fields are validated while parsing: an
unknown value — or an unknown key — is a parse error naming the field
and the values it accepts, never a silent fallback. `http` parses but
is not implemented by the host, so selecting it skips that plugin with
a diagnostic.

A plugin is optional capability, not a boot precondition. An entry that
cannot be installed — the process will not start, the handshake fails,
a `full` promotion the plugin cannot serve (it announces no `gate`), a
static install that registers nothing, or a registration the host
refuses when its tools meet the registry — is logged and skipped, and
the boot continues. The composition owns the handles that keep the
installed plugins' registrations alive for the process lifetime, and
exposes the resolved and skipped sets through `Composition::resolved`
and `Composition::skipped`. `GET /api/v1/plugins` is their reader — it
returns both sets (`resolved`: name, kind, authority, risk, claimed
tool names; `skipped`: name and the reason boot recorded), so an entry
that did not install is visible without reading the log. The web UI
shows the same panel on a session page.

The composition installs into **one** deployment host, kept on the
server for the process lifetime. Every agent the server builds — the
boot agent, each per-run chat agent, eval agents and dynamically
registered agents — receives a clone of that host and binds the node's
one tool registry, so a plugin's tools are callable from a normal
conversation, not only from the boot agent. A host clone shares the
registrations (no per-run plugin process), and binding an already-bound
registry is a no-op.

A `static` entry names a factory compiled into the binary (`name` plus
`kind: static`). No in-tree crate implements the harness's `Plugin`
trait today, so the shipped table is **empty** and a `static` entry is
skipped with a diagnostic. A fork that links a plugin crate registers
its factory in the server's boot path, one line per plugin.

## Probe endpoints

```bash
curl http://localhost:8080/livez      # liveness (direct server)
curl http://localhost:8080/readyz     # readiness (direct server)
curl http://localhost/livez           # liveness (through Nginx proxy)
curl http://localhost/readyz          # readiness (through Nginx proxy)
```

All should return `200 OK`. `/readyz` returns `503` with the failing
check names while the server is not ready to serve traffic.

## Troubleshooting

### SSE streaming cuts off mid-response

The Nginx config **must** include `proxy_buffering off;` for
the `/api/v1/sessions/` location (the `/{id}/follow` stream).
Buffered responses will hold frames until the SSE stream closes,
defeating the streaming UX.

The same applies to the `/v1/` block: a `POST /v1/messages`
whose body sets `"stream": true` is answered with SSE, so that
block carries the identical settings.

### `POST /v1/messages` returns HTML instead of a Messages reply

The proxy has no rule for `/v1/`, so the request reached the SPA
fallback and Nginx answered `200` with `index.html`. Add the
`location /v1/` block from [nginx.conf](./nginx.conf) — proxying to
the same upstream as `/api/`, with `proxy_buffering off;` — and
reload Nginx. The Vite dev server hides this: its proxy forwards
`/v1` too, so the bug only shows up behind Nginx.

### CORS errors from the browser

`config.toml` ships with `cors.allowed_origins = ["http://localhost:5173"]`.
Adjust when deploying behind a different host name. New
origins require a server restart.

### Container cannot reach the server

The `elysper-server` container in `docker-compose.prod.yml`
does not expose a host port — Nginx reaches it over the
internal docker network. If you stop the server, Nginx health
checks will fail.
