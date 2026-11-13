# The desktop shell's workspace-local entry points. The ROOT Makefile
# must NOT grow desktop targets: it runs --all-features --all over the
# root workspace only, and desktop/ is outside it by design (C2).
SHELL := /bin/bash
.DEFAULT_GOAL := help

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

frontend: ## Build uva-web and sync its dist into desktop/dist
	cd ../uva-web && npm ci && npm run build
	rm -rf dist && cp -R ../uva-web/dist dist

sidecar: ## Copy the root-workspace app-server binary as the Tauri sidecar
	cargo build --manifest-path ../Cargo.toml -p uva-app-server --release
	mkdir -p src-tauri/binaries
	cp ../target/release/uva-app-server "src-tauri/binaries/uva-app-server-$$(rustc -vV | sed -n 's/host: //p')"

check: frontend ## cargo check over this workspace only
	cargo check --all

test: frontend ## Unit tests, one crate (never --workspace)
	cargo test -p uva-desktop --lib

bundle: frontend sidecar ## Package the release bundle (needs cargo-tauri; sidecar via overlay)
	cd src-tauri && cargo tauri build --config tauri.bundle.conf.json

lint: frontend ## clippy with the root make lint-rust's exact flags
	cargo clippy --all-targets --all-features --tests --all -- -D warnings

fmt: ## Format (root rustfmt.toml applies via upward search)
	cargo +nightly fmt --all

fmt-check: ## Check formatting without writing
	cargo +nightly fmt --all --check

deny: ## License / ban / source check for THIS workspace's lockfile (C10)
	cargo deny --workspace check licenses bans sources

run: frontend ## Dev run (debug build; window opens the Vite dev server)
	cargo run -p uva-desktop

.PHONY: help frontend sidecar check test bundle lint fmt fmt-check deny run
