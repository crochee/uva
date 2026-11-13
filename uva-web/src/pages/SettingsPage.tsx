/**
 * The settings surface — `/settings/:section`.
 *
 * The shell owns three things and nothing else: routing a section id to
 * its body, the source card that explains where the rows come from
 * (design §6.1: *the interface is the resolved profile read backwards*,
 * D26), and the `⟳ 重新载入` token every body refetches on. Each body
 * lives in `settings/sections/` and owns its own data.
 *
 * Unknown section ids redirect rather than 404: a stale bookmark to a
 * renamed section should land on 通用, not on a dead page.
 */
import { useState, type ComponentType } from 'react';
import { Navigate, useNavigate, useParams } from 'react-router-dom';
import {
  SETTINGS_SECTIONS,
  type SettingsSectionId,
  type SettingsSectionProps,
} from '../settings/sections';
import { AccountSection } from '../settings/sections/AccountSection';
import { AgentsSection } from '../settings/sections/AgentsSection';
import { AutomationSection } from '../settings/sections/AutomationSection';
import { GeneralSection } from '../settings/sections/GeneralSection';
import { HooksSection } from '../settings/sections/HooksSection';
import { McpSection } from '../settings/sections/McpSection';
import { ModelsSection } from '../settings/sections/ModelsSection';
import { SkillsSection } from '../settings/sections/SkillsSection';
import './SettingsPage.css';

/** Section id → body. Kept out of `sections.ts` so the registry the
 *  sidebar imports stays data-only (no section chunk in the shell). */
const SECTION_COMPONENTS: Record<SettingsSectionId, ComponentType<SettingsSectionProps>> = {
  account: AccountSection,
  general: GeneralSection,
  models: ModelsSection,
  agents: AgentsSection,
  skills: SkillsSection,
  mcp: McpSection,
  hooks: HooksSection,
  automation: AutomationSection,
};

export function SettingsPage() {
  const { section: rawSection } = useParams<{ section: string }>();
  const navigate = useNavigate();
  const [reloadToken, setReloadToken] = useState(0);

  const section = SETTINGS_SECTIONS.find((candidate) => candidate.id === rawSection);
  if (!section) {
    return <Navigate to="/settings/general" replace />;
  }
  const Body = SECTION_COMPONENTS[section.id];

  return (
    <div className="nt-settings" data-testid="settings-page">
      <header className="nt-settings__topbar">
        <div className="nt-settings__heading">
          <span className="nt-settings__heading-icon" aria-hidden>
            {section.icon}
          </span>
          <div>
            <h1 className="nt-settings__title" data-testid="settings-title">
              {section.label}
            </h1>
            <p className="nt-settings__subtitle">{section.description}</p>
          </div>
        </div>
        <button
          type="button"
          className="nt-settings__close"
          onClick={() => navigate('/chat')}
          data-testid="settings-close"
          title="返回对话 (Esc)"
        >
          esc 关闭
        </button>
      </header>
      <div className="nt-settings__body">
        <div className="nt-settings__source">
          <div className="nt-settings__source-head">
            <span className="nt-settings__source-title">来源</span>
            <code className="nt-settings__source-path">{section.source}</code>
            <button
              type="button"
              className="nt-settings__reload"
              onClick={() => setReloadToken((token) => token + 1)}
              data-testid="settings-reload"
            >
              ⟳ 重新载入
            </button>
          </div>
          <p className="nt-settings__source-note">
            这些行由 profile 装配进内核；设置页是<strong>解析后的 profile 的反推</strong>
            ——core 装的就是这棵树，这里只改启用与参数，直接写 profile 文档。
          </p>
        </div>
        <Body reloadToken={reloadToken} />
      </div>
    </div>
  );
}
