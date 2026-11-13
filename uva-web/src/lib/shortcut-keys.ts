/**
 * The one shortcut table.
 *
 * `GeneralSection` counts it and `ShortcutHelp` renders it, so the
 * "N 条键位" number can never disagree with the help panel. Keys are
 * the Vim-first set from design §6.1 (D30); `g c` / `g s` keep the
 * existing GitHub-style prefix navigation.
 */

export type ShortcutScope = '列表' | '打开' | '全局';

export interface ShortcutRow {
  /** The keystroke, as the user types it. */
  keys: string;
  action: string;
  scope: ShortcutScope;
}

export const SHORTCUT_HELP: ReadonlyArray<ShortcutRow> = [
  { keys: 'j / k', action: '移动光标', scope: '列表' },
  { keys: 'gg', action: '跳到首项', scope: '列表' },
  { keys: 'G', action: '跳到末项', scope: '列表' },
  { keys: 'Ctrl+d / Ctrl+u', action: '向下 / 向上半页', scope: '列表' },
  { keys: '/', action: '聚焦搜索', scope: '列表' },
  { keys: 'n / N', action: '下一个 / 上一个匹配', scope: '列表' },
  { keys: 'Enter', action: '打开光标项', scope: '打开' },
  { keys: 'y', action: '复制 id', scope: '打开' },
  { keys: 'x', action: '删除光标项（需确认）', scope: '打开' },
  { keys: 'Esc', action: '取消 / 关闭', scope: '打开' },
  { keys: ':', action: '命令面板', scope: '全局' },
  { keys: '?', action: '快捷键帮助', scope: '全局' },
  { keys: 'Ctrl+B', action: '折叠侧栏', scope: '全局' },
  { keys: 'Ctrl+K', action: '命令面板', scope: '全局' },
  { keys: 'g c', action: '跳到对话', scope: '全局' },
  { keys: 'g s', action: '跳到设置', scope: '全局' },
];
