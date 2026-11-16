# uva-approval

The approval capability: the vocabulary a held tool call becomes
(`ApprovalRequest` / `Suspension` / `ResumeToken`), the policy that
decides which calls are held (`ApprovalPolicy` with modes, per-tool
overrides and workspace-bound grants), the log-derived decision
functions a resume is checked with (`open_requests`,
`check_coverage`, `align_decisions`), and the kernel plugin that
publishes the deployment's stance as the `approval` service
(`ApprovalPlugin` / `KEY_APPROVAL`).

> 详细 seam 与公开 API 见 [`uva-approval/src/lib.rs`](src/lib.rs)；
> 7 大组件的可替换点见 [`SEAMS.md`](../../SEAMS.md) §1。

## 边界

- 冻结面路径保持：`uva-harness` 在自己的 crate root re-export 本
  crate 的全部审批名，消费方的既有 import 不动（`ApprovalGate`
  暂留 harness 作薄适配器，待拦截契约下沉后整体迁入）。
- 开放集 / 恢复令牌 / 覆盖检查全部从 `SessionEvent` 行推导——
  依赖 `uva-session` 是本仓独有形状，不是待消除的耦合。
- runtime-free：无 async、无执行器、无时钟；决策函数是对调用方
  已持有行的纯折叠。

## 用法

```rust
use uva_approval::{ApprovalPlugin, ApprovalPolicy};

// 挂载：策略作为 `approval` 服务发布（uva_kernel::mount_all）。
let plugin = ApprovalPlugin::new(ApprovalPolicy::default());
```

## CI 契约

- `cargo test -p uva-approval` 绿；
- 依赖树不含 `uva-harness`（范式面之下，无回指）；
- runtime-free 名单成员（`make check-no-runtime`）。
