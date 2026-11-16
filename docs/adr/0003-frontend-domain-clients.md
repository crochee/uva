# 前端调用按域组织，不按 URL 路径组织

`uva-web/src/api/client.ts` 在 JSON-RPC 之上另造了一套 REST 词汇：45 行手写的 path→method 映射表，加一个字符串匹配器 `resolveRoute`。这个匹配器从不抛错，而是**回退到更短的模板**——于是 `GET /api/v1/sessions/abc` 答的是 `sessions/list`，按名字取的 GET 返回列表，`POST /api/v1/evals/x/run` 调的是 `evals/create`。45 行里 14 行没有任何调用点会用到。

我们把前端调用改成**按域的类型化客户端**（方法名 + 参数 + 错误码），形状照 `api/settings-profile.ts` 已有的样子；`client.ts` 的路径表与 `resolveRoute` 删除，35 处调用点**按域推进**迁移，`client.ts` 最后删。前端类型由 `schemars` 导出 schema 后生成，漂移由 `scripts/check-web-contract.sh` 把住。

## Considered Options

- **保留路径风格、映射表由 Rust 生成**：被否。那要把假 REST 词汇永久化，还要让 Rust 知道前端想要哪一套 URL 词汇——等于把耦合从「手写表」升级成「跨语言契约」。
- **一次性建 10 个域客户端再改 35 处调用点**：被否。diff 太大，评审看不出每域的边界，回归也难定位到域。

## Consequences

- 类型漂移有门禁，**调用点没有**。`scripts/check-web-contract.sh` 只比较
  `types.ts` 与 Rust 类型；九个域客户端与 35 处已迁移调用点没有单元测试——
  wire 方法名或参数名写错了照样编译，`check-web` 的 `tsc`/`vitest` 都看不见，
  人工交叉核对只剩 MCP 冒烟。契约清单已经带 `method` 字段，将来可以加一道
  门禁，把每个客户端的 `METHOD` 表与清单逐个 diff；在那之前这条边界靠评审。
  **这道门禁已经有了**（`make check-web-methods`，`scripts/check-web-methods.sh`），
  但比对的不是契约清单而是**服务端的方法表**：清单的 52 行只命名 **31** 个方法值
  （29 个具体值 + 2 个通配 `*/list` / `schedules/*`），前端调用的 38 个方法里有
  **15** 个在清单里没有字面条目（清单是类型目录，不是方法目录），拿清单当全集
  会把这 15 个正确的方法判成错——其中 3 个（`agents/list`、`evals/list`、
  `schedules/delete`）落在通配族的覆盖范围内，其余 12 个不在任何一行里。服务端
  的路由注册表才是全集（64 个请求方法），通知方法另算；契约清单里那些具体的
  `method` 值也一并比对，生成出来的 `types.ts` 不能宣称一个已经不存在的接口。
  参数名这一半仍然没有门禁——那要单元测试，不是一道 diff。
- 迁移是按域推进的原子改动，`client.ts` 的删除是最后一步——那时它是唯一的 REST 词汇，删掉即完成切换。服务端注册 64 个方法而前端只用其中一部分，每个域客户端要写清自己用到哪几个方法；未被前端使用的服务端方法不因这次改动而收敛，那属于另一件事。
