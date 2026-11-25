# Uva Deployment Guide

This document describes how to build, run, and deploy the
full Uva stack — `uva-app-server` (Rust), `uva-web`
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
│   - Proxies `/api/*` to uva-app-server                   │
│   - Proxies `/v1/*` (Anthropic Messages) to server       │
│   - Proxies `/api/v1/sessions/{id}/follow` (SSE)         │
└────────────────────┬─────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────┐
│   uva-app-server (Rust + Axum, :8080 internal)           │
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
make examples      # every example + the standalone consumer binaries
make check-web     # frontend types + lint + formatting (no browser)
```

`.github/workflows/rust-quality.yml` runs exactly those targets on every
push and pull request — five jobs (`gates`, `tests`, `examples`, `web`,
`bench`), no API keys required.

### Frontend verification

The frontend keeps **no e2e suite** by design (R72; see
`docs/adr/0001-frontend-test-policy.md`). The former Playwright e2e
tree took 13 minutes per run and needed `cargo build` + Vite +
Chromium, while its mocks rotted unnoticed. It is gone, along with
`playwright.config.ts`, `.github/workflows/e2e.yml`, and an ad-hoc
browser-script harness under `uva-web/scripts/e2e/`.

Pure-function unit tests run with vitest (`uva-web/tests/*.test.ts`,
node environment). What runs in CI is `make check-web` —
`tsc --noEmit`, `eslint`, `prettier --check`, and `vitest run`.
Everything the unit tests do not cover (DOM, network, React, real
rendering) is verified by driving the running app with Playwright MCP
(`AGENTS.md` §4.2):

```bash
make dev            # backend :8080 + Vite :5173
# then open the app through the MCP browser and exercise the change
```

The MCP browser is supplied by the harness, not by `uva-web`'s
dependencies — it keeps working with `playwright` uninstalled.

## Production deployment (separate)

The recommended deployment is **split**: Nginx serves the
compiled React app and reverse-proxies the server's HTTP surface
to `uva-app-server`. This lets the two parts scale and ship
independently.

The server's HTTP surface is the gateway's — five paths:

| Path       | What it is                                            |
|------------|-------------------------------------------------------|
| `/rpc`     | the JSON-RPC **websocket** (the SPA's one channel)     |
| `/livez`   | liveness probe                                        |
| `/readyz`  | readiness probe                                       |
| `/version` | the build identity                                    |
| `/metrics` | the Prometheus exposition                             |

Everything else a client asks for is the front-end's own
territory (the SPA's routes resolve to `index.html`).

### 1. Build the artifacts

```bash
make build-release
```

This produces:
- `target/release/uva-app-server` — the server binary
- `uva-web/dist/` — the React app's static bundle

### 2. Containerize (recommended)

```bash
make image-build   # builds the three images (server, web, mcp)
make docker-prod-up # starts the production compose stack
```

The container topology:
- `uva-app-server` listens on internal port 8080 (no host port)
- `uva-web` (Nginx) serves the static bundle and proxies the
  server's surface — `/rpc` (websocket), `/livez`, `/readyz`,
  `/version`, `/metrics` — to the server over the docker network

The web image runs as a non-root user (65532) with a read-only root
filesystem and listens on 8080, so the container needs no
capabilities and no privileged port. Everything nginx writes lands
under `/tmp`; the compose file and `k8s/web.yaml` both mount a
writable `/tmp`.

### 3. Bare-metal deployment

If you prefer to run on a single host without Docker, give Nginx a server
block like this one — it serves the React bundle and proxies the
server's surface. `root` points at the directory holding the built
bundle (`uva-web/dist/`). The image ships the same block, rendered
from [`nginx/default.conf.template`](nginx/default.conf.template).

```nginx
server {
    listen 80;
    root /srv/uva-web;

    # The JSON-RPC websocket. The upgrade headers are the whole
    # point: without them the connection never leaves the HTTP
    # handshake, and the SPA's one channel to the server is dead.
    location = /rpc {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_buffering off;      # the answer streams as it arrives
        proxy_read_timeout 3600s;
    }

    # The probes and the Prometheus exposition.
    location ~ ^/(livez|readyz|version|metrics)$ {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
    }

    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

Then run the server and Nginx directly:

```bash
./target/release/uva-app-server &
nginx -g "daemon off;"
```

### Anthropic-protocol clients

The Anthropic-compatible surface is the `anthropic/messages` **RPC
method** — it answers with the Messages wire shape, and it is served
on the same `/rpc` channel every other method rides. A client that
speaks Anthropic's own HTTP protocol needs an adapter in front of it:
the server does not mount `POST /v1/messages` as an HTTP route.

```bash
# through Nginx (compose prod, or a bare-metal nginx on :80)
export ANTHROPIC_BASE_URL=http://localhost

# or straight to uva-app-server, bypassing the proxy
export ANTHROPIC_BASE_URL=http://localhost:8080

# the SDKs insist on a key; which values the server accepts
# depends on the deployment's `auth` config
export ANTHROPIC_API_KEY=<a key the server accepts>
```

Nginx forwards the `/rpc` channel to the server unchanged, so the
adapter's websocket and the SPA's websocket share one path — and the
`/v1/` prefix claim of the old deployment is gone: a path Nginx does
not proxy would fall through to the SPA fallback and answer `200`
with `index.html`.

## Docker Compose

Two Compose files ship with the repo:

| File                   | Purpose                  |
|------------------------|--------------------------|
| `docker-compose.yml`     | dev (HMR, source mounts) |
| `docker-compose.prod.yml` | prod (separate, Nginx)   |

The prod stack is two services: `app-server` (the server image) and
`web` (the Nginx image that serves the bundle and proxies the
server's surface).

Common operations:

```bash
make docker-up            # start dev stack
make docker-down          # stop dev stack
make docker-prod-up       # start prod stack
make docker-prod-down     # stop prod stack
make clean-docker         # remove all containers + images
```

## Environment variables

`uva-app-server` reads:

| Var                    | Default       | Meaning                          |
|------------------------|---------------|----------------------------------|
| `UVA_HOST`         | `0.0.0.0`     | bind address                     |
| `UVA_PORT`         | `8080`        | listen port                      |
| `RUST_LOG`             | `info`        | log level (`debug`/`info`/...)   |

Precedence for the address: `--host`/`--port` → `UVA_HOST`/
`UVA_PORT` → `host`/`port` in the deployment config → the binary's
own defaults (`127.0.0.1:8080`). The container images set the two
variables to `0.0.0.0:8080`, because a container that binds loopback is
unreachable through a published port.

`uva-web` (Vite dev server only):

| Var                    | Default               | Meaning                |
|------------------------|-----------------------|------------------------|
| `VITE_UVA_VERSION`     | —                     | the build's version    |
| `VITE_UVA_GIT_SHA`     | —                     | the build's commit     |
| `VITE_UVA_BUILD_TIME`  | —                     | the build's timestamp  |

The front-end talks to the server on **its own origin**: the RPC
channel is `/rpc` (a websocket) and the health badge polls
`/readyz`. In development Vite proxies those three paths to the
backend; in production Nginx proxies the same paths.

Both services can also be configured through their
respective `config.toml` files in the workspace root.

### The deployment configuration file

`uva-app-server --config <path>` names the file that configures the
boot: `providers` (base_url / api_key / models), `agents`
(`max_steps`, `compaction`, `strategy`), `[tools]`, `[plugins]`,
`auth`, `cors`, `operations`, and `mcp_servers`. **The whole file
applies**, not just its provider section.

The parser is chosen by extension: a `.yaml` file is read as YAML,
every other extension — `.toml` and `.yml` included — as JSON. Name
the deployment file `.yaml`; a TOML document does not load.

```bash
# everything from this file: providers, agents, tools, cors, mcp, …
uva-app-server --directory /srv/uva --config /etc/uva/deploy.yaml
```

Without `--config` the server tries `<directory>/config.toml`, then
`<directory>/.uva/config.toml`, then its defaults, and the provider
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
| `strategy` | the reasoning loop: `react` |
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

### 第三方工具 — MCP server

动态插件通道（进程插件 / Wasm 组件）已删除。第三方工具与脚本以
**MCP server** 的形式接入：在一份 `mcp.json` 里按 `command` + `args`
声明要拉起的服务器，服务器走 stdio 说话，它发布的工具注册进 agent
用的同一个工具注册表：

```json
{
  "mcpServers": {
    "redactor": { "command": "python", "args": ["-m", "redactor"] }
  }
}
```

文件位置与其它定义同一套双层布局：`<workspace>/.agents/mcp.json`、
`~/.agents/mcp.json`；插件包里的 `<plugin>/mcp.json` 按
`<plugin>/<server>` 命名空间落地。可选键 `env`（环境覆盖）与
`enabled`（关掉这一台）由 `mcp.json` 的解析器校验。

一个服务器是可选能力，不是启动前提：拉不起来、握手失败或列不出工具
的服务器会被记一条警告并跳过，boot 继续。

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
`location /v1/` block from the server block above — proxying to
the same upstream as `/api/`, with `proxy_buffering off;` — and
reload Nginx. The Vite dev server hides this: its proxy forwards
`/v1` too, so the bug only shows up behind Nginx.

### CORS errors from the browser

`config.toml` ships with `cors.allowed_origins = ["http://localhost:5173"]`.
Adjust when deploying behind a different host name. New
origins require a server restart.

### Container cannot reach the server

The `uva-app-server` container in `docker-compose.prod.yml`
does not expose a host port — Nginx reaches it over the
internal docker network. If you stop the server, Nginx health
checks will fail.
