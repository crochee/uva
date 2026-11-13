import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { initTheme } from './hooks/useTheme';
import { initDisplayPrefs } from './settings/general-prefs';
import '@radix-ui/themes/styles.css';
import './styles/fonts.css';
import './styles/tokens.css';
import './index.css';

// Apply the saved theme before React mounts so the first
// paint already reflects the user's preference. Skipping this
// would flash a light surface for users who picked dark.
initTheme();
// Density / font size ride the same rule: stamp the stored values
// before the first frame so the shell does not reflow on mount.
initDisplayPrefs();

const root = document.getElementById('root');
if (!root) throw new Error('Root element #root not found');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
