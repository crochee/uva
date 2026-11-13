/**
 * 技能与命令 — what the `skills` row loaded, plus the one enable bit
 * that exists for it.
 *
 * A skill's body lives in the plugin layout (`.agents/skills/<name>`);
 * the profile has one row for the whole surface, not one per skill. The
 * card therefore leads with that row's switch and lists each skill as
 * read-only, with `⟳ 重新载入` re-reading the directory (the same
 * `skills/reload` the removed Skills page used).
 */
import { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { List, ReloadSkillsResponse, Skill } from '../../api/types';
import { useToast } from '../../hooks/useToast';
import { EnableToggle } from '../EnableToggle';
import { ProfileToggle } from '../ProfileToggle';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';
import { useProfileSurface } from '../useProfileSurface';

export function SkillsSection({ reloadToken }: SettingsSectionProps) {
  const surface = useProfileSurface(reloadToken);
  const toast = useToast();
  const [skills, setSkills] = useState<Skill[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const [listVersion, setListVersion] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    api
      .get<List<Skill>>('/api/v1/skills?limit=100', controller.signal)
      .then((response) => {
        setSkills(response.data);
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken, listVersion]);

  const reloadSkills = async (): Promise<void> => {
    setReloading(true);
    try {
      const response = await api.post<ReloadSkillsResponse>('/api/v1/skills/reload');
      toast.push({
        variant: 'success',
        message: `已重新载入 ${response.count} 个技能。`,
      });
      setSkills(null);
      setListVersion((value) => value + 1);
    } catch (cause) {
      toast.push({
        variant: 'error',
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setReloading(false);
    }
  };

  const skillsRow = (surface.rows ?? []).find((row) => row.id === 'skills');

  return (
    <>
      <SettingsCard
        title="技能"
        description="技能与命令的正文在 plugin 排布里；profile 只提供 skills 行的整体开关。"
        actions={
          <button
            type="button"
            className="nt-settings__reload"
            onClick={() => void reloadSkills()}
            disabled={reloading}
            data-testid="skills-reload"
          >
            {reloading ? '⟳ 载入中…' : '⟳ 重新载入'}
          </button>
        }
        testId="skills-card"
      >
        {surface.error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取 profile：${surface.error}`}
          </p>
        ) : skillsRow ? (
          <ul className="nt-settings__rows">
            <RowCard
              icon="⌘"
              label="skills"
              badge={skillsRow.status === 'mounted' ? undefined : '未装配'}
              description={`${skillsRow.runtime} · ${skillsRow.status}`}
              actions={
                <ProfileToggle
                  surface={surface}
                  row={skillsRow}
                  label="启用 skills 行"
                  testId="skills-row-toggle"
                />
              }
              testId="profile-row-skills"
            />
          </ul>
        ) : (
          <p className="nt-settings__empty">这个 profile 没有 skills 行。</p>
        )}
      </SettingsCard>

      <SettingsCard title="已载入的技能" testId="skills-list">
        {error ? (
          <p className="nt-settings__error" role="alert">
            {`无法读取技能目录：${error}`}
          </p>
        ) : !skills ? (
          <p className="nt-settings__empty">正在读取技能目录…</p>
        ) : skills.length === 0 ? (
          <p className="nt-settings__empty">.agents/skills 下还没有技能。</p>
        ) : (
          <ul className="nt-settings__rows">
            {skills.map((skill) => (
              <RowCard
                key={skill.name}
                icon="📚"
                label={skill.name}
                description={skill.description || '（无描述）'}
                actions={
                  <EnableToggle
                    checked
                    locked
                    lockedReason="技能随 skills 行启用；没有独立的 profile 行"
                    label={`技能 ${skill.name}`}
                    testId={`skill-toggle-${skill.name}`}
                  />
                }
                testId={`skill-row-${skill.name}`}
              />
            ))}
          </ul>
        )}
      </SettingsCard>
    </>
  );
}
