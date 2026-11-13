/**
 * The settings surface's section registry.
 *
 * One entry per row of design §6.1's settings table. This module is
 * deliberately data-only (no component imports): the sidebar and the
 * command palette read it to build the settings navigation, so pulling
 * a section's data-fetching component in here would drag every section
 * chunk into the shell bundle. `<SettingsPage>` maps ids onto
 * components itself.
 *
 * There is no "rules & memory" section (design D24), and `account` is
 * carried by the sidebar's user card, not the section list — it still
 * has a route (`/settings/account`) because that is where the user
 * card lands.
 */

export type SettingsSectionId =
  'account' | 'general' | 'models' | 'agents' | 'skills' | 'mcp' | 'hooks' | 'automation';

export interface SettingsSection {
  id: SettingsSectionId;
  /** Sidebar / page-title label. */
  label: string;
  /** Glyph from the prototype's vocabulary (no icon dependency). */
  icon: string;
  /** One-line responsibility, from §6.1's 界面职责 column. */
  description: string;
  /** Where the rows come from, from §6.1's 内容来源 column. */
  source: string;
}

export const SETTINGS_SECTIONS: ReadonlyArray<SettingsSection> = [
  {
    id: 'account',
    label: '账户',
    icon: '🧑',
    description: '用户 / 订阅 / 凭据',
    source: '—（本机）',
  },
  {
    id: 'general',
    label: '通用',
    icon: '⚙',
    description: '主题 / 密度 / 字号 / 快捷键',
    source: '壳自己的 UI 行',
  },
  {
    id: 'models',
    label: '模型',
    icon: '◈',
    description: 'provider 行的启用',
    source: '.agents/providers',
  },
  {
    id: 'agents',
    label: '智能体',
    icon: '🤖',
    description: '内置默认 agent（不可停用）+ 附加 agent 行',
    source: '内置 + .agents/agents',
  },
  {
    id: 'skills',
    label: '技能与命令',
    icon: '⌘',
    description: 'skill / command 行的启用（正文在 plugin 排布里）',
    source: '.agents/skills',
  },
  {
    id: 'mcp',
    label: 'MCP',
    icon: '🔌',
    description: 'server 行的启用',
    source: 'mcp.json',
  },
  {
    id: 'hooks',
    label: 'Hooks',
    icon: '🪝',
    description: 'hook 行的启用 + 运行方式 + 运行日志',
    source: 'plugin 排布',
  },
  {
    id: 'automation',
    label: '自动化',
    icon: '⏰',
    description: '自动化 / 模板 / 执行历史',
    source: 'plugin 排布',
  },
];

/** The account section is reached through the user card, so the
 *  sidebar's navigable list is the other seven. */
export const SETTINGS_NAV_SECTIONS: ReadonlyArray<SettingsSection> = SETTINGS_SECTIONS.filter(
  (section) => section.id !== 'account',
);

/** Props every section body receives. `reloadToken` changes when the
 *  source card's `⟳ 重新载入` runs, and is the effect dependency each
 *  data-backed section uses to refetch. */
export interface SettingsSectionProps {
  reloadToken: number;
}
