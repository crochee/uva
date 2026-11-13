# Uva harness integration tests

Three integration-test binaries live in this directory:

| File | Coverage |
| :--- | :--- |
| `mvp_run_test.rs` | 7 MVP scenarios on a minimal ReAct agent (text / tool / cancel / max-iter / error / sequential abort / truncation telemetry). |
| `full_user_scenario.rs` | 4 end-to-end scenarios: book-flight, recovery, cancel, streaming. |
| `support/` | Shared fixtures — `FakeProvider`, `FakeTool`, `FakeProvider::tool_then_text`, `ScriptedStreamProvider`. |

Ad-hoc integration coverage also lives under
`crates/uva-harness/src/agent/re_act/tests/` (in-module
sub-suites by concern).

Run: `cargo test -p uva-harness --test <file>`.