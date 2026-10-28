# elysper-tool-task

The `task` tool plugin: multi-agent delegation by running registered
peers as sub-agents (depth cap, verification gates, worktree
isolation).

It is the first consumer of the
[`DelegatingTool`](../../SEAMS.md) seam — i.e. it needs the loop's
internal capability (event forwarding, cancellation, depth), so it
depends on `elysper-harness` rather than just `elysper-tool`.

> Plugin crate for `elysper-tool`. See [`SEAMS.md`](../../SEAMS.md) for
> the delegation seam.

## CI 契约

- 离线（无 HTTP / DB / OTel），`make check-mvp-deps` 断言；
- `cargo test -p elysper-tool-task --lib` 绿。

## Examples（2 个）

| Example | Shows |
| :--- | :--- |
| `delegation_gate` | 子任务 gate 命令必须通过 |
| `worktree_isolation` | 每个子 agent 一个 `elysper/agent-<ulid>` worktree |

跑： `cargo run --example <name> -p elysper-tool-task`。
