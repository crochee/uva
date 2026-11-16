# 前端测试政策：保留纯函数单元测试，不保留 e2e 套件

`AGENTS.md` §4.2 曾明令「不保留任何前端自动化测试套件」，依据是被删掉的 Playwright e2e 套件一次跑 13 分钟、需 `cargo build` + Vite + Chromium 三件套、spec 与实现会同步腐化。那条理由对 e2e 成立，对纯函数单元测试不成立——`session-to-messages.ts` 的 1,242 行日志投影、`receipt.ts` 的四态、`chat-stream.ts` 的四个 frame validator、`useCursorList.ts` 的分页与陈旧响应防护，全是纯函数且规则密集，却只能靠 MCP 手动验收。

我们改用 **vitest@4（node 环境，不装 jsdom）** 覆盖这四项；§4.2 改写为「不保留前端 e2e 套件；纯函数单元测试用 vitest，随 `check-web` 跑」，`DEPLOYMENT.md`、两份 `docs/design/` 计划与 CI 注释同步改写。

## Considered Options

- **组件测试（jsdom / happy-dom）**：被否。DOM 环境与 Node 20 的 engines 限制相撞（vitest@5 要求 Node ≥22.12，jsdom@31 要求 ≥22.22），且组件测试会重新走回被删套件的腐化路径。
- **只在 CI 跑、不进 `make`**：被否。本地跑 `make ci` 的人就看不到类型漂移。

## Consequences

前端从此有第一条自动化的「接口即测试面」防线。§4.2 的边界也从此是**有方向的一条线**——e2e 不做、纯函数做——任何越过这条线的提议都要重新论证，而不是靠「反正不写前端测试」一句带过。
