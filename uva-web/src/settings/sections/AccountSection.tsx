/**
 * 账户 — who is using this harness, and which credentials exist.
 *
 * There is no account system in this repository (single local host, no
 * sign-in), so the card says exactly that instead of faking a login
 * surface. Credentials are read-only: the providers that answered
 * `models/list` are the credentials that exist, and no wire writes a
 * key (D28 sends capability changes through the settings MCP, not the
 * UI).
 */
import { useEffect, useState } from 'react';
import { listModels, type ModelsResponse } from '../../api/chat-stream';
import type { SettingsSectionProps } from '../sections';
import { SettingsCard } from '../SettingsCard';
import { RowCard } from '../RowCard';

export function AccountSection({ reloadToken }: SettingsSectionProps) {
  const [models, setModels] = useState<ModelsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    listModels(controller.signal)
      .then((response) => {
        setModels(response);
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken]);

  const providers = new Map<string, number>();
  for (const entry of models?.models ?? []) {
    providers.set(entry.provider, (providers.get(entry.provider) ?? 0) + 1);
  }

  return (
    <>
      <SettingsCard title="账户" description="本机 · 无订阅 · 凭据只存在本机" testId="account-card">
        <ul className="nt-settings__rows">
          <RowCard
            icon="🧑"
            label="dev"
            badge="免费"
            description="本机用户；没有远程账户，也没有登录态。"
          />
          <RowCard icon="💳" label="订阅" description="无。全部能力由本机进程与本地凭据提供。" />
        </ul>
      </SettingsCard>

      <SettingsCard
        title="凭据"
        description="凭据只存在本机；这里只读显示 provider 的可用情况。"
        testId="account-credentials"
      >
        {error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取模型目录：${error}`}
          </p>
        ) : providers.size === 0 ? (
          <p className="nt-settings__empty">尚无可用 provider。</p>
        ) : (
          <ul className="nt-settings__rows">
            {[...providers].map(([provider, count]) => (
              <RowCard
                key={provider}
                icon="🔑"
                label={provider}
                description={`${count} 个模型可用`}
                testId={`credential-${provider}`}
              />
            ))}
          </ul>
        )}
      </SettingsCard>
    </>
  );
}
