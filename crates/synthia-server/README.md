# synthia-server

HTTP / WebSocket server with axum: exposes the REST + SSE chat surface
that the React frontend (`synthia-web/`) talks to.

This is the **only** application crate in the workspace — the one
place where `tokio` / `axum` may appear in a public signature. Library
crates elsewhere must remain runtime-agnostic
(`make check-public-api-runtime`).

> 公开 API、路由清单与 `config.yaml` schema 见
> [`synthia-server/src/lib.rs`](src/lib.rs)。

## 用法

```sh
cargo run -p synthia-server -- --config config.yaml
# → HTTP on :8080, SSE chat on /api/v1/chat
```

## CI 契约

- `cargo test -p synthia-server --lib` 绿；
- `cargo build -p synthia-server --release` 产出单二进制
  （`make build-release`）；
- 是 `make ci` 的运行时豁免面（其它 lib crate 必须零 tokio）。

## 多节点部署

默认 `mode = "local"` 是单进程。横向扩展时，每个节点都是**无状态计算**，
durable 真源在共享文件系统上，Redis 只做控制面：

```yaml
cluster:
  mode: redis
  redis_url: redis://redis:6379
  namespace: synthia        # 可选，key/频道前缀，默认 "synthia"
  lease_ttl_secs: 15        # 可选，会话租约 TTL，默认 15
  node_id: node-1           # 可选，默认启动时随机 UUID
```

前提与语义：

- **共享 workspace**：`sessions/`、`workflows/`、`evals/`、`schedules/`
  必须对所有节点可见（NFS / EFS / 同一 PVC）。会话 JSONL 是唯一真源；
  Redis 不存会话内容。
- **租约**：`{ns}:lease:session:{user}:{session}`，只有持约节点运行该会话的
  controller，并按 TTL/3 续约。节点崩溃后 TTL 过期，任意节点下一次请求接管
  并从磁盘重建会话（in-flight run 丢失，与单节点崩溃一致）。
- **帧投递是尽力而为**：会话帧走 Redis pub/sub，慢订阅者可能丢帧；SSE 客户端
  带 `from` cursor 重连时从磁盘补齐快照 + 回放，`snapshot`/`cursor` 帧给出
  当前位置。控制帧（工具注册/删除、转发的 op、cancel）同为单次投递——漏收的
  节点保持旧视图直到下一次变更或重启。
- **provider 固定是节点本地的**：跨节点提交只支持不指定 `model` 的请求
  （owner 用自己配置的默认 provider）。显式 `model` 选择在远程会话上会
  返回错误，而不是静默换模型。
- **`/metrics` 与 `GET /api/v1/chat/usage` 按节点统计**：加节点 = 加 Prometheus
  scrape 目标；不做全局聚合（避免引入第二套一致性协议）。
- **调度器主租约**（`{ns}:lease:leader:scheduler`）已预留，但当前**无后台
  tick 任务**——调度推进是操作者显式 `POST /api/v1/schedules/{id}/tick`，
  因此不需要租约门控。
