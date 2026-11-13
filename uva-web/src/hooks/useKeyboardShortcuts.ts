import { useEffect, useRef } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';

/**
 * Maps the shell's destinations to keyboard shortcuts. There are two
 * now — the conversation and settings — because the capability
 * catalogues are settings sections, not pages (D34).
 *
 * Besides the `g` prefix, this hook owns the single-key shell chords
 * from design D30: `:` opens the command palette, `/` focuses the
 * search for the current mode, and `Esc` leaves the settings surface.
 * `?` lives in `<ShortcutHelp>` because it owns that panel's state.
 *
 * The convention is "g + <letter>" (a Vim / GitHub-style
 * two-key prefix) so we never steal single keys from form
 * inputs. The first `g` starts a 1-second window during which
 * the next key is consumed as a shortcut; if no second key
 * arrives, the `g` is dropped and the window closes.
 *
 * This hook is global: it lives at the App level so shortcuts
 * work from any page. Keystrokes aimed at a form element
 * (`<input>`, `<textarea>`, `<select>`, contenteditable) are
 * skipped via `isEditableTarget` below — the event still reaches
 * this window listener, so typing "go to chat" in the chat
 * textarea would otherwise end in a navigation jump.
 */

interface ShortcutEntry {
  key: string; // case-insensitive
  path: string;
}

const SHORTCUTS: ReadonlyArray<ShortcutEntry> = [
  { key: 'c', path: '/chat' },
  { key: 's', path: '/settings/general' },
];

const PREFIX_WINDOW_MS = 1_000;

/**
 * True when the event target is a control the user types into.
 *
 * Exported because every global key binding in the app has to
 * honour it: `g <key>` navigation here, and the `Ctrl/Cmd+B` /
 * `Ctrl/Cmd+K` chords in `useSidebarCollapse` / `CommandPalette`.
 * A binding that ignores this steals the keystroke from someone
 * typing a sentence, so the check lives in one place rather than
 * being re-derived per hook.
 */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!target || !(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (target.isContentEditable) return true;
  return false;
}

/**
 * Install a global keyboard listener for navigation shortcuts.
 * Returns nothing — the hook is fire-and-forget at App mount.
 *
 * Implementation note: the `g` prefix is a small state machine
 * stored in a ref so the hook doesn't trigger a re-render each
 * time the user presses `g`.
 */
export function useKeyboardShortcuts(): void {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const inSettings = pathname.startsWith('/settings');
  // Refs hold the timing state. A ref avoids the "stale
  // closure over a counter" trap and lets us reset cleanly on
  // each mount.
  const prefixExpiresAtRef = useRef<number>(0);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent): void {
      // Modifier-laden combos (Cmd/Ctrl/Alt + anything) are
      // reserved for the browser and OS. Skip.
      if (event.metaKey || event.ctrlKey || event.altKey) return;

      if (isEditableTarget(event.target)) return;

      // Single-key shell chords, before the `g` prefix machine so `g :`
      // cannot be swallowed as a navigation.
      if (event.key === ':') {
        event.preventDefault();
        window.dispatchEvent(new CustomEvent('uva:palette-open'));
        return;
      }
      if (event.key === '/') {
        event.preventDefault();
        window.dispatchEvent(
          new CustomEvent(inSettings ? 'uva:settings-search' : 'uva:search-open'),
        );
        return;
      }
      if (event.key === 'Escape' && inSettings) {
        // A dialog's Escape handler stops propagation before this
        // window listener runs, so an open panel keeps precedence.
        navigate('/chat');
        return;
      }

      const now = Date.now();
      const inPrefixWindow = now < prefixExpiresAtRef.current;

      // Reset window if it expired.
      if (!inPrefixWindow) {
        prefixExpiresAtRef.current = 0;
      }

      if (event.key === 'g' && !inPrefixWindow) {
        // Open a 1-second window for the next key.
        prefixExpiresAtRef.current = now + PREFIX_WINDOW_MS;
        return;
      }

      if (inPrefixWindow) {
        const entry = SHORTCUTS.find((s) => s.key === event.key.toLowerCase());
        if (entry) {
          event.preventDefault();
          navigate(entry.path);
        }
        // Either way, close the window — we only act once per
        // prefix to avoid `g g c` chaining into two nav jumps.
        prefixExpiresAtRef.current = 0;
      }
    }

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [navigate, inSettings]);
}
