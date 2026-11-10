/**
 * MCP — the row that supervises MCP servers.
 *
 * Server rows are `mcp.json` entries assembled by the `mcp` profile row;
 * the profile surface exposes the row's own enable bit (a write that
 * usually answers `restart-required`, because supervision lives in the
 * process), and its resolved `config` is shown read-only as the
 * truth about which servers the row was handed. No per-server enable
 * bit exists, so none is invented.
 */
import { ProfileToggle } from '../ProfileToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';
import { useProfileSurface } from '../useProfileSurface';

export function McpSection({ reloadToken }: SettingsSectionProps) {
  const surface = useProfileSurface(reloadToken);
  const rows = (surface.rows ?? []).filter((row) => row.id === 'mcp' || row.id.startsWith('mcp-'));

  return (
    <SettingsCard
      title="MCP Servers"
      description="server 随服务端存活；这一行只决定 MCP 面是否装配。写回通常要求重启 supervision。"
      testId="mcp-card"
    >
      {surface.error ? (
        <p className="nt-settings__error" role="alert">
          {`无法读取 profile：${surface.error}`}
        </p>
      ) : !surface.rows ? (
        <p className="nt-settings__empty">正在读取 profile…</p>
      ) : rows.length === 0 ? (
        <p className="nt-settings__empty">这个 profile 没有 mcp 行。</p>
      ) : (
        <ul className="nt-settings__rows">
          {rows.map((row) => (
            <RowCard
              key={row.id}
              icon="🔌"
              label={row.id}
              badge={row.status === 'mounted' ? undefined : '未装配'}
              description={
                <>
                  {row.runtime} · {row.status}
                  {row.config ? (
                    <details className="nt-settings__config">
                      <summary>已解析的 config</summary>
                      <pre>{JSON.stringify(row.config, null, 2)}</pre>
                    </details>
                  ) : null}
                </>
              }
              actions={
                <ProfileToggle
                  surface={surface}
                  row={row}
                  label={`启用 profile 行 ${row.id}`}
                  testId={`mcp-toggle-${row.id}`}
                />
              }
              testId={`profile-row-${row.id}`}
            />
          ))}
        </ul>
      )}
    </SettingsCard>
  );
}
