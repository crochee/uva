/**
 * Hooks — hook rows plus the two read-only settings the prototype
 * shows.
 *
 * Hook definitions come from the plugin layout and never from an
 * imported config file (D24). Today no native profile row carries hooks,
 * so the list is empty until a plugin declares one; the two read-only
 * rows say where the run mode lives and that the log surface does not
 * exist yet — a disabled button with the reason, not a fake log.
 */
import { ProfileToggle } from '../ProfileToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';
import { useProfileSurface } from '../useProfileSurface';

export function HooksSection({ reloadToken }: SettingsSectionProps) {
  const surface = useProfileSurface(reloadToken);
  const rows = (surface.rows ?? []).filter((row) => row.id.includes('hook'));

  return (
    <>
      <SettingsCard
        title="已配置的 Hooks"
        description="Hook 的定义来自 plugin 排布，不再导入别处的配置文件。"
        testId="hooks-card"
      >
        {surface.error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取 profile：${surface.error}`}
          </p>
        ) : !surface.rows ? (
          <p className="nt-settings__empty">正在读取 profile…</p>
        ) : rows.length === 0 ? (
          <p className="nt-settings__empty">profile 里还没有 Hooks 行。</p>
        ) : (
          <ul className="nt-settings__rows">
            {rows.map((row) => (
              <RowCard
                key={row.id}
                icon="🪝"
                label={row.id}
                description={`${row.name} · ${row.status}`}
                actions={
                  <ProfileToggle
                    surface={surface}
                    row={row}
                    label={`启用 profile 行 ${row.id}`}
                    testId={`hook-toggle-${row.id}`}
                  />
                }
                testId={`profile-row-${row.id}`}
              />
            ))}
          </ul>
        )}
      </SettingsCard>

      <SettingsCard title="运行方式" testId="hooks-runmode">
        <ul className="nt-settings__rows">
          <RowCard
            icon="⚙"
            label="Hooks 命令运行方式"
            description="由 profile 的 hook 行声明（沙箱 / 直通）；界面暂不提供切换。"
            actions={<span className="nt-settings__tag">只读</span>}
            testId="hooks-runmode-row"
          />
          <RowCard
            icon="📜"
            label="运行日志"
            description="运行日志面尚未提供。"
            actions={
              <button
                type="button"
                className="nt-settings__reload"
                disabled
                title="运行日志面尚未提供"
                data-testid="hooks-log-open"
              >
                查看日志
              </button>
            }
            testId="hooks-log-row"
          />
        </ul>
      </SettingsCard>
    </>
  );
}
