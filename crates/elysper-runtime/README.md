# elysper-runtime

Config-driven assembly for elysper: the fat plugin factories behind the
built-in native plugins.

Each factory reads a profile entry's `config` and builds the plugin that
publishes the matching service key (`llm`, `agentLoop`, …). Mounting the
default assembly goes through this crate for every host — the app-server
gateway and external `cargo add elysper` consumers alike — so each
built-in plugin has exactly one construction site. Facade entry:
`elysper = { features = ["runtime"] }` (opt-in; it carries the provider
adapters and the tier-1 plugins alongside the kernel).
