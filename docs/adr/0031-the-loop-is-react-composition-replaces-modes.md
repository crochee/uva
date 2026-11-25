# 循环只有 react：组合取代模式

三个「推理模式」——`cot`（思维链）、`plan-execute`（计划-执行）、
`best-of-n`（多候选评分）——住在 `uva-policy-plugins`，各自一个模块、一个
feature、一个 facade feature，且都实现同一个 `ReasoningStrategy`。它们卖的是
「换一个循环」，但每一样都能用 react 组合出来：思维链是提示工程，计划-执行
是 react + 一个计划工具，多候选是 react 跑 n 次再评分。维护成本是三份
策略实现、三份测试、三个 feature 与文档里的三段说明；换来的能力为零。

## 决定

- **删除三个策略**：`uva-policy-plugins` 的 `cot` / `plan_execute` /
  `best_of_n` 模块、它们的 feature、facade 的
  `strategy-cot` / `strategy-plan-execute` / `strategy-best-of-n`、
  app-server 依赖表里的三项、三个 example。
- **`ReasoningStrategy` 与注册表保留**：循环是可替换的范式（冻结 lib 面），
  `with_strategy(Arc<dyn MyStrategy>)` 仍在；仓外实现注册
  `StrategyRegistry::register` 即可。内置的解析表只剩 `react` / `re-act` /
  `re_act`。
- **`best_of_n` 的 `CandidateScorer` 一并删除**：它唯一的使用者是已删除的
  策略；`uva-core` 的 `judge_score` 评分纯函数随之下沉回非公开面。

## 后果

- 配置里写 `strategy = "cot"` 的部署在启动时得到点名的「未知策略」错误，
  而不是静默回退——错误信息列出注册表与内置别名。
- 要同等行为的消费方组合 react：思维链写在 agent 的提示里，计划-执行是
  一个工具，多候选是宿主跑 n 个回合再按 `judge_score` 式评分挑一个。
- SEAMS / plugins / examples / DEPLOYMENT / host-capabilities 文档与
  `check-row-vocabulary` 豁免表同步收缩。
