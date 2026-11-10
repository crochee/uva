/**
 * 通用 — the shell's own UI rows: theme, density, font size and the
 * shortcut summary.
 *
 * Theme already has a store (`useTheme`); density and font size are
 * local display preferences (`general-prefs`). Nothing here is a
 * profile write — these are how *this* browser draws the shell.
 *
 * The shortcut row's count comes from the shared key table, and the
 * button opens the same help panel `?` does (via the shell event), so
 * the two entry points cannot list different bindings.
 */
import { useTheme, type Theme } from '../../hooks/useTheme';
import { SHORTCUT_HELP } from '../../lib/shortcut-keys';
import { FONT_SIZES, useDisplayPrefs, type Density } from '../general-prefs';
import { SettingsCard } from '../SettingsCard';

const THEMES: ReadonlyArray<{ value: Theme; label: string }> = [
  { value: 'light', label: '浅色' },
  { value: 'dark', label: '深色' },
  { value: 'system', label: '跟随系统' },
];

const DENSITIES: ReadonlyArray<{ value: Density; label: string }> = [
  { value: 'compact', label: '紧凑' },
  { value: 'comfortable', label: '舒适' },
];

export function GeneralSection() {
  const { theme, setTheme } = useTheme();
  const { density, setDensity, fontSize, setFontSize } = useDisplayPrefs();

  return (
    <SettingsCard
      title="外观"
      description="主题、密度、字号与会话气泡样式。存在本机，跨工作区保留。"
      testId="general-card"
    >
      <div className="nt-settings__control-row">
        <span>主题</span>
        <div className="nt-settings__segmented" role="group" aria-label="主题">
          {THEMES.map((option) => (
            <button
              key={option.value}
              type="button"
              className={`nt-settings__segment${
                theme === option.value ? ' nt-settings__segment--on' : ''
              }`}
              aria-pressed={theme === option.value}
              onClick={() => setTheme(option.value)}
              data-testid={`theme-${option.value}`}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>

      <div className="nt-settings__control-row">
        <span>密度</span>
        <div className="nt-settings__segmented" role="group" aria-label="密度">
          {DENSITIES.map((option) => (
            <button
              key={option.value}
              type="button"
              className={`nt-settings__segment${
                density === option.value ? ' nt-settings__segment--on' : ''
              }`}
              aria-pressed={density === option.value}
              onClick={() => setDensity(option.value)}
              data-testid={`density-${option.value}`}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>

      <div className="nt-settings__control-row">
        <label htmlFor="settings-font-size">字号</label>
        <select
          id="settings-font-size"
          className="nt-settings__select"
          value={fontSize}
          onChange={(event) => setFontSize(Number(event.target.value))}
          data-testid="font-size"
        >
          {FONT_SIZES.map((size) => (
            <option key={size} value={size}>
              {size}
            </option>
          ))}
        </select>
      </div>

      <div className="nt-settings__control-row">
        <span>快捷键</span>
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: 'var(--space-2)' }}>
          <span className="nt-settings__tag" data-testid="shortcut-count">
            {`${SHORTCUT_HELP.length} 条键位`}
          </span>
          <button
            type="button"
            className="nt-settings__link"
            onClick={() => window.dispatchEvent(new CustomEvent('elysper:shortcut-help'))}
            data-testid="shortcuts-open"
          >
            查看全部
          </button>
        </span>
      </div>
    </SettingsCard>
  );
}
