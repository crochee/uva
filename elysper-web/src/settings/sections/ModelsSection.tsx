/**
 * 模型 — the provider catalogue.
 *
 * The catalogue itself is `models/list` (one row per model, grouped by
 * provider). Enablement is a property of the row that builds the
 * catalogue — `model-adapter` in the native set, or a provider row the
 * profile declares (`llm-<provider>`); anything else shown here is
 * informational, so no switch is invented for it.
 */
import { useEffect, useState } from 'react';
import { listModels, type ModelsResponse } from '../../api/chat-stream';
import type { ProfileRow } from '../../api/settings-profile';
import { ProfileToggle } from '../ProfileToggle';
import { EnableToggle } from '../EnableToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';
import { useProfileSurface } from '../useProfileSurface';

function modelDescription(entry: ModelsResponse['models'][number]): string {
  const tools = entry.supports_tools ? '工具' : '无工具';
  const streaming = entry.supports_streaming ? '流式' : '非流式';
  return `上下文 ${entry.context_window.toLocaleString()} · ${tools} · ${streaming}`;
}

export function ModelsSection({ reloadToken }: SettingsSectionProps) {
  const surface = useProfileSurface(reloadToken);
  const [catalogue, setCatalogue] = useState<ModelsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    listModels(controller.signal)
      .then((response) => {
        setCatalogue(response);
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken]);

  const profileRows = surface.rows ?? [];
  const adapterRow: ProfileRow | undefined = profileRows.find((row) => row.id === 'model-adapter');
  const providerRows = new Map<string, ProfileRow>();
  for (const row of profileRows) {
    if (row.id.startsWith('llm-')) providerRows.set(row.id.slice(4), row);
  }

  const byProvider = new Map<string, ModelsResponse['models']>();
  for (const entry of catalogue?.models ?? []) {
    const bucket = byProvider.get(entry.provider);
    if (bucket) bucket.push(entry);
    else byProvider.set(entry.provider, [entry]);
  }

  return (
    <>
      <SettingsCard
        title="目录行"
        description="模型目录由 profile 的 `model-adapter` 行构建；这一行停了，所有 provider 都不再出现在选择器里。"
        testId="models-adapter"
      >
        {surface.error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取 profile：${surface.error}`}
          </p>
        ) : !surface.rows ? (
          <p className="nt-settings__empty">正在读取 profile…</p>
        ) : adapterRow ? (
          <ul className="nt-settings__rows">
            <RowCard
              icon="◈"
              label="model-adapter"
              description={`${adapterRow.runtime} · ${adapterRow.status}`}
              actions={
                <ProfileToggle
                  surface={surface}
                  row={adapterRow}
                  label="启用 model-adapter 行"
                  testId="model-adapter-toggle"
                />
              }
              testId="profile-row-model-adapter"
            />
          </ul>
        ) : (
          <p className="nt-settings__empty">这个 profile 没有 model-adapter 行。</p>
        )}
      </SettingsCard>

      {error ? (
        <SettingsCard title="模型" testId="models-catalogue-error">
          <p className="nt-settings__error" role="alert">
            {`无法读取模型目录：${error}`}
          </p>
        </SettingsCard>
      ) : (
        <SettingsCard
          title="模型"
          description={`provider 由 profile 装配；默认 ${catalogue?.default_provider ?? '—'}/${
            catalogue?.default_model ?? '—'
          }。`}
          testId="models-catalogue"
        >
          {!catalogue ? (
            <p className="nt-settings__empty">正在读取模型目录…</p>
          ) : byProvider.size === 0 ? (
            <p className="nt-settings__empty">没有可用的 provider。</p>
          ) : (
            [...byProvider].map(([provider, models]) => {
              const providerRow = providerRows.get(provider);
              const isDefaultProvider = provider === catalogue.default_provider;
              return (
                <div
                  key={provider}
                  className="nt-settings__group"
                  data-testid={`model-group-${provider}`}
                >
                  <h3 className="nt-settings__group-title">
                    {provider}
                    {isDefaultProvider ? (
                      <span className="nt-settings__row-badge" style={{ marginLeft: 8 }}>
                        默认
                      </span>
                    ) : null}
                  </h3>
                  <ul className="nt-settings__rows">
                    {models.map((entry) => {
                      const isDefaultModel =
                        isDefaultProvider && entry.model === catalogue.default_model;
                      return (
                        <RowCard
                          key={`${entry.provider}/${entry.model}`}
                          icon="◇"
                          label={entry.model}
                          badge={isDefaultModel ? '默认' : undefined}
                          description={modelDescription(entry)}
                          actions={
                            providerRow ? (
                              <ProfileToggle
                                surface={surface}
                                row={providerRow}
                                label={`启用 provider ${provider}`}
                                testId={`provider-toggle-${provider}`}
                              />
                            ) : (
                              <EnableToggle
                                checked
                                locked
                                lockedReason="provider 由 model-adapter 行承载，没有独立的 profile 行"
                                label={`provider ${provider}`}
                                testId={`provider-toggle-${provider}`}
                              />
                            )
                          }
                          testId={`model-row-${entry.provider}-${entry.model}`}
                        />
                      );
                    })}
                  </ul>
                </div>
              );
            })
          )}
        </SettingsCard>
      )}
    </>
  );
}
