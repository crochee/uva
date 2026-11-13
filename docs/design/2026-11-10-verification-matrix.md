# 验证矩阵：十刀落地与设计的整体满足度（2026-11-10）

> 本文件是设计文档 [`2026-11-09-desktop-frontend-tree-design.md`](2026-11-09-desktop-frontend-tree-design.md)
> §8 逐刀验收的**当前状态证据表**。每一行都给结论与可复现的证据（命令输出摘要或文件:行号），
> 不用"应该""看起来"这类措辞。

## 0. 验证方法

| 证据类型 | 含义 |
|---|---|
| **门禁** | 仓库自身的自动门禁（`make ci` / `make check-web` / markdownlint / actionlint）当场跑通 |
| **测试** | 具名测试通过，且**回退实现即失败**（逐条验证过） |
| **实机** | 我在本机真跑过（终端 / 网关 / 桌面壳进程 / stdio 帧） |
| **页面** | Playwright MCP 驱动真实页面的 DOM 断言 |
| **审计** | 独立代理的只读审计结论（带文件:行号），我复核过关键项 |

不可由机器证明的（像素级窗口、需要特定 provider 的 DOM 形态）标 **人工**。

## 1. 逐刀验收（设计 §8 原文 → 当前状态）

| 刀 | 验收条目 | 结论 | 证据 |
|---|---|---|---|
| 0 | 本地 openspec 层与代码一致 | ✅ | `openspec/specs/{web-projection,desktop-projection,shared-interface}/spec.md`；`git check-ignore -v` → `.gitignore:41`（本地层不入库） |
| 1 | 网关 crate 测试绿且不起真进程 | ✅ | `cargo test -p uva-gateway`（假 spawn 覆盖：就绪状态行 / 退避重启 / Drop 回收 / id 关联 / 通知扇出 / 重连重放） |
| 1 | `make ci` 绿（members 与 ORDER 同步） | ✅ | `Cargo.toml` members 含新 crate；`scripts/check-publishable.sh` ORDER 同序；门禁跑通 |
| 2 | TUI 测试绿 | ✅ | `cargo test -p uva-tui`（104） |
| 2 | 手工冒烟：起一轮对话、审批 `y`/`n`、`--attach` 行为一致 | ✅ 实机 | 真 provider 一轮对话完成（`turn completed`）；`--attach` 连已在跑的 `serve`；**网关被杀 ⇒ 5s 内状态栏转 `offline: the gateway is unreachable`**（真实修掉的一个 bug） |
| 2 | stdio 契约仍被覆盖 | ✅ | `cargo test -p uva-app-server --test stdio_roundtrip`（9） |
| 3 | `make ci` 绿（desktop 不进 root workspace） | ✅ | `desktop/Cargo.toml` 自成 workspace；root members 无 desktop；root lockfile 无 tauri |
| 3 | 手工：窗口 → 对话 → 审批 → 杀 sidecar → 离线徽标 → 恢复 | ⚠️ 部分 | **进程生命周期实测**：sidecar 自愈（新代 + `/readyz` 200）、关壳零孤儿、readyz/SPA 200。**"同一份前端"硬证**：`diff -rq uva-web/dist desktop/dist` 逐字节一致（23 个文件）⇒ 窗口渲染的内容就是 Playwright 验过的那份 SPA。**像素级窗口内容 = 人工**（`cd desktop && make run`；本机无 X11 截图工具） |
| 4 | 每格有值 / 锚点 / 证据在盘 / 方法在册 / 缺口数 ≤ 基线 | ✅ | `scripts/check-host-capabilities.sh` + 基线；**五种反向验证全部命中**（无锚点 planned、删列、缺口回升、证据不存在、编造方法） |
| 5 | sessions/chat 在 tui 列 ≥ adapted | ✅ | 矩阵 §6：两者 `adapted`；`crates/uva-tui/src/domains.rs` 落地证据 |
| 6 | 七域 tui 列全部 ≥ adapted | ✅ | 矩阵 §6：agents/skills/tools/tasks/schedules/workflows/evals 全 `full`；方法名对照 71 个注册方法 |
| 7 | 禁用一行 ⇒ 能力不可用 ∧ 界面块不渲染 | ✅ | `state/tests/profile_lifecycle.rs::one_disabled_row_withdraws_the_capability_and_its_block` |
| 7 | 装不起来的写 ⇒ 报错、文件不变、树不变、界面仍显旧目录 | ✅ | `…::a_failed_mcp_write_changes_neither_the_document_nor_the_tree`（文档逐字节不变 + 树不变 + 不广播） |
| 7 | 装得起来的写 ⇒ 提交、刷新、下一 turn 用新装配 | ✅ | `state/tests/profile_apply.rs::applied_commits_the_document_and_broadcasts`；`state/tests/self_mcp.rs` |
| 7 | 创建能力 ⇒ 数据文件 + profile 行都在 | ✅（语义修订后） | 设计 §8 刀 7④ 按实际模型修订：**数据文件 + 承载它的聚合行**（agent 无独立 profile 行，它由目录发现）；见 §14 的缺口处置 |
| 8 | 同一 ui-bundle 行两宿主可达（插槽 id 同、呈现异） | ✅ | 服务器 `routes/ui.rs` 返回 `slots` + `schemas`；浏览器 `SlotRegion`；终端 `ui_schema.rs` + `render/ui_slots.rs`；双宿主实机冒烟（页面断言 + 终端缓冲文本） |
| 8 | 未声明插槽 ⇒ 拒绝 + 记录 | ✅ | `ui/tree/tests.rs` 三类拒绝 + `tracing::error!` 记录（此前只有上抛无记录） |
| 8 | 终端新增 UI 插件不改终端代码即多一块界面 | ✅（边界写明） | 已有 kind（panel/list/key-value）零改动；**新 kind 需终端渲染器**——边界写进 `crates/uva-tui/README.md` 与 `uva-web/README.md` |
| 9 | 旧页面与路由零残留 | ✅ | `uva-web/src/pages/` 只剩 `ChatPage` / `SettingsPage`；旧路径落到 `/chat`；残留 grep 零命中 |
| 9 | 新壳按原型呈现（Playwright） | ✅ 页面 | 新壳快照：HARNESS 徽标、工作区组常驻、bash `❯ 默认工作区`、设置九分区与过滤搜索 |
| 9 | `make check-web` 绿；`npm run build` 通过 | ✅ 门禁 | `make check-web` 绿；`npm run build` 通过且**已进 root CI**（此前只有 desktop job 构建） |

## 2. 验证期抓到并修掉的缺陷（均有回退即失败的测试）

| # | 缺陷 | 根因 | 修法 | 测试 |
|---|---|---|---|---|
| 1 | attach 模式网关静默死亡时 TUI 永远 `connected` | 探针只认 `Transport` 错误；client 的 `request` 断连时挂起等重连 | client 暴露 `connected()`，探针先读它 | `transport/tests.rs::an_idle_disconnect_is_announced_by_the_probe` |
| 2 | 第二次 `profile/set` 恒报 `restart-required`（整个装配被判重挂） | 文档层缺 `config` 键被解析成 `{}`（显式清空）而非 `null`（无意见） | 缺键解析为 null，与 `to_toml` 对称 | `state/tests/reload.rs::a_second_write_does_not_remount_the_world` |
| 3 | 每次第二次写都重挂 boot 物化的行 | 配置内部的 JSON `null` 键不可被 TOML 表达 | diff 比较"TOML 视图"（递归丢 null 值键） | `…::a_round_trip_over_an_unrepresentable_null_is_not_a_change` |
| 4 | 同值重复写仍广播 `profile/changed` | no-op 也走 `applied` 分支 | diff 为空不广播 | `state/tests/profile_apply.rs::a_no_op_write_does_not_broadcast` |
| 5 | **agent 能力创建/删除路径穿越（P1）** | 名称只查 `is_protected`，原样 join 进 `.agents/agents/` | 两处入口都走 `valid_definition_name` | `state/tests/profile_security.rs`（回退后 `Delete("../../escape")` 返回 **Applied**） |
| 6 | 退避窗口内 drop 复活孤儿进程 | 退避醒来后循环顶部不检查 shutdown，重新 launch 且子进程被监督任务永久持有 | `Notify` 唤醒 + 循环顶部/launch 后双检查 + 回收路径拿得到受监管代 | `gateway::tests::dropping_the_gateway_inside_the_restart_backoff_does_not_revive_an_orphan` |
| 7 | client 关闭时挂起的请求永久挂起 | `run` 的出口分支漏 `fail_pending` | 清算收敛到 `run` 的出口 | `gateway::tests::a_request_parked_during_the_redial_window_fails_when_the_client_closes` |
| 8 | 指定端口被占时无限重启同一端口 | `pick_port` 对非零端口照用 | 占用则另选空闲端口，并真正传给 `serve` | `gateway::tests::an_occupied_port_is_replaced_by_a_free_one` |
| 9 | 重载与写事务可并发进入装配 | 重载不取单写锁 | 两入口共用 `profile_write`，事务内走免锁变体 | `app-server::a_reload_waits_for_the_write_transaction` |
| 10 | `dial` 对黑洞地址永不返回 | 无超时 | 5s 预算，覆盖连接与握手 | `gateway::tests::the_dial_gives_up_when_the_gateway_never_accepts` |

**审计还揪出一个"空真"旧测试**：`dropping_start_before_its_outcome_reaps_the_generation`
从未 poll 过 `start` future（被丢弃的 future 不执行），断言恒真——已改为真正启动再放弃，
"boot 被放弃"这条路径才第一次被覆盖。

## 3. rust-dev 契约核对（`skill://rust-dev`）

| 条 | 结论 | 证据 |
|---|---|---|
| lib 无 `unwrap()/expect()` | ✅ | 全仓 grep（排除测试）：仅 2 处——`profile/merge.rs` 有文档的不变量 panic、desktop app 入口 |
| 公开项文档 + rustdoc 无警告 | ✅ | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p uva-gateway -p uva-tui -p uva-app-server` exit=0 |
| 无 MutexGuard 跨 await | ✅ | 审计逐处核对 + `profile_apply`/`reconcile` 全部快照式 |
| 长任务可取消 | ✅ | 审计的 2 条 must-fix + 3 条 should-fix 全部闭合：退避内 drop 不再复活孤儿（`Notify` 唤醒 + 循环双检查）、client 出口清算 pending（挂起的请求会失败而不是永久挂起）、`dial` 有超时（连接与握手双预算）、就绪探测感知 shutdown、写路径与重载共用单写锁 |
| 无 unsafe | ✅ | 三个新 crate `#![forbid(unsafe_code)]` |
| clone 有持有者理由 | ✅ | 审计抽查 69 处克隆，逐处有理由 |

## 4. 六维质量目标（精简 / 低复杂度 / 易维护 / 高内聚低耦合 / 高性能 / 幂等）

| 维度 | 落点与证据 |
|---|---|
| **精简** | 网关 crate 把"进程监管 + 协议"从 TUI/桌面壳里抽出来（原先两处各写一套）；矩阵门禁取代一次性脚本 |
| **低复杂度** | `make check-harness-shape`（函数 ≤100 行、嵌套 ≤4）+ 本轮新增代码零超限；`classify` 的比较逻辑收敛到一处 `equivalent`/`toml_view` |
| **易维护** | 计划→偏差记录→落地结果三段留痕（计划尾部偏差记录 + 设计 §14）；门禁反查事实而非自述 |
| **高内聚低耦合** | 能力矩阵的每格有主人与证据；`crates/uva-kernel` 在刀 7 里**零改动**（`git diff` 为空） |
| **高性能** | 渲染热路径零 JSON 解析（`from_value` 只在消息处理里）；域表 12 项线性扫；探针 5s 一次（测试态 200ms） |
| **幂等** | 同值重复写：diff 空、不广播、树不动（实测三次连写）；网关订阅重放不双发（既有测试）；重启退避不复活孤儿（已闭合并钉住） |

## 5. 上手（界面端与无头）

| 入口 | 路径 | 证据 |
|---|---|---|
| 无头 | `uva-app-server core`（stdio JSON-RPC 2.0，Content-Length 分帧） | `crates/uva-app-server/README.md`；`stdio_roundtrip` 9 测试 |
| 网关 + 浏览器 | `serve`（`/rpc` + `/readyz` + SPA） | 根 README；`uva-web/README.md` |
| 终端 | `cargo run -p uva-tui`（自起网关）或 `--attach` | `crates/uva-tui/README.md` |
| 桌面 | `cd desktop && make frontend && make run` | `desktop/README.md`（本轮新建） |

## 6. 剩余人工项（机器证明不了）

1. **桌面窗口的像素级内容**（窗口打开、对话、审批弹窗、离线徽标）——进程生命周期我已实测，像素需要人眼。
2. **`overridden` / `failed` 之外的第四态在浏览器里的回执**——`applied` / `restart-required` 已实证，另两态需要构造上层 patch 覆盖或无效行名。
3. **思考块的 DOM 两态**——需要会吐 thinking 段的 provider（纯函数与样式已覆盖）。

## 7. 门禁总账

| 门禁 | 结果 |
|---|---|
| `make ci`（fmt / clippy / rustdoc / 编译指南 / 全部棘轮 / 逐 crate 测试） | exit=0（377s，零 FAIL） |
| `make check-web`（tsc + eslint + prettier） | 绿 |
| `npm run build` | 通过（并已进 root CI） |
| markdownlint（43 个 crate README + CONTRIBUTORS.md） | 0 issues（46 个文件） |
| desktop workspace 门禁（lint / test / fmt-check / deny） | 全绿（test 10 通过；deny 三节 ok） |
| 三个 crate 的测试套件 | gateway 23 · tui 104 · app-server 663，全绿 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` + doctest | exit=0 / 绿 |
| actionlint（desktop workflow） | 无输出 |
| `cargo deny check licenses bans sources`（root + desktop） | 三节 ok |
