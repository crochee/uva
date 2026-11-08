# elysper-agents

Agent definitions as **files**: one directory per agent —
`agent.md`（YAML frontmatter 承载身份，正文即 system prompt）加可选的
`config.json`（结构化字段），两者无共享键，合成一个
`AgentDescriptor` 读 / 写；`discover` 按 `~/.agents` 两层布局的四源
优先级（项目直定义 > 项目插件包 > 全局直定义 > 全局插件包）走一遍
并按名去重。布局本身由
[`elysper-core::layout`](../elysper-core/src/layout.rs) 拥有，本 crate
只是 agent 域之上的薄格式层。

> 公开 API 与文件格式见 [`elysper-agents/src/lib.rs`](src/lib.rs)。

## 用法

```rust
use std::path::Path;
use elysper_agents::{discover, format, AgentDefinition};

// 发现：走两层布局，解析失败的定义跳过（warn），不拖垮整个注册表
for found in discover(Path::new("/srv/workspace")) {
    println!("{} ← {:?}", found.definition.name(), found.location);
}

// 读写：CRUD 面的持久化就是这两个文件
let definition =
    AgentDefinition::read(Path::new("/srv/workspace/.agents/agents/reviewer"))?;
let descriptor = definition.descriptor(); // → elysper_core::AgentDescriptor
format::write(&definition)?;              // 写回 agent.md + config.json
```

经 facade 使用时打开 `agents` feature（`elysper = { features = ["agents"] }`）。

## CI 契约

- `cargo test -p elysper-agents --lib` 绿；
- 依赖 `elysper-core` + serde 系 + `thiserror` / `tracing`（无运行时、
  无 HTTP），不依赖运行时类型（`make check-public-api-runtime`）。
