# Elysper 接口契约并集表

> 生成时间: 2026-10-28T05:57:21.006Z
> 版本: 1

## 统计

- Total: 63
- Paired (双侧一致): 51
- Backend-only (后端提供, 前端未调用): 0
- Web-free (后端提供, 前端按设计不调用): 12
- Frontend-only (前端调用, 后端未注册): 0

## 端点

### DELETE `/api/v1/agents/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:241`
- **前端来源**: `elysper-web/src/pages/AgentsPage.tsx:163`

### DELETE `/api/v1/attachments/{key}`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:361`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Attachment store delete. The web composer keeps the bytes in the turn, so it never deletes from the store.

### DELETE `/api/v1/evals/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:345`
- **前端来源**: `elysper-web/src/api/evals.ts:28`

### DELETE `/api/v1/schedules/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:286`
- **前端来源**: `elysper-web/src/api/schedules.ts:39`

### DELETE `/api/v1/sessions/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:150`
- **前端来源**: `elysper-web/src/pages/SessionsPage.tsx:114`

### DELETE `/api/v1/skills/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:246`
- **前端来源**: `elysper-web/src/pages/SkillsPage.tsx:135`

### DELETE `/api/v1/tools/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:270`
- **前端来源**: `elysper-web/src/pages/ToolsPage.tsx:150`

### DELETE `/api/v1/workflows/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:317`
- **前端来源**: `elysper-web/src/api/workflows.ts:42`

### GET `/api/v1/agents`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:232`
- **前端来源**: `elysper-web/src/pages/AgentsPage.tsx:90`, `elysper-web/src/pages/ChatPage.tsx:1149`, `elysper-web/src/components/ui/CommandPalette.tsx:251`

### GET `/api/v1/agents/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:241`
- **前端来源**: `elysper-web/src/pages/AgentDetailPage.tsx:56`

### GET `/api/v1/agents/default`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:237`
- **前端来源**: `elysper-web/src/pages/ChatPage.tsx:779`

### GET `/api/v1/attachments/{key}`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:361`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Attachment store read-back. The web composer keeps the bytes in the turn, so it never reads the store.

### GET `/api/v1/chat/usage`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:199`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Deployment-wide token usage. The web app reads the per-session usage (`GET /api/v1/sessions/{key}/usage`); this one serves operators, not the chat page.

### GET `/api/v1/evals`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:343`
- **前端来源**: `elysper-web/src/api/evals.ts:16`

### GET `/api/v1/evals/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:345`
- **前端来源**: `elysper-web/src/api/evals.ts:20`

### GET `/api/v1/models`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:140`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:214`

### GET `/api/v1/plugins`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:147`
- **前端来源**: `elysper-web/src/api/plugins.ts:51`

### GET `/api/v1/schedules`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:282`
- **前端来源**: `elysper-web/src/pages/SchedulesPage.tsx:39`, `elysper-web/src/api/schedules.ts:23`

### GET `/api/v1/schedules/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:286`
- **前端来源**: `elysper-web/src/api/schedules.ts:27`

### GET `/api/v1/search`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:254`
- **前端来源**: `elysper-web/src/pages/SessionsPage.tsx:221`, `elysper-web/src/components/ui/CommandPalette.tsx:272`

### GET `/api/v1/sessions`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:149`
- **前端来源**: `elysper-web/src/pages/SessionsPage.tsx:94`, `elysper-web/src/components/ui/CommandPalette.tsx:250`

### GET `/api/v1/sessions/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:150`
- **前端来源**: `elysper-web/src/pages/SessionDetailPage.tsx:199`, `elysper-web/src/api/chat-stream.ts:192`

### GET `/api/v1/sessions/{key}/events`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:163`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: SSE tail of one session's events. The web app follows `/api/v1/sessions/{key}/follow` instead (one reader per log); this route is the API surface for external clients that poll a session's event stream.

### GET `/api/v1/sessions/{key}/export`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:155`
- **前端来源**: `elysper-web/src/pages/SessionsPage.tsx:134`

### GET `/api/v1/sessions/{key}/follow`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:171`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:464`

### GET `/api/v1/sessions/{key}/status`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:167`
- **前端来源**: `elysper-web/src/pages/SessionDetailPage.tsx:233`

### GET `/api/v1/sessions/{key}/usage`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:183`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:354`

### GET `/api/v1/sessions/search`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:257`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Full-text search over session logs. The web app searches the unified index (`GET /api/v1/search`); this route is the session-scoped variant for API clients.

### GET `/api/v1/skills`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:226`
- **前端来源**: `elysper-web/src/pages/SkillsPage.tsx:72`

### GET `/api/v1/skills/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:246`
- **前端来源**: `elysper-web/src/pages/SkillDetailPage.tsx:50`

### GET `/api/v1/tasks`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:335`
- **前端来源**: `elysper-web/src/api/tasks.ts:17`

### GET `/api/v1/tasks/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:336`
- **前端来源**: `elysper-web/src/api/tasks.ts:22`

### GET `/api/v1/tools`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:266`
- **前端来源**: `elysper-web/src/pages/ToolsPage.tsx:74`

### GET `/api/v1/tools/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:270`
- **前端来源**: `elysper-web/src/pages/ToolDetailPage.tsx:83`

### GET `/api/v1/workflows`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:312`
- **前端来源**: `elysper-web/src/api/workflows.ts:26`

### GET `/api/v1/workflows/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:317`
- **前端来源**: `elysper-web/src/api/workflows.ts:30`

### GET `/livez`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:398`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Liveness probe for orchestrators. The web app polls `/api/health`.

### GET `/metrics`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:410`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Prometheus metrics. Scraped by operators, never by the web app.

### GET `/readyz`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:399`
- **前端来源**: `elysper-web/src/hooks/useServerHealth.ts:96`

### GET `/version`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:404`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Build identity for probes and clients. The web app reads its version from the bundle, not from this route.

### PATCH `/api/v1/schedules/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:294`
- **前端来源**: `elysper-web/src/api/schedules.ts:35`

### POST `/api/v1/agents`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:232`
- **前端来源**: `elysper-web/src/pages/AgentsPage.tsx:145`

### POST `/api/v1/attachments`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:357`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Attachment store upload (`kind = "attachment_ref"` payloads). The web composer sends attachment bytes inline with the turn; the store's own API is for clients that upload first and reference later.

### POST `/api/v1/chat/messages/{key}/feedback`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:216`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:173`

### POST `/api/v1/chat/sessions`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:200`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Explicit session creation. The web app lets the server mint the session on the first turn (`POST /api/v1/chat/sessions/{key}/messages` creates it on demand), so the web client never calls this route; external clients may.

### POST `/api/v1/chat/sessions/{key}/cancel`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:208`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:143`

### POST `/api/v1/chat/sessions/{key}/messages`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:204`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:243`

### POST `/api/v1/chat/sessions/{key}/regenerate`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:212`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:156`

### POST `/api/v1/evals`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:344`
- **前端来源**: `elysper-web/src/api/evals.ts:24`

### POST `/api/v1/evals/{key}/run`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:349`
- **前端来源**: `elysper-web/src/api/evals.ts:32`

### POST `/api/v1/schedules`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:282`
- **前端来源**: `elysper-web/src/api/schedules.ts:31`

### POST `/api/v1/schedules/{key}/tick`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:298`
- **前端来源**: `elysper-web/src/api/schedules.ts:44`

### POST `/api/v1/sessions/{key}/compact`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:179`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:297`

### POST `/api/v1/sessions/{key}/fork`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:159`
- **前端来源**: `elysper-web/src/pages/SessionsPage.tsx:158`

### POST `/api/v1/sessions/{key}/resume`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:175`
- **前端来源**: `elysper-web/src/api/chat-stream.ts:278`

### POST `/api/v1/skills`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:226`
- **前端来源**: `elysper-web/src/pages/SkillsPage.tsx:117`

### POST `/api/v1/skills/reload`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:230`
- **前端来源**: `elysper-web/src/pages/SkillsPage.tsx:151`

### POST `/api/v1/tools`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:266`
- **前端来源**: `elysper-web/src/pages/ToolsPage.tsx:132`

### POST `/api/v1/workflows`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:313`
- **前端来源**: `elysper-web/src/api/workflows.ts:34`

### POST `/api/v1/workflows/{key}/run`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:325`
- **前端来源**: `elysper-web/src/api/workflows.ts:52`

### POST `/chat/sessions/{key}/operation`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:371`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: The R29 operation endpoint, mounted only when `[operations] enabled = true`. The web client uses the chat message route; the operation wire is for clients that drive the gated operation surface.

### POST `/v1/messages`

- **状态**: backend
- **后端来源**: `crates/elysper-server/src/server/router.rs:427`
- **前端消费**: 否（web 之外的消费者，见 Note）
- **Note**: Anthropic-compatible surface for external Anthropic clients (the web app uses `/api/v1/chat/*`).

### PUT `/api/v1/workflows/{key}`

- **状态**: both
- **后端来源**: `crates/elysper-server/src/server/router.rs:321`
- **前端来源**: `elysper-web/src/api/workflows.ts:38`
