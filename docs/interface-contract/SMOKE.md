# 端到端冒烟（server 无状态闭环）

`contract.yaml` / `contract.json` 是 `make contract-scan` 的产物：本页是它
旁边的**手测步骤**——一条可复跑的路径，证明前端调用的那条契约在真实进程里
走得通。静态扫描证明“两侧对得上”，这一步证明“对得上之后还能跑”。

全流程不需要 API key，也不需要网络：部署用内置的 `demo` provider
（`[providers.<name>] type = "demo"`，键名免密），它把最后一条用户消息回显
回来。

## 0. 准备一个免密部署

```bash
cd <repo>
mkdir -p /tmp/elysper-smoke/.agents
cat > /tmp/elysper-smoke/.agents/config.toml <<'TOML'
default_provider = "demo"
default_model = "demo"

[providers.demo]
type = "demo"
api_key_env = "ELYSPER_DEMO_KEY"
default_model = "demo"
TOML
```

`api_key_env` 只是配置结构的必填字段名——`demo` provider 从不读它。

## 1. 起后端

```bash
cargo run -p elysper-server --features test-utils -- --directory /tmp/elysper-smoke
# 默认监听 127.0.0.1:8080（`--host` / `--port` 可改）
```

`--features test-utils` 是**必须的**：`demo` provider 只在
`cfg(any(test, feature = "test-utils"))` 下编译（`state/app_state/demo_provider.rs`），
普通构建里 `type = "demo"` 会被判成不支持的 provider 类型，boot 直接失败
（`No LLM provider available`）。

就绪探针：`curl -s http://127.0.0.1:8080/readyz`（前端轮询的也是它）→ `{"status":"ok"}`。

## 2. 建会话、发消息（带一个附件）

```bash
SESSION=smoke-1
curl -s -X POST http://127.0.0.1:8080/api/v1/chat/sessions \
  -H 'content-type: application/json' \
  -d "{\"session_id\":\"$SESSION\"}"

# 一条带附件的文本消息：图片的 base64 内联在 `attachments` 里
curl -s -X POST "http://127.0.0.1:8080/api/v1/chat/sessions/$SESSION/messages" \
  -H 'content-type: application/json' \
  -d '{
        "text": "look at this",
        "attachments": [
          {"kind": "image", "mime_type": "image/png",
           "data_base64": "iVBORw0KGgo="}
        ]
      }'
```

响应里有 `message_id` 与 `queued: true`——消息已经**落盘**（`parked_prompt`
行），不是躺在进程内存里：路由在回答 `200` 之前，会等控制器把这一行写进
会话日志并 flush 完。所以「已接受」=「已落盘」——在这条回答之后立刻
`kill -9` 掉进程，重启后这条消息仍然在日志里、仍然会被执行。

（集群部署里另一台节点拥有该会话时，`200` 的含义是「已转发给属主」，
由属主在它自己的日志里落盘。）

## 3. 历史还在、附件也在

```bash
curl -s "http://127.0.0.1:8080/api/v1/sessions/$SESSION" | jq '.history'
```

历史里有这一条用户消息（`UserInput` 行），附件以引用（`attachments`）的形式
记在它旁边。行类型是 `UserInput`——**排队**进来的 turn 记成 `UserInput`；
`user_message` 是另一种行，只由 **steer**（往正在跑的会话里注入）写出。

## 4. 连发两条消息 = 两个 turn

```bash
curl -s -X POST "http://127.0.0.1:8080/api/v1/chat/sessions/$SESSION/messages" \
  -H 'content-type: application/json' -d '{"text":"first"}'
curl -s -X POST "http://127.0.0.1:8080/api/v1/chat/sessions/$SESSION/messages" \
  -H 'content-type: application/json' -d '{"text":"second"}'

curl -s "http://127.0.0.1:8080/api/v1/sessions/$SESSION" \
  | jq '[.history[] | select(.type == "UserInput")] | length'
```

三条（第一条带附件的那条 + 两条排队消息）——每条排队消息各自成为一个 turn
（顺序不乱），这是一条排队消息 = 一个 turn 的语义（Task 8）。

实测（2026-10-28，本机）：

```
UserInput rows: 3
UserInput texts: ['look at this', 'first', 'second']
user_message rows: 0
```

## 5. 重启后端，历史仍在

```bash
kill -TERM <server-pid>     # 10 秒内退出，退出码 0
cargo run -p elysper-server --features test-utils -- --directory /tmp/elysper-smoke &
curl -s "http://127.0.0.1:8080/api/v1/sessions/$SESSION" \
  | jq '[.history[] | select(.type == "UserInput")] | length'
```

历史逐条还在；排队中尚未开始的 turn 也会被日志恢复并执行。

实测（2026-10-28，本机）：SIGTERM 之后 `cargo run` 的退出码是 `0`；重启后
`UserInput rows: 3`、文本仍是 `['look at this', 'first', 'second']`。

## 6. 静态契约仍然对齐

```bash
make contract-scan && make contract-check
```

`contract-check` 报告 `frontend_only=0 backend_only=0`：前端 `fetch` 的每条
路径都在后端路由里，后端的每条路径要么被前端调用，要么在
`contract-closure/curated/web-free-endpoints.json` 里写明了它服务的是
web 之外的消费者。

## 前端那一半

上面的步骤是 server 侧的闭环。前端的等价走查（浏览器里建会话 → 发消息（含
附件）→ 收到流式回复 → 刷新后历史仍在）见仓库 `AGENTS.md` §4.2：仓库**没有**
前端 e2e 套件，这一步是手测，不是 CI。
