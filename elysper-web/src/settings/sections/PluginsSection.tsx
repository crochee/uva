/**
 * 插件 — the deployment's plugin composition plus every row of the
 * resolved profile.
 *
 * The composition answers "what did this deployment install?" (and why
 * an entry did not); the profile list answers "what will the kernel
 * assemble?" — which is the surface the settings page is allowed to
 * change (D26). A composition entry only gets a live switch when the
 * profile holds a row of the same id; otherwise the switch says so
 * instead of pretending a write would land.
 */
import { useEffect, useState } from 'react';
import { pluginsApi, type PluginsResponse } from '../../api/plugins';
import type { ProfileRow } from '../../api/settings-profile';
import { rowEnabled } from '../../api/settings-profile';
import { EnableToggle } from '../EnableToggle';
import { ProfileToggle } from '../ProfileToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';
import { useProfileSurface } from '../useProfileSurface';

function RuntimeBadge({ row }: { row: ProfileRow }) {
  return <span className="nt-settings__tag">{row.runtime}</span>;
}

export function PluginsSection({ reloadToken }: SettingsSectionProps) {
  const surface = useProfileSurface(reloadToken);
  const [composition, setComposition] = useState<PluginsResponse | null>(null);
  const [compositionError, setCompositionError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    pluginsApi
      .list(controller.signal)
      .then((response) => {
        setComposition(response);
        setCompositionError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setCompositionError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken]);

  const profileRows = surface.rows ?? [];
  const profileById = new Map(profileRows.map((row) => [row.id, row]));
  const resolved = composition?.resolved ?? [];
  const skipped = composition?.skipped ?? [];
  const resolvedNames = new Set(resolved.map((plugin) => plugin.name));
  const remaining = profileRows.filter((row) => !resolvedNames.has(row.id));
  const coreRows = remaining.filter((row) => row.segment === 'core');
  const uiRows = remaining.filter((row) => row.segment === 'ui');

  return (
    <>
      <SettingsCard
        title="已发现的插件"
        description="部署配置里声明的插件；profile 有同名行时开关写回 profile，否则只读。"
        testId="plugins-composition"
      >
        {compositionError ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取部署组成：${compositionError}`}
          </p>
        ) : !composition ? (
          <p className="nt-settings__empty">正在读取部署组成…</p>
        ) : resolved.length === 0 ? (
          <p className="nt-settings__empty">这个部署没有额外的插件条目。</p>
        ) : (
          <ul className="nt-settings__rows">
            {resolved.map((plugin) => {
              const row = profileById.get(plugin.name);
              return (
                <RowCard
                  key={plugin.name}
                  icon="🧩"
                  label={plugin.name}
                  description={
                    <>
                      {plugin.kind === 'remote' ? '远程进程' : '静态（进程内）'} · authority:{' '}
                      {plugin.authority} · 风险: {plugin.risk}
                      {plugin.tools.length > 0 ? (
                        <span className="nt-settings__tags">
                          {plugin.tools.map((tool) => (
                            <code key={tool} className="nt-settings__tag">
                              {tool}
                            </code>
                          ))}
                        </span>
                      ) : (
                        <span className="nt-settings__tags">
                          <span className="nt-settings__tag">不注册工具</span>
                        </span>
                      )}
                    </>
                  }
                  actions={
                    row ? (
                      <ProfileToggle
                        surface={surface}
                        row={row}
                        label={`启用插件 ${plugin.name}`}
                        testId={`plugin-toggle-${plugin.name}`}
                      />
                    ) : (
                      <EnableToggle
                        checked={false}
                        locked
                        lockedReason="该插件来自部署配置 [plugins]；profile 中没有同名行"
                        label={`插件 ${plugin.name} 无 profile 行`}
                        testId={`plugin-toggle-${plugin.name}`}
                      />
                    )
                  }
                  testId={`plugin-row-${plugin.name}`}
                />
              );
            })}
          </ul>
        )}
      </SettingsCard>

      {skipped.length > 0 && (
        <SettingsCard
          title={`未安装（${skipped.length}）`}
          description="配置了但启动时没有装上；boot 记录了原因，能力因此缺失。"
          testId="plugins-skipped"
        >
          <ul className="nt-settings__rows">
            {skipped.map((plugin, index) => (
              <RowCard
                key={`${plugin.name}-${index}`}
                icon="⚠️"
                label={plugin.name}
                description={plugin.reason}
                dimmed
                testId={`plugin-skipped-${index}`}
              />
            ))}
          </ul>
        </SettingsCard>
      )}

      {surface.error ? (
        <SettingsCard title="profile 行" testId="plugins-profile-error">
          <p className="nt-settings__error" role="alert">
            {`无法读取 profile：${surface.error}`}
          </p>
        </SettingsCard>
      ) : (
        <>
          <SettingsCard
            title="profile · core 行"
            description="内核将要装配的插件行；开关直接写回 profile（D26）。"
            testId="plugins-profile-core"
          >
            {!surface.rows ? (
              <p className="nt-settings__empty">正在读取 profile…</p>
            ) : coreRows.length === 0 ? (
              <p className="nt-settings__empty">profile 的 core 段没有独立行。</p>
            ) : (
              <ul className="nt-settings__rows">
                {coreRows.map((row) => (
                  <RowCard
                    key={row.id}
                    icon="⚙"
                    label={row.id}
                    badge={row.status === 'mounted' ? undefined : '未装配'}
                    description={
                      <>
                        <RuntimeBadge row={row} /> · {row.status}
                      </>
                    }
                    actions={
                      <ProfileToggle
                        surface={surface}
                        row={row}
                        label={`启用 profile 行 ${row.id}`}
                        testId={`profile-toggle-${row.id}`}
                      />
                    }
                    testId={`profile-row-${row.id}`}
                  />
                ))}
              </ul>
            )}
          </SettingsCard>

          <SettingsCard
            title="profile · ui 行"
            description="UI 装载器的行：声明插槽的 ui-bundle 插件（两个宿主共用插槽 id）。"
            testId="plugins-profile-ui"
          >
            {!surface.rows ? (
              <p className="nt-settings__empty">正在读取 profile…</p>
            ) : uiRows.length === 0 ? (
              <p className="nt-settings__empty">这个部署没有 ui-bundle 行。</p>
            ) : (
              <ul className="nt-settings__rows">
                {uiRows.map((row) => (
                  <RowCard
                    key={row.id}
                    icon="🎨"
                    label={row.id}
                    badge={rowEnabled(row) ? undefined : '已停用'}
                    description={<RuntimeBadge row={row} />}
                    actions={
                      <ProfileToggle
                        surface={surface}
                        row={row}
                        label={`启用 profile 行 ${row.id}`}
                        testId={`profile-toggle-${row.id}`}
                      />
                    }
                    testId={`profile-row-${row.id}`}
                  />
                ))}
              </ul>
            )}
          </SettingsCard>
        </>
      )}
    </>
  );
}
