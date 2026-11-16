/**
 * 智能体 — the built-in default agent (never flippable, D22) and the
 * agents registered from `.agents/agents`.
 *
 * Agents are registered from files, not from enable bits: a profile row
 * per agent does not exist, so a switch here would be a lie. The
 * default agent's switch is shown locked for the reason design D22
 * names; every other row says where its lifecycle lives. Deleting a
 * definition is the settings MCP's job (D28), not a switch.
 */
import { useEffect, useState } from 'react';
import { agentsApi, type DefaultAgent } from '../../api/agents';
import type { AgentDetail } from '../../api/types';
import { EnableToggle } from '../EnableToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';

export function AgentsSection({ reloadToken }: SettingsSectionProps) {
  const [agents, setAgents] = useState<AgentDetail[] | null>(null);
  const [defaultAgent, setDefaultAgent] = useState<DefaultAgent | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      agentsApi.list({ limit: 100 }, controller.signal),
      agentsApi.defaultAgent(controller.signal),
    ])
      .then(([list, fallback]) => {
        setAgents(list.data);
        setDefaultAgent(fallback);
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken]);

  const builtin = agents?.filter((agent) => agent.protected) ?? [];
  const added = agents?.filter((agent) => !agent.protected) ?? [];

  const describe = (agent: AgentDetail): string => {
    const tools = `${agent.tools.length} 个工具`;
    const model = agent.model_hint ? ` · ${agent.model_hint}` : '';
    return agent.description ? `${agent.description} · ${tools}${model}` : `${tools}${model}`;
  };

  return (
    <>
      <SettingsCard
        title="内置"
        description="整个 harness 靠内置的默认 agent 运转，它不可停用。"
        testId="agents-builtin"
      >
        {error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取 agent 目录：${error}`}
          </p>
        ) : !agents ? (
          <p className="nt-settings__empty">正在读取 agent 目录…</p>
        ) : builtin.length === 0 && !defaultAgent ? (
          <p className="nt-settings__empty">这个部署没有内置 agent 行。</p>
        ) : (
          <ul className="nt-settings__rows">
            {builtin.map((agent) => (
              <RowCard
                key={agent.name}
                icon="🤖"
                label={agent.name}
                badge="内置"
                description={describe(agent)}
                actions={
                  <EnableToggle
                    checked
                    locked
                    lockedReason="内置 · 不可停用"
                    label={`内置 agent ${agent.name}`}
                    testId={`agent-toggle-${agent.name}`}
                  />
                }
                testId={`agent-row-${agent.name}`}
              />
            ))}
            {builtin.length === 0 && defaultAgent ? (
              <RowCard
                icon="🤖"
                label={defaultAgent.name}
                badge="默认"
                description={`整个 harness 的运转方式（${defaultAgent.source}）`}
                actions={
                  <EnableToggle
                    checked
                    locked
                    lockedReason="内置 · 不可停用"
                    label={`默认 agent ${defaultAgent.name}`}
                    testId="agent-toggle-default"
                  />
                }
                testId="agent-row-default"
              />
            ) : null}
          </ul>
        )}
      </SettingsCard>

      <SettingsCard
        title="来自 plugin 排布"
        description="在默认 agent 之外附加的智能体；定义注册自 .agents/agents。"
        testId="agents-added"
      >
        {!agents ? (
          <p className="nt-settings__empty">正在读取 agent 目录…</p>
        ) : added.length === 0 ? (
          <p className="nt-settings__empty">还没有附加的 agent 定义。</p>
        ) : (
          <ul className="nt-settings__rows">
            {added.map((agent) => (
              <RowCard
                key={agent.name}
                icon="🤖"
                label={agent.name}
                description={describe(agent)}
                actions={
                  <EnableToggle
                    checked
                    locked
                    lockedReason="智能体由文件注册；启用位随 plugin 排布，没有独立 profile 行"
                    label={`附加 agent ${agent.name}`}
                    testId={`agent-toggle-${agent.name}`}
                  />
                }
                testId={`agent-row-${agent.name}`}
              />
            ))}
          </ul>
        )}
      </SettingsCard>
    </>
  );
}
