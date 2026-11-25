# Uva full-stack Makefile
#
# Unified entry point for development, build, test, and deployment
# of the uva-app-server (Rust) and uva-web (React/Vite) pair.

SHELL := /bin/bash
.DEFAULT_GOAL := help

# ---- Configuration ----
SERVER_PORT       ?= 8080
WEB_PORT          ?= 5173
SERVER_CRATE      := uva-app-server
WEB_DIR           := uva-web
DOCKER_COMPOSE    := docker compose
COMPOSE_FILE_DEV  := docker-compose.yml
COMPOSE_FILE_PROD := docker-compose.prod.yml

# ---- Help ----

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "; printf "Usage:\n  make \033[36m<target>\033[0m\n"} \
	  /^[a-zA-Z_][a-zA-Z0-9_-]*:.*## / {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# ---- Development ----

.PHONY: dev dev-server dev-web dev-stop

dev: ## Start backend (:8080) and frontend (:5173) in parallel
	@echo "Starting uva-app-server on :$(SERVER_PORT) and uva-web on :$(WEB_PORT)"
	@$(MAKE) -j2 dev-server dev-web

dev-server: ## Start the App Server only (:8080; spawns the headless core)
	cargo run -p $(SERVER_CRATE) -- serve --config config.yaml

dev-web: ## Start frontend only (vite dev server)
	cd $(WEB_DIR) && npm run dev -- --port $(WEB_PORT)

dev-stop: ## Stop background dev processes (best-effort)
	@pkill -f "cargo run -p $(SERVER_CRATE)" || true
	@pkill -f "vite" || true

# ---- Build ----

.PHONY: build build-server build-web build-release

build: build-server build-web ## Build both server and web (debug)

build-server: ## Build backend binary (debug)
	cargo build -p $(SERVER_CRATE)

build-web: ## Build frontend assets
	cd $(WEB_DIR) && npm ci && npm run build

build-release: ## Build release binaries
	cargo build --release -p $(SERVER_CRATE)
	cd $(WEB_DIR) && npm ci && npm run build

# ---- Test ----

.PHONY: test test-rust test-unit

test: test-rust ## Run Rust tests (frontend unit tests run under
              #  `make check-web` via vitest; no frontend e2e suite
              #  by design, see AGENTS.md §4.2)

test-rust: test-crates ## Run every member's tests in turn (never --workspace, per AGENTS.md §3.3)

# The plugin families ship nothing until a feature asks for it, so
# their own suites run with every feature on. (A `case` pattern takes
# `|`-separated alternatives, hence the pipe-joined list.)
FAMILY_CRATES := uva-tool-plugins|uva-policy-plugins|uva-hook-plugins

test-unit: ## Run every member's library unit tests in turn
	@set -e; for c in $(CRATES); do \
	  echo "== $$c"; \
	  case $$c in \
	    $(FAMILY_CRATES)) cargo test -p $$c -q --lib --all-features ;; \
	    *) cargo test -p $$c -q --lib ;; \
	  esac; \
	done

# ---- Code quality ----

.PHONY: fmt fmt-rust fmt-web lint lint-rust lint-web check-web check-web-contract check-web-methods

fmt: fmt-rust fmt-web ## Format all code

fmt-rust: ## Format Rust code
	cargo +nightly fmt --all

fmt-web: ## Format frontend code (whole tree; .prettierignore decides what is skipped)
	cd $(WEB_DIR) && npx prettier --write .

lint: lint-rust lint-web lint-sh ## Lint all code

lint-rust: ## Lint Rust code (clippy)
	cargo clippy --all-targets --all-features --tests --all -- -D warnings

lint-web: ## Lint frontend TypeScript (types + eslint)
	cd $(WEB_DIR) && npx tsc --noEmit && npx eslint .

lint-sh: ## Lint the bash gate scripts (shellcheck)
	@command -v shellcheck >/dev/null 2>&1 || { \
	  echo "shellcheck not installed; install with: apt-get install -y shellcheck"; \
	  echo "(the CI gate uses ludeeus/action-shellcheck, which downloads the binary)"; \
	  exit 1; \
	}
	shellcheck -x -e SC1091,SC2086 -S warning \
	  scripts/check-harness-shape.sh \
	  scripts/check-public-api-runtime.sh \
	  scripts/check-plugin-shape.sh \
	  scripts/check-host-capabilities.sh \
	  scripts/check-pub-surface-strict.sh \
	  scripts/check-harness-docs.sh \
	  scripts/check-single-writer.sh \
	  scripts/check-row-vocabulary.sh \
	  scripts/check-list-recipe.sh \
	  scripts/check-frozen-api.sh \
	  scripts/check-publishable.sh \
	  scripts/check-public-docs.sh \
	  scripts/check-kernel-deps.sh \
	  scripts/check-hook-external.sh \
	  scripts/verify-harness-e2e.sh \
	  scripts/check-stdout.sh \
	  scripts/check-web-contract.sh \
	  scripts/check-web-methods.sh \
	  scripts/check-profile-document.sh

check-web: ## Frontend gates: types, lint, formatting, unit tests (no browser needed)
	cd $(WEB_DIR) && npx tsc --noEmit
	cd $(WEB_DIR) && npx tsc -p tsconfig.tests.json
	cd $(WEB_DIR) && npx eslint .
	cd $(WEB_DIR) && npx prettier --check .
	cd $(WEB_DIR) && npx vitest run

# The wire contract is generated, not written: the Rust emitter prints
# its JSON Schema and the Node side compiles it into
# `uva-web/src/api/types.ts`. `check-web-contract` regenerates into a
# scratch file and diffs; `generate-web-types` is the target a human
# runs after changing a response type.
.PHONY: generate-web-types check-web-contract

generate-web-types: ## Regenerate uva-web/src/api/types.ts from the server's wire types
	cd $(WEB_DIR) && npm run generate:types

check-web-contract: ## Fail if uva-web/src/api/types.ts drifts from the server's wire types
	@bash scripts/check-web-contract.sh

# The domain clients name the RPC methods they call as plain strings,
# and a typo in one of them compiles. `check-web-methods` compares every
# name in a `METHOD` table against the methods the server actually
# serves — the router's registrations and the notification constants.
check-web-methods: ## Fail if a front-end METHOD table names a method the server does not serve
	@bash scripts/check-web-methods.sh --self-test
	@bash scripts/check-web-methods.sh

# ---- Kernel dependency gate ----
# The plugin kernel is the framework boundary, not an agent: its normal
# dependency graph must never include a provider, tool, session,
# harness, app-server, or UI / capability crate. The gate script is the
# single source of truth for the crate list, so the boundary is stated
# once and stays readable.
.PHONY: check-kernel-deps

check-kernel-deps: ## Assert the kernel's dependency graph contains no agent/UI crate
	@bash scripts/check-kernel-deps.sh

# ---- External hook contract gate ----
# The hook contract is only "usable from outside" if a third-party
# plugin's *normal* dependency tree carries no harness. The fixture
# (`docs/examples/hook-only-consumer`) takes `uva-hook` as its only
# normal dependency and the harness as a dev-dependency; the gate runs
# `cargo tree -e normal` inside it and then its dev tests, which drive
# one real tool call (ADR 0004).
.PHONY: check-hook-external

check-hook-external: ## Assert a hook-only consumer's normal tree has no harness
	@bash scripts/check-hook-external.sh

# ---- stdout discipline gate ----
# The headless core's stdout is the JSON-RPC transport. A `println!`
# in the binary reaches the peer as a non-frame and corrupts the
# stream; logs go to stderr. The protocol writer in
# `crates/uva-app-server/src/rpc/stdio.rs` is the only stdout
# caller and is outside the scanned binaries.
.PHONY: check-stdout

check-stdout: ## Assert the core's binaries never write to stdout directly
	@bash scripts/check-stdout.sh

# ---- CLI surface gate ----
# Every binary's command line is clap's, and the protocol modes keep
# stdout for frames only. Two rules:
#
#   1. No hand-rolled argv parsing in library code. The binaries own
#      argv; a library takes the parsed arguments. This is what stops
#      a second `parse_argv` from growing next to the clap
#      definitions (the one that did — `uva-mcp-server`'s — was
#      removed when that crate moved to clap).
#   2. The protocol modes never print help through clap's stdout
#      printer: `core` and the stdio MCP server render `--help` and
#      `--version` on stderr, so a peer never reads them as frames.
#
# The runtime half of the same contract lives in the `cli_surface`
# integration tests of the three binary crates.
.PHONY: check-cli-surface

check-cli-surface: ## Assert the CLIs are clap's and the protocol modes keep stdout for frames
	@hits=$$(find crates/*/src -name '*.rs' ! -name 'main.rs' ! -name 'tests.rs' ! -path '*/bin/*' -print0 \
	  | xargs -0 -r grep -nE '\benv::args(_os)?\b' 2>/dev/null \
	  | grep -vE ':[0-9]+:[[:space:]]*(//|/\*|\*)' || true); \
	if [ -n "$$hits" ]; then \
	  echo "FAIL: library code must not read argv by hand — the CLI is clap's:"; \
	  printf '%s\n' "$$hits" | sed 's/^/  /'; \
	  echo "Move the parsing into the crate's clap module and pass the parsed"; \
	  echo "arguments in (crates/*/src/cli.rs)."; \
	  exit 1; \
	fi
	@hits=$$(grep -rnE '\bprint_(long_)?help\b' crates/uva-app-server/src crates/uva-mcp-server/src 2>/dev/null \
	  | grep -vE ':[0-9]+:[[:space:]]*(//|/\*|\*)' || true); \
	if [ -n "$$hits" ]; then \
	  echo "FAIL: the protocol modes must render help on stderr, never stdout:"; \
	  printf '%s\n' "$$hits" | sed 's/^/  /'; \
	  echo "Render the help yourself and write it to stderr."; \
	  exit 1; \
	fi
	@echo "OK: every CLI is clap's, and the protocol modes keep stdout for frames"

# ---- MVP dependency gate ----
# The smallest agent (MINIMAL.md) must not drag the deployment stack
# along: the seven-feature subset of the `uva` facade has to build
# with no HTTP client, no exporter, no HTTP framework, no database, and
# no OTLP transport. This target fails if any of them reappear.
#
# `cargo tree` prints a tree, not a flat list; `--prefix none` flattens
# it so each line starts with the crate name, and each forbidden name is
# a PREFIX match so `hyper-rustls` / `rustls-pki-types` /
# `tower-http` are caught by `hyper` / `rustls` / `tower`.
MVP_FEATURES := core,provider,context,tool,session,steering,harness
MVP_FORBIDDEN := opentelemetry|tonic|axum|rusqlite|sqlx|reqwest|hyper|rustls|h2|tower|webpki
# The shipped tool plugins: one family crate with one offline feature
# per tool, plus the `task` tool's own crate (a single tool, no
# features). The isolation property is per *feature*, not per crate:
# the offline set pulls no HTTP stack, and only `web` does. Each entry
# is `crate:features` (colon, not `|`: the list is word-split
# inside a shell `for`, where an unquoted pipe would be an operator);
# an empty feature list means the plain build.
FAMILY_TOOL_CRATES := uva-tool-plugins:read,write,shell,todo,scheduler,search,memo uva-tool-task:
# The policy plugins (R-OPT-HPC round: Tier 1 performance + concurrency,
# Tier 2 reasoning + quality) live in one family crate too, all in the
# offline tier (no HTTP/OTel/DB — proved by the `mvp-forbidden` check
# below).
FAMILY_POLICY_CRATES := uva-policy-plugins
# The hook implementations are one family crate as well; every feature
# is offline (the contract and the tool trait carry no transport).
FAMILY_HOOK_CRATES := uva-hook-plugins
# The two crates whose interesting configuration the plain loops never
# check each get their own section: `uva-search` under
# `--features provider`, `uva-tool-scheduler` under `--features
# cron`. `doc-check` builds only the default features, so the cron
# section also runs a rustdoc pass — otherwise the cron-gated surface
# is documented nowhere in `make ci`.

.PHONY: check-mvp-deps

check-mvp-deps: ## Assert the MVP subset: no HTTP/server/OTel/DB deps, empty prelude compiles
	@cargo check -q -p uva --no-default-features 2>/dev/null \
	  && echo "OK: uva --no-default-features compiles (the prelude is empty, not broken)"
	@if cargo tree -p uva --no-default-features --features $(MVP_FEATURES) -e normal --prefix none 2>/dev/null \
	  | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	  echo "FAIL: the MVP feature subset pulled a forbidden dependency:"; \
	  cargo tree -p uva --no-default-features --features $(MVP_FEATURES) -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	  exit 1; \
	fi
	@echo "OK: $(MVP_FEATURES) pulls no ($(MVP_FORBIDDEN))"
	@for pair in $(FAMILY_TOOL_CRATES); do \
	  c=$${pair%%:*}; f=$${pair#*:}; \
	  if [ -n "$$f" ]; then feat="--features $$f"; else feat=""; fi; \
	  if cargo tree -p $$c $$feat -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: $$c (the offline tool plugins) pulled a forbidden dependency:"; \
	    cargo tree -p $$c $$feat -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: the tool family ($$(echo '$(FAMILY_TOOL_CRATES)' | tr ' ' ',')) pulls no ($(MVP_FORBIDDEN))"
	@# The reverse assertion: the web feature really is the switch that
	@# opens the HTTP stack. Without it the gate above would pass on a
	@# crate that never had one.
	@cargo tree -p uva-tool-plugins --features web -e normal --prefix none 2>/dev/null \
	  | cut -d' ' -f1 | sort -u | grep -Eq '^reqwest$$' \
	  && echo "OK: the tool family's web feature is the only one that opens reqwest" \
	  || { echo "FAIL: the tool family's web feature did not open reqwest"; exit 1; }
	@for c in $(FAMILY_POLICY_CRATES); do \
	  if cargo tree -p $$c --all-features -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: $$c (the policy plugins) pulled a forbidden dependency:"; \
	    cargo tree -p $$c --all-features -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: $(FAMILY_POLICY_CRATES) (all features) pull no ($(MVP_FORBIDDEN))"
	@for c in $(FAMILY_HOOK_CRATES); do \
	  if cargo tree -p $$c --all-features -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: $$c (the hook plugins) pulled a forbidden dependency:"; \
	    cargo tree -p $$c --all-features -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: $(FAMILY_HOOK_CRATES) (all features) pull no ($(MVP_FORBIDDEN))"
	@for f in "" "--features simd"; do \
	  if cargo tree -p uva-search $$f -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: uva-search $$f pulled a forbidden dependency:"; \
	    cargo tree -p uva-search $$f -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: uva-search (default + --features simd) stays offline"
	@for f in "" "--features provider" "--features hnsw"; do \
	  if cargo tree -p uva-search $$f -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: uva-search $$f pulled a forbidden dependency:"; \
	    cargo tree -p uva-search $$f -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: uva-search (default + --features provider + --features hnsw) pulls no ($(MVP_FORBIDDEN))"
	@for f in "--features scheduler" "--features scheduler,cron"; do \
	  if cargo tree -p uva-tool-plugins $$f -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: uva-tool-plugins $$f pulled a forbidden dependency:"; \
	    cargo tree -p uva-tool-plugins $$f -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: uva-tool-plugins (scheduler + scheduler,cron) pulls no ($(MVP_FORBIDDEN))"
	@for f in ""; do \
	  if cargo tree -p uva-harness $$f -e normal --prefix none 2>/dev/null \
	    | cut -d' ' -f1 | sort -u | grep -Eq '^($(MVP_FORBIDDEN))'; then \
	    echo "FAIL: uva-harness $$f pulled a forbidden dependency:"; \
	    cargo tree -p uva-harness $$f -e normal --prefix none 2>/dev/null \
	      | cut -d' ' -f1 | sort -u | grep -E '^($(MVP_FORBIDDEN))'; \
	    exit 1; \
	  fi; \
	done
	@echo "OK: uva-harness (default) pulls no ($(MVP_FORBIDDEN))"
	@cargo check -q -p uva-harness --no-default-features 2>/dev/null \
	  && echo "OK: uva-harness --no-default-features compiles"
	@for p in uva-policy-plugins uva-hook-plugins; do \
	  cargo check -q -p $$p --all-features 2>/dev/null \
	    || { echo "FAIL: $$p does not build standalone"; exit 1; }; \
	done
	@echo "OK: the policy and hook plugin families build standalone"
	@RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p uva-scheduler \
	    --features cron 2>/dev/null \
	  && echo "OK: uva-scheduler --features cron rustdoc is warning-free"

# ---- Runtime-free gate ----
# These pieces promise their consumer no async runtime: their own code
# is `futures` + std and the runtime lives in the caller (the scheduler
# takes `tick(now)`, the eval runner never spawns). A `tokio` crate
# reappearing in one of their *lib* trees means the promise broke —
# most likely because someone went back to inheriting the workspace's
# tokio entry instead of declaring features on it.
RUNTIME_FREE := uva-core uva-scheduler uva-eval \
	uva-search uva-session uva-events uva-agent uva-hook uva-hook-plugins \
	uva-tool-task \
	uva-harness \
	uva-context uva-steering \
	uva-kernel uva-approval

.PHONY: check-no-runtime

# `uva-tool` keeps its OWN [dependencies] runtime-free (dispatch
# runs on the caller's poll; tokio lives in [dev-dependencies] only).
# It cannot join RUNTIME_FREE: the wire types it consumes come from
# `uva-provider`, whose default features enable the two HTTP
# adapters (and with them tokio), so the crate gets the same depth-1
# "declares no runtime" assertion `check-harness-features` applies to
# `uva-harness`.
check-no-runtime: ## Assert the runtime-free crates pull no tokio in a lib build
	@for c in $(RUNTIME_FREE); do \
	  n=$$(cargo tree -p $$c -e normal --prefix none 2>/dev/null | cut -d' ' -f1 | sort -u | grep -c '^tokio' || true); \
	  if [ "$$n" != "0" ]; then \
	    echo "FAIL: $$c (lib) pulls tokio ($$n crates)"; exit 1; \
	  fi; \
	done
	@n=$$(cargo tree -p uva-telemetry --no-default-features -e normal --prefix none 2>/dev/null | cut -d' ' -f1 | sort -u | grep -c '^tokio' || true); \
	  if [ "$$n" != "0" ]; then \
	    echo "FAIL: uva-telemetry --no-default-features pulls tokio ($$n crates)"; exit 1; \
	  fi
	@n=$$(cargo tree -p uva-tool -e normal --depth 1 --prefix none 2>/dev/null | cut -d' ' -f1 | sort -u | grep -cE '^(tokio|tokio-stream|tokio-util|async-std|smol|futures-executor)' || true); \
	  if [ "$$n" != "0" ]; then \
	    echo "FAIL: uva-tool declares a runtime dependency ($$n crates)"; exit 1; \
	  fi
	@echo "OK: $(RUNTIME_FREE), uva-telemetry --no-default-features, and uva-tool's own deps are tokio-free"


.PHONY: check-harness-features

check-harness-features: ## Assert uva-harness declares zero features and no direct runtime dependency
	@if grep -q '^\[features\]' crates/uva-harness/Cargo.toml; then \
	  echo "FAIL: uva-harness must not declare a [features] table"; exit 1; fi
	@if cargo tree -p uva-harness -e normal --depth 1 --prefix none 2>/dev/null \
	  | cut -d' ' -f1 | sort -u | grep -Eq '^(tokio|tokio-util|async-std|smol|futures-executor)'; then \
	  echo "FAIL: uva-harness declares a runtime dependency"; exit 1; fi
	@echo "OK: uva-harness has no features and no direct runtime dependency"

# ---- Claim-language gate ----
# Forbidden phrasings that contradict facts the code enforces (the
# facade's MVP subset still compiles the two synthetic contracts and
# the registry's `ToolEntry::dynamic` passthrough, so "paradigm only"
# and "zero tool implementations" are stale; the paradigm crate ships
# no default set, so "default registry" / "the built-ins" are stale
# too). Caught in *current-state* docs and the facade's doc-comments
# only: frozen historical reports (`docs/optimization-report-R*.md`,
# `docs/traitclaw-gap-analysis.md`), released CHANGELOG entries, and
# `docs/superpowers/` work-in-progress notes are exempt — they are
# records of past rounds, by the precedent set in
# `optimization-report-R46`.
CLAIM_FORBIDDEN := \bparadigm only\b|\bzero tool implementations\b|\bships no (implementations|tools?)\b|\bdefault (tool set|tool registry|registry of tools|built-?in)\b|\bthe built-?ins\b|\bdefault registry\b|\bdefault registry ships\b|\bfour offline builtins\b
# Files & paths scanned. The prelude list is intentionally narrow: a
# wider scan catches too much in the published historical record.
# AGENTS.md is exempt because §3.7 is the gate's own source of truth
# (it names the forbidden patterns); a self-trip there would be
# noise. Anything that matters is caught in the other files.
CLAIM_DOCS := README.md MINIMAL.md SEAMS.md CHANGELOG.md

CLAIM_CRATE_DOCS := $(wildcard crates/*/README.md)
# Doc comments / inline comments in the facade and the paradigm crate
# are scanned; tests are exempt (they pin a contract that the runtime
# enforces, and the wording there is necessarily literal).
CLAIM_RUST_DOCS := crates/uva/src/lib.rs crates/uva/src/tool.rs crates/uva-tool/src/lib.rs

.PHONY: check-claim-language

check-claim-language: ## Fail on absolute "paradigm only / default set" claims in current-state docs
	@set -e; \
	hits=$$( \
	  { for f in $(CLAIM_DOCS); do \
	      awk 'BEGIN{keep=1} /^## \[0\.2\.0\]/ && FILENAME ~ /CHANGELOG\.md/ {keep=0} keep { print FILENAME ":" NR ":" $$0 }' "$$f"; \
	    done; \
	    for f in $(CLAIM_CRATE_DOCS); do \
	      awk '{ print FILENAME ":" NR ":" $$0 }' "$$f"; \
	    done; \
	    for f in $(CLAIM_RUST_DOCS); do \
	      awk '{ print FILENAME ":" NR ":" $$0 }' "$$f"; \
	    done; \
	  } | grep -nE "$(CLAIM_FORBIDDEN)" || true \
	); \
	if [ -n "$$hits" ]; then \
	  echo "FAIL: absolute claim-language in current-state docs:"; \
	  echo "$$hits"; \
	  echo "Rephrase — the paradigm crate ships the paradigm + the two synthetic"; \
	  echo "contracts + the registry passthrough; no default tool set ships. See"; \
	  echo "AGENTS.md §3.7. Frozen reports (optimization-report-R*, released"; \
	  echo "CHANGELOG entries, traitclaw-gap-analysis.md, superpowers/) are exempt."; \
	  exit 1; \
	fi; \
	echo "OK: no absolute claim-language in current-state docs"
# The end-to-end proof that the assembled harness really works: it
# needs a live model endpoint and a provider key, so it is a
# development-time tool rather than a CI gate (CI has no key).
verify-harness-e2e: ## Prove the mounted harness answers with a real tool call (needs a provider key)
	@bash scripts/verify-harness-e2e.sh

# ---- CI entry points ----
# `.github/workflows/rust-quality.yml` calls these instead of spelling
# the commands out in YAML, so a contributor can reproduce any CI step
# by running one `make` target locally. `cargo test --workspace` is
# deliberately absent: AGENTS.md §3.3 requires per-module batches.

# Every workspace member, derived from Cargo's own manifest so a new
# crate needs no Makefile edit.
#
# The list comes from `cargo metadata`, not from `wildcard crates/*`:
# git does not track empty directories, so a removed crate's directory
# can survive on disk and make this loop test a package that does not
# exist ("package ID specification `X` did not match any packages").
# Cargo's manifest is the single source of truth for membership.
CRATES := $(shell cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c 'import json,sys; print(" ".join(sorted(p["name"] for p in json.load(sys.stdin)["packages"])))')

.PHONY: fmt-check ci test-crates test-sqlite bench examples verify-harness-e2e

fmt-check: ## Verify Rust formatting (no writes)
	cargo +nightly fmt --all --check

ci: fmt-check lint-rust doc-check test-guides check-mvp-deps check-kernel-deps check-hook-external check-stdout check-cli-surface check-no-runtime check-harness-features check-public-api-runtime check-pub-surface check-pub-surface-strict check-frozen-api check-publishable check-public-docs check-claim-language check-clock check-harness-shape check-harness-docs check-single-writer check-row-vocabulary check-list-recipe check-plugin-shape check-host-capabilities check-profile-document check-test-layout check-deny check-web check-web-contract check-web-methods test-crates ## The fast gate: fmt, clippy, rustdoc, compiled guides, dependency + layout + shape + comment-budget + public-docs + stdout-discipline + CLI-surface + clock + single-writer + publishability + license/ban invariants + frontend type/lint/contract/method gates + every …

test-crates: ## Run every member's test suite in turn (never --workspace)
	@set -e; for c in $(CRATES); do \
	  echo "== $$c"; \
	  case $$c in \
	    $(FAMILY_CRATES)) cargo test -p $$c -q --all-features ;; \
	    *) cargo test -p $$c -q ;; \
	  esac; \
	done

test-sqlite: ## Run the optional SQLite memory tier's tests
	cargo test -p uva-context -q --features sqlite

check-public-api-runtime: ## Fail if a library crate's public API names a runtime type
	@bash scripts/check-public-api-runtime.sh

doc-check: ## Fail on any rustdoc warning (a broken intra-doc link points at nothing)
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# ---- The compiled guides ----
# `MINIMAL.md`'s `rust` fences are doctests (via `include_str!` in the
# facade), and CI runs them twice: default features, then the seven-feature
# MVP subset the guide promises is sufficient. `doc-check` does NOT cover
# them — it *generates* docs, it does not *compile* examples — so without
# this target a broken recipe in the guide reports green locally and only
# fails on CI. Run it after editing `MINIMAL.md` (or the facade docs).
.PHONY: test-guides

test-guides: ## Compile MINIMAL.md's fences under both feature sets
	cargo test -p uva --doc
	cargo test -p uva --doc --no-default-features \
	    --features core,provider,context,tool,session,steering,harness

# ---- Public-surface gate ----
# AGENTS.md §3.7: a crate's lib.rs decides its public surface by
# naming every re-export explicitly. A `pub use <module>::*;` makes
# any future `pub` item inside that module part of the crate's
# contract without anyone choosing it. The facade is the one
# exception — `crates/uva/src/*.rs` propagate their crate's
# curated surface wholesale, which is safe precisely because the
# crates below are curated.
.PHONY: pub-surface-audit check-pub-surface

pub-surface-audit: ## List glob re-exports outside the facade
	@grep -rnE '^[[:space:]]*pub use [a-z_]+::\*;' crates/*/src --include='*.rs' 2>/dev/null \
	  | grep -v '^crates/uva/src/' || echo "  none"

check-pub-surface: ## Fail on a glob re-export inside a uva-* crate
	@hits=$$(grep -rnE '^[[:space:]]*pub use [a-z_]+::\*;' crates/*/src --include='*.rs' 2>/dev/null \
	  | grep -v '^crates/uva/src/' || true); \
	if [ -n "$$hits" ]; then \
	  echo "FAIL: glob re-export in a library crate's src (AGENTS.md §3.7):"; \
	  echo "$$hits"; \
	  echo "List the names explicitly; the crate's lib.rs is the single place that"; \
	  echo "decides what is public. The facade (crates/uva/src/) is exempt — it"; \
	  echo "propagates each crate's already-curated surface. Run 'make pub-surface-audit'."; \
	  exit 1; \
	fi; \
	echo "OK: no glob re-export inside a uva-* crate (facade exempt)"

# ---- Strict paradigm-crate surface (R-G --strict-paradigm) ----
# The glob `pub use <module>::*;` check above catches the worst
# failure mode (every new `pub` in the named module leaks out
# without anyone choosing it), but a crate can still drift toward
# the same problem by *counting* — 50 explicit re-exports are not
# meaningfully safer than one glob. AGENTS.md §3.7 says each
# `uva-*` crate's lib.rs is the *single* place that decides
# what is public, so a tighter ratchet is appropriate: each
# non-facade lib crate gets a per-crate line-count budget, encoded
# in `scripts/paradigm-surface/baselines/<crate>=<N>`, and any
# growth fails the gate. Intentional additions go in the same
# commit as a baseline bump — mirrors the `check-plugin-shape`
# precedent.
#
# Facade (`crates/uva/`) is exempt by the same reasoning as the
# glob gate: `pub use uva_xxx::*;` is the documented propagation
# pattern, and the surface budget lives on the underlying crates.
# `uva-app-server` and `uva-mcp-server` are app / binary
# crates; their lib.rs is a thin wiring entry, not a curated
# surface, so they're exempt too.
.PHONY: check-pub-surface-strict paradigm-surface-audit

paradigm-surface-audit: ## Print current per-crate pub-use counts (largest first)
	@bash scripts/check-pub-surface-strict.sh --print

check-pub-surface-strict: ## Fail when a paradigm crate's pub-use count grows past its per-crate baseline
	@bash scripts/check-pub-surface-strict.sh

# ---- Frozen curated surface (public API freeze) ----
# The two gates above police the *shape* of the curated surface (no
# glob re-exports, no growth in the number of `pub use` statements).
# This one pins its *identity*: every name a frozen crate re-exports
# from its lib.rs is listed in `scripts/frozen-api/baseline`, and a
# name appearing or disappearing without the baseline changing in the
# same commit fails the gate. The human-readable side of the same
# promise is `docs/api-frozen.md`; both change together, in one
# reviewed commit.
.PHONY: check-frozen-api frozen-api-audit

frozen-api-audit: ## Print the current curated surface in baseline format
	@bash scripts/check-frozen-api.sh --print

check-frozen-api: ## Fail when a frozen crate's curated surface moved without docs/api-frozen.md and the baseline changing
	@bash scripts/check-frozen-api.sh --self-test
	@bash scripts/check-frozen-api.sh

# ---- Publishability gate ----
# `cargo package` 是 "可安装" 的唯一硬证据: 它检查每个 crate 的
# manifest 是否带版本号、README 是否在包内、依赖是否可被 registry
# 解析. `cargo build` / `cargo test` 看不到这些 —— path 依赖缺版本
# 在 workspace 内完全正常, 只有在打包那一刻才暴露.
.PHONY: check-publishable
check-publishable: ## Fail when a publishable crate cannot be packaged
	@bash scripts/check-publishable.sh

# ---- License / source / bans gate ----
# `cargo-deny` 是 R161 三件套里的公告 + 许可证检查; 与 `cargo-audit`
# (rustsec/advisory-db) 冗余防御. 在 `make ci` 链路上跑一次, 等价于
# `.github/workflows/security-audit.yml` 的 `deny` job.
#
# 故意**不**要求卡 PR 合规 —— deny.toml 的策略收紧时旧 PR 会立刻
# 全红, 收紧的策略应单独发 PR; 这条门禁只看 master 后的合规性.
.PHONY: check-deny
check-deny: ## Run cargo deny (license / bans / sources)
	# Advisories 是 `cargo-audit` 的工作 — 那是 `.github/workflows/security-audit.yml`
	# 与每 2 个月 cron 的责任; 在 `make ci` 上跑 cargo deny advisories 会让 transitive
	# 公告 (h2 / rustls 等的旧版) 一次性 fail, 但升级 lock 不在本 R 范围内.
	# 收紧 `advisories = "deny"` 应在专门的 dep-upgrade PR 里打开.
	@command -v cargo-deny >/dev/null 2>&1 || { \
	  echo "cargo-deny not installed; install with: cargo install --locked cargo-deny"; \
	  exit 1; \
	}
	cargo deny check licenses bans sources

bench: ## Run the hot-path benchmarks (harness + kernel + app-server)
	cargo bench -p uva-harness --bench hot_paths
	cargo bench -p uva-kernel --bench hot_paths
	cargo bench -p uva-app-server --bench framing

bench-check: ## Enforce the hot-path ratios (machine-independent; runs in CI)
	cargo bench -p uva-harness --bench hot_paths -- --check
	cargo bench -p uva-kernel --bench hot_paths -- --check
	cargo bench -p uva-app-server --bench framing -- --check

# Note: `otel_collector_mock` is a long-running server that only
# exits after a companion client POSTs to it; there is no client
# here, so the inner loop skips it (it gets its 30-second smoke
# in the telemetry workflow instead). The other conditionals add
# the per-example `--features` flag a few examples require.
#
# The three `docs/examples/` consumers run last and each prints a
# proof line. The harness-only one also proves its *dependency tree*:
# `cargo tree` there must contain zero `tokio`. A non-zero count means
# the harness closure grew a runtime crate — fix the library, never add
# tokio to the fixture to silence it.
examples: ## Run every example plus the standalone consumer binaries (each prints a proof line)
	@set -e; for d in $(wildcard crates/*/examples); do \
	  ls $$d/*.rs >/dev/null 2>&1 || continue; \
	  pkg=$$(echo $$d | cut -d/ -f2); \
	  for f in $$d/*.rs; do \
	    name=$$(basename $$f .rs); \
	    extra=""; \
	    [ "$$name" = "sqlite_memory" ] && extra="--features sqlite" || true; \
	    [ "$$name" = "hnsw_bench" ] && extra="--features hnsw" || true; \
	    [ "$$name" = "provider_embedder" ] && extra="--features provider" || true; \
	    [ "$$name" = "provider_pool_bench" ] && extra="--features hpc-pool" || true; \
	    [ "$$name" = "simd_bench" ] && extra="--features simd" || true; \
	    [ "$$name" = "otel_collector_mock" ] && continue || true; \
	    [ "$$name" = "read_ranges" ] && extra="--features read" || true; \
	    [ "$$name" = "write_modes" ] && extra="--features write" || true; \
	    [ "$$name" = "todo_list" ] && extra="--features todo" || true; \
	    [ "$$name" = "sandbox_policy" ] && extra="--features shell" || true; \
	    [ "$$name" = "allowlist" ] && extra="--features web" || true; \
	    [ "$$name" = "schedule_jobs" ] && extra="--features scheduler" || true; \
	    [ "$$name" = "search_tool_demo" ] && extra="--features search" || true; \
	    [ "$$name" = "tool_memo_demo" ] && extra="--features memo" || true; \
	    [ "$$name" = "semantic_cache_demo" ] && extra="--features cache" || true; \
	    [ "$$name" = "resilience_demo" ] && extra="--features resilience" || true; \
	    [ "$$name" = "budget_demo" ] && extra="--features budget" || true; \
	    [ "$$name" = "reflection_demo" ] && extra="--features reflection" || true; \
	    echo "== $$pkg/$$name"; \
	    cargo run -q -p $$pkg --example $$name $$extra > /dev/null; \
	  done; \
	done
	@echo "== docs/examples/plugin-author (a third-party tool plugin)"
	@cd docs/examples/plugin-author && cargo run -q
	@echo "== docs/examples/external-consumer"
	@cd docs/examples/external-consumer && cargo run -q
	@echo "== docs/examples/minimal-consumer"
	@cd docs/examples/minimal-consumer && cargo run -q
	@echo "== docs/examples/harness-only-consumer (cargo tree | grep -c tokio)"
	@cd docs/examples/harness-only-consumer \
	  && n=$$(cargo tree -q | grep -c tokio || true) \
	  && echo "tokio in tree: $$n" \
	  && [ "$$n" = "0" ] || { \
	    echo "FAIL: the harness closure pulls tokio into a consumer that"; \
	    echo "      depends only on uva-harness (AGENTS.md: libraries are"; \
	    echo "      runtime-neutral). Remove it from the library."; \
	    exit 1; }
	@echo "== docs/examples/harness-only-consumer"
	@cd docs/examples/harness-only-consumer && cargo run -q

# ---- Wall-clock discipline gate ----
# AGENTS.md §3.8: library code reads the wall clock through
# `uva_core::Clock`, never `chrono::Utc::now()`. Only *code* counts
# here — a line whose first characters are `//` is a comment (the docs
# legitimately mention the call they forbid) — and `uva-app-server`
# is the **app** crate (AGENTS.md §3.1: libs use thiserror, apps use
# anyhow) so its 4 production reads are exempt. After excluding
# comments, `#[cfg(test)]` blocks, and the app crate, the count is 0;
# lower the baseline whenever the audit shrinks; it fails the moment a
# new production call site appears in a library crate.
CLOCK_BASELINE := 0

.PHONY: clock-audit check-clock

clock-audit: ## List every chrono::Utc::now() call left in library code
	# Same scope as `check-clock`: lib crates only (app crate
	# uva-app-server is exempt); inside libs we still exclude
	# `#[cfg(test)]` blocks. Run this to see which lines contributed
	# to the count.
	@for f in $$(find crates -name '*.rs' -path '*/src/*' \
	              ! -name 'clock.rs' \
	              ! -name 'tests.rs' \
	              ! -path '*/tests/*' \
	              ! -path '*/uva-app-server/src/*'); do \
	  awk -v F="$$f" '\
	    /^[[:space:]]*#\[cfg[.(]test[.)]]/{in_test=1; next} \
	    /^}[[:space:]]*$$/ && in_test{in_test=0; next} \
	    /Utc::now/{ line=$$0; sub(/^[ \t]+/,"",line); \
	                 if (substr(line,1,2)!="//" && !in_test) \
	                   printf "%s:%d: %s\n", F, FNR, line }' "$$f"; \
	done

check-clock: ## Fail when chrono::Utc::now() calls in library code grow
	# Scope: lib crates only — `uva-app-server` is the **app** crate
	# (AGENTS.md §3.1: libs use thiserror, apps use anyhow), and apps
	# legitimately read wall-clock time directly. Inside libs we still
	# exclude `#[cfg(test)]` blocks (test code uses real time, not the
	# injected `FixedClock`).
	@n=$$(for f in $$(find crates -name '*.rs' -path '*/src/*' \
	                   ! -name 'clock.rs' \
	                   ! -name 'tests.rs' \
	                   ! -path '*/tests/*' \
	                   ! -path '*/uva-app-server/src/*'); do \
	  awk '/^[[:space:]]*#\[cfg[.(]test[.)]]/{in_test=1; next} \
	       /^}[[:space:]]*$$/ && in_test{in_test=0; next} \
	       /Utc::now/{ stripped=$$0; sub(/^[ \t]+/, "", stripped); \
	                    if (substr(stripped,1,2)!="//" && !in_test) c++ } \
	       END{print c+0}' "$$f"; \
	done | awk '{s+=$$1} END{print s+0}'); \
	if [ "$$n" -gt $(CLOCK_BASELINE) ]; then \
	  echo "FAIL: $$n chrono::Utc::now() call(s) in lib production code (baseline $(CLOCK_BASELINE); app crate uva-app-server is exempt, cfg(test) is exempt)"; \
	  echo "Wall-clock reads in library code must go through uva_core::Clock — run 'make clock-audit'."; \
	  exit 1; \
	fi; \
	echo "OK: $$n chrono::Utc::now() call(s) in lib production code (baseline $(CLOCK_BASELINE); app crate uva-app-server is exempt, cfg(test) is exempt)"

# ---- Harness shape (R122) ----
# `clippy.toml` sets `cognitive-complexity-threshold = 20`, but
# `clippy::cognitive_complexity` is allow-by-default and its own docs
# now say it no longer measures what its name suggests: it counts
# decisions only (no nesting, no loops, no `?`), so at R122 the whole
# `uva-harness` loop maxed out at 8/20 and the threshold was inert.
# The properties this harness actually maintains are **flat nesting and
# short functions**, and neither was measured. These two lints are what
# clippy recommends in its place.
#
# Scoped to `uva-harness`'s **production** code on purpose: a
# workspace-wide run of `too_many_lines` at this threshold reports 24
# sites (14 of them production), and this round touched none of them,
# so enforcing the budget everywhere would be an unrequested refactor.
# The harness is the crate whose shape is the contract here.
#
# `--lib` keeps tests and examples out. The thresholds live in
# `scripts/harness-shape/clippy.toml`, NOT the workspace one: a
# threshold *enables* its allow-by-default lint, so a root-level value
# would hit every crate under `lint-rust`'s `-D warnings`. The gate
# script injects that file through `CLIPPY_CONF_DIR`, and it is what
# makes `excessive_nesting` able to fire at all (default 0 = disabled).
.PHONY: check-harness-shape

check-harness-shape: ## Fail when a uva-harness production fn grows past the length / nesting budget
	@bash scripts/check-harness-shape.sh

# ---- Harness comment budget ----
# Doc comments on private items are reading tax when they restate
# the signature or narrate the next three lines; the ones recording
# a non-obvious constraint are the payload and stay. The gate fails
# on growth so a later round cannot quietly put the tax back.
check-harness-docs: ## Fail when uva-harness private-item doc comments grow past the budget
	@bash scripts/check-harness-docs.sh --self-test
	@bash scripts/check-harness-docs.sh

# ---- Public docs ratchet ----
# `[workspace.lints] missing_docs` only reaches a crate that opts in
# with `[lints] workspace = true`, and none does, so the frozen
# crates' public surface is kept honest by this ratchet instead:
# per-crate counts of undocumented `pub` items that only go down.
check-public-docs: ## Fail when a frozen crate grows an undocumented public item
	@bash scripts/check-public-docs.sh

# ---- Single writer per session ----
# A session's log has exactly one writer: the session crate's
# `LogWriter`. Every row a session gains — the model's turns, the tool
# results, the user's prompts, the feedback, a fork's inherited rows
# — goes through it, so the ordinals come from one counter and the
# flush cadence is one schedule. A direct `SessionSink::append` /
# `append_many` outside the writer is a second write path, and the
# next person who needs a row written outside the writer would add one
# and CI would stay green. The gate skips the test code (an inline
# `#[cfg(test)] mod` block is skipped for its whole extent, and the
# files a `#[cfg(test)] mod NAME;` pulls in are skipped too).
check-single-writer: ## Fail when a session's rows are written outside the session's one writer
	@bash scripts/check-single-writer.sh

# ---- Row vocabulary (one place spells a row's type) ----
# A session row's `type` is declared once, by `uva-session`'s
# `RowTag` / `RowDialect` (`events/vocab/mod.rs`). Production code
# reads it through `classify_row` and writes it through the typed
# constructors; a hand-spelled tag in a tag position (`"type": "…"`,
# `== Some("…")`, a `"…" =>` arm) is a second declaration that drifts
# the first time the vocabulary renames. The gate's audited table
# covers spellings that are not row tags (an operation kind, a
# content-block kind, …); `--self-test` proves the detector still
# fires.
check-row-vocabulary: ## Fail when production code spells a session row tag itself
	@bash scripts/check-row-vocabulary.sh --self-test
	@bash scripts/check-row-vocabulary.sh

# ---- List recipe (one place builds a page envelope) ----
# Every cursor-paginated list route answers the same six questions —
# which sort keys, how they order, which default, which way `-` turns
# it, which row a page resumes after, what the envelope holds. Six
# per-route copies drifted silently until `routes/list/` became the one
# implementation (ADR 0027); this gate is what keeps it the one. A
# `List { … }` literal outside the recipe is a second construction
# path — `List::new` / `List::map` are not, because they carry no page.
check-list-recipe: ## Fail when a list envelope is built outside the list recipe
	@bash scripts/check-list-recipe.sh

# ---- Plugin shape (R-G) ----
# Plugin tool roots (`crates/uva-tool-plugins`, `crates/uva-tool-task`
# plus `crates/uva-mcp` / `-skill` / `-attachment`) ship one tool (or
# one tool-like facade) per module and stay flat — `lib.rs` plus focused
# siblings. Long-file drift hurts the same way it did in the harness,
# so the gate is a sibling of `check-harness-shape` but scoped at the
# **file** grain (plugins are small, the natural remediation is
# splitting the file, not extracting inline helpers).
#
# Baseline is encoded as a single integer in
# `scripts/plugin-shape/baseline` — the highest current line count
# across the scanned file set, captured at gate introduction. The
# gate fails on growth past that baseline; intentional cleanup that
# lowers the max updates the baseline in the same commit (mirrors
# the `check-test-layout` 300-band precedent).
.PHONY: plugin-shape-audit check-plugin-shape

plugin-shape-audit: ## List plugin-crate production files and their line counts (largest first)
	@for root in crates/uva-tool-plugins crates/uva-tool-task \
	             crates/uva-policy-plugins crates/uva-hook-plugins crates/uva-mcp \
	             crates/uva-skill crates/uva-attachment; do \
	  [[ -d "$$root/src" ]] || continue; \
	  find "$$root/src" -name '*.rs' ! -name 'tests.rs' -exec wc -l {} +; \
	done | sort -rn | head -20

check-plugin-shape: ## Fail when a plugin-crate production file grows past the baseline
	@bash scripts/check-plugin-shape.sh

# ---- Capability matrix (the host matrix only improves) ----
# The matrix in `docs/host-capabilities.md` is the reverse view of the
# resolved profile: every row carries a status, a `planned` row anchors
# itself, a landed row cites evidence that is on disk, every method it
# names is registered by the server, and the gap count never rises
# above `scripts/host-capabilities/baseline`.
check-host-capabilities: ## Fail when the capability matrix regresses
	@bash scripts/check-host-capabilities.sh

# ---- Committed profile document (the deployment's own tree) ----
# `.agents/profile.toml` is a project asset: the deployment's statement
# of the whole plugin tree. It drifts silently — a plugin gains an
# `inject` edge, the document keeps describing the old tree — so the
# gate compares the document's `inject` against the effective edge set
# the kernel resolves per row (manifest ∪ row declaration). A manifest
# edge the document does not restate is red; a document edge the
# manifest does not declare is legal (the document widening a plugin
# is the feature).
check-profile-document: ## Fail when the committed profile document drifts from the plugin manifests
	@bash scripts/check-profile-document.sh

# ---- Inline test-module layout (R105–R109) ----
# No production file may grow an inline `#[cfg(test)] mod` block to
# the hard limit, and the 300-line band may only shrink. New test
# modules go in sibling `tests.rs` files, next to the module they test.
TEST_BLOCK_LIMIT := 400
TEST_BLOCK_BASELINE := 0
.PHONY: test-layout-audit check-test-layout

test-layout-audit: ## List every inline test module >=300 lines in a production file
	@for f in $$(find crates -name '*.rs' -path '*/src/*' ! -name 'tests.rs' ! -path '*/tests/*'); do \
	  awk -v F="$$f" '/^#\[cfg\(test\)\]$$/ && !seen { seen=1; start=FNR; check=1; n=0; next } check && n<3 { n++; if ($$0 ~ /^[ \t]*mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) ismod=1; if ($$0 ~ /mod/) check=0 } END { if (seen && ismod) printf "%s:%d: %d-line inline test module\n", F, start, NR-start+1 }' $$f; \
	done | awk -F: '{ n=$$3+0 } n >= 300'

check-test-layout: ## Fail when an inline test module reaches the hard limit or the 300-line band grows
	@out=$$(for f in $$(find crates -name '*.rs' -path '*/src/*' ! -name 'tests.rs' ! -path '*/tests/*'); do \
	  awk -v F="$$f" '/^#\[cfg\(test\)\]$$/ && !seen { seen=1; start=FNR; check=1; n=0; next } check && n<3 { n++; if ($$0 ~ /^[ \t]*mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) ismod=1; if ($$0 ~ /mod/) check=0 } END { if (seen && ismod) printf "%d %s\n", NR-start+1, F }' $$f; \
	done | sort -rn); \
	hard=$$(echo "$$out" | awk -v L=$(TEST_BLOCK_LIMIT) '$$1 >= L { c++ } END { print c+0 }'); \
	band=$$(echo "$$out" | awk '$$1 >= 300 { c++ } END { print c+0 }'); \
	if [ "$$hard" -gt 0 ]; then \
	  echo "FAIL: $$hard inline test module(s) at or above $(TEST_BLOCK_LIMIT) lines:"; \
	  echo "$$out" | awk -v L=$(TEST_BLOCK_LIMIT) '$$1 >= L { print "  " $$0 }'; \
	  echo "Move them to a sibling tests.rs — 'make test-layout-audit' lists every site."; \
	  exit 1; \
	fi; \
	if [ "$$band" -gt $(TEST_BLOCK_BASELINE) ]; then \
	  echo "FAIL: $$band inline test module(s) >=300 lines (baseline $(TEST_BLOCK_BASELINE))"; \
	  echo "New test modules belong in sibling tests.rs files, not inline blocks."; \
	  exit 1; \
	fi; \
	echo "OK: $$band inline test module(s) >=300 lines (baseline $(TEST_BLOCK_BASELINE)); none at or above $(TEST_BLOCK_LIMIT)"

# ---- Docker ----
# `make image-build-*` shells out to `scripts/images.sh` so the same
# script that local devs and CI share is the one this Makefile
# invokes (single-source-of-truth for build args, registry, identity
# injection). The compose-driven `docker-*` targets below cover
# dev-mode orchestration (live-rebuild on source change).

IMAGE_TARGETS := server web mcp

.PHONY: docker docker-up docker-down docker-build docker-prod-up docker-prod-down
.PHONY: image-build image-build-server image-build-web image-build-mcp image-push
.PHONY: server-artifact windows-artifact

image-build: image-build-server image-build-web image-build-mcp ## Build all three container images (server + web + mcp)

image-build-server: ## Build uva-app-server image (Dockerfile.server, multi-stage cargo-chef)
	bash scripts/images.sh build server

image-build-web: ## Build uva-web image (Dockerfile.web, Vite + nginx with envsubst)
	bash scripts/images.sh build web

image-build-mcp: ## Build uva-mcp-server image (Dockerfile.mcp, stdio binary)
	bash scripts/images.sh build mcp

server-artifact: ## Export Linux musl binary to ./dist/uva-app-server (matches ci.yml cross-linux-musl)
	bash scripts/images.sh artifact server

windows-artifact: ## Export Windows PE to ./dist/uva-app-server.exe (matches ci.yml cross-windows)
	bash scripts/images.sh artifact windows

image-push: ## Push all three images to the configured registry (REGISTRY / TAG)
	PUSH=1 bash scripts/images.sh build all

docker: docker-build ## Build Docker images

docker-build: ## Build all Docker images (dev target)
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_DEV) build

docker-up: ## Start Docker Compose (dev)
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_DEV) up -d

docker-down: ## Stop Docker Compose (dev)
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_DEV) down

docker-prod-up: ## Start Docker Compose (production)
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_PROD) up -d

docker-prod-down: ## Stop Docker Compose (production)
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_PROD) down

# ---- Deploy ----

.PHONY: deploy deploy-local deploy-prod

deploy: build ## Build all artifacts (alias for `build`)
	@echo "Build complete. Run 'make deploy-local' to start the server."

deploy-local: build-server ## Run server locally with production build
	./target/debug/$(SERVER_CRATE)

deploy-prod: build-release docker-prod-up ## Build release and start production Docker

# ---- Cleanup ----

.PHONY: clean clean-rust clean-web clean-docker

clean: clean-rust clean-web ## Clean all build artifacts

clean-rust: ## Clean Rust build artifacts
	cargo clean

clean-web: ## Clean frontend build artifacts
	cd $(WEB_DIR) && rm -rf dist node_modules

clean-docker: ## Remove Docker containers and images
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_DEV) down --rmi all -v
	$(DOCKER_COMPOSE) -f $(COMPOSE_FILE_PROD) down --rmi all -v

# ---- Health ----

.PHONY: health

health: ## Check server probe endpoints (liveness + readiness)
	@curl -sS http://localhost:$(SERVER_PORT)/livez || echo "Server not alive"
	@curl -sS http://localhost:$(SERVER_PORT)/readyz || echo "Server not ready"
