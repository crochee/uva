/**
 * The `?` shortcut reference (design D30).
 *
 * One table, grouped by scope, sourced from `lib/shortcut-keys` — the
 * same table the settings · 通用 section counts, so the number and the
 * panel can never disagree. Mounted once by the shell; opens on `?`
 * (outside a text field) and on the `uva:shortcut-help` event the
 * settings row dispatches.
 */
import { useEffect, useState } from 'react';
import { SHORTCUT_HELP, type ShortcutScope } from '../../lib/shortcut-keys';
import { isEditableTarget } from '../../hooks/useKeyboardShortcuts';
import { Modal } from './Modal';

const SCOPES: ReadonlyArray<ShortcutScope> = ['列表', '打开', '全局'];

export function ShortcutHelp() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key !== '?' || event.metaKey || event.ctrlKey || event.altKey) return;
      if (isEditableTarget(event.target)) return;
      event.preventDefault();
      setOpen((value) => !value);
    }
    function onOpen(): void {
      setOpen(true);
    }
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('uva:shortcut-help', onOpen);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('uva:shortcut-help', onOpen);
    };
  }, []);

  return (
    <Modal
      open={open}
      onClose={() => setOpen(false)}
      title="快捷键"
      titleId="nt-shortcut-help-title"
      testId="shortcut-help"
    >
      <table className="nt-shortcut-table">
        <tbody>
          {SCOPES.map((scope) => (
            <ScopeRows key={scope} scope={scope} />
          ))}
        </tbody>
      </table>
    </Modal>
  );
}

function ScopeRows({ scope }: { scope: ShortcutScope }) {
  const rows = SHORTCUT_HELP.filter((row) => row.scope === scope);
  if (rows.length === 0) return null;
  return (
    <>
      <tr className="nt-shortcut-table__scope">
        <th colSpan={2}>{scope}</th>
      </tr>
      {rows.map((row) => (
        <tr key={row.keys}>
          <td className="nt-shortcut-table__keys">
            <kbd>{row.keys}</kbd>
          </td>
          <td>{row.action}</td>
        </tr>
      ))}
    </>
  );
}
