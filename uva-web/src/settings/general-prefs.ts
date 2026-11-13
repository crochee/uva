/**
 * The 通用 section's two local display preferences.
 *
 * Density and base font size are *local UI* rows, not profile rows:
 * they change how this browser draws the shell and are stored beside
 * `uva.theme` / `uva.sidebarCollapsed` under the same
 * `uva.` prefix, so one sweep can find every key this app owns.
 *
 * Design D26 says the shell's own UI rows live in the profile
 * eventually; until the shell row's `config` is wired, `localStorage`
 * is the honest home. The `initDisplayPrefs` call in `main.tsx` applies
 * the stored values before React mounts, exactly like `initTheme`, so
 * the first paint uses the chosen density and size.
 */
import { useEffect, useState } from 'react';

export type Density = 'compact' | 'comfortable';

const DENSITY_KEY = 'uva.density';
const FONT_SIZE_KEY = 'uva.fontSize';

/** The sizes the picker offers; the middle one is the stylesheet
 *  default (`--fs-base: 14px`). */
export const FONT_SIZES: ReadonlyArray<number> = [13, 14, 15, 16];

const DEFAULT_DENSITY: Density = 'comfortable';
const DEFAULT_FONT_SIZE = 14;

function readDensity(): Density {
  try {
    return localStorage.getItem(DENSITY_KEY) === 'compact' ? 'compact' : DEFAULT_DENSITY;
  } catch {
    return DEFAULT_DENSITY;
  }
}

function readFontSize(): number {
  try {
    const raw = Number(localStorage.getItem(FONT_SIZE_KEY));
    if (FONT_SIZES.includes(raw)) return raw;
  } catch {
    // Storage unavailable — fall through to the default.
  }
  return DEFAULT_FONT_SIZE;
}

function applyDensity(density: Density): void {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  if (density === 'compact') root.setAttribute('data-density', 'compact');
  else root.removeAttribute('data-density');
}

function applyFontSize(size: number): void {
  if (typeof document === 'undefined') return;
  document.documentElement.style.setProperty('--fs-base', `${size}px`);
}

/** Apply both stored preferences. Called once from `main.tsx` before
 *  the first render, mirroring `initTheme`. */
export function initDisplayPrefs(): void {
  applyDensity(readDensity());
  applyFontSize(readFontSize());
}

export interface DisplayPrefs {
  density: Density;
  setDensity: (density: Density) => void;
  fontSize: number;
  setFontSize: (size: number) => void;
}

export function useDisplayPrefs(): DisplayPrefs {
  const [density, setDensityState] = useState<Density>(readDensity);
  const [fontSize, setFontSizeState] = useState<number>(readFontSize);

  useEffect(() => {
    applyDensity(density);
    try {
      localStorage.setItem(DENSITY_KEY, density);
    } catch {
      // Best-effort persistence; the in-memory value still applies.
    }
  }, [density]);

  useEffect(() => {
    applyFontSize(fontSize);
    try {
      localStorage.setItem(FONT_SIZE_KEY, String(fontSize));
    } catch {
      // Best-effort persistence.
    }
  }, [fontSize]);

  return {
    density,
    setDensity: setDensityState,
    fontSize,
    setFontSize: setFontSizeState,
  };
}
