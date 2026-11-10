import { Suspense, lazy } from 'react';
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom';
import { Theme, Box } from '@radix-ui/themes';
import { MainLayout } from './components/layout/MainLayout';
import { ErrorBoundary } from './components/layout/ErrorBoundary';
import { useServerHealth } from './hooks/useServerHealth';
import { ToastProvider } from './hooks/useToast';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useTheme } from './hooks/useTheme';
import { SkeletonList } from './components/ui/SkeletonList';
import './styles/page.css';
import './styles/page-visual.css';

// The router tree is conversation-first (design §6.1): three routes and
// a catch-all. The settings surface is the only lazy chunk — the
// capability catalogues that used to be pages are sections inside it
// (D34), and ChatPage stays eager because it is the default route.
const SettingsPage = lazy(() =>
  import('./pages/SettingsPage').then((m) => ({ default: m.SettingsPage })),
);

// Eager ChatPage import — kept below the lazy declaration so the
// lazy references stay top-of-file for code-review visibility, but ES
// modules hoist `import` statements above any runtime code regardless
// of textual position.
import { ChatPage } from './pages/ChatPage';

/**
 * Top-level application component.
 *
 * Wires the React Router tree under the main layout
 * (header + sidebar + outlet) and feeds the layout the
 * live server-health status from `useServerHealth`.
 *
 * The router tree is wrapped in an `<ErrorBoundary>` so a
 * render-phase throw from any page surfaces as a recoverable
 * fallback instead of a blank screen. `<ToastProvider>` lives
 * inside the boundary so toasts themselves can never crash the
 * tree.
 */
export default function App() {
  const isServerAvailable = useServerHealth();
  const { resolvedAppearance } = useTheme();
  // `useKeyboardShortcuts` itself is wired inside
  // `<RouterShell>` (below) — it calls `useNavigate`, which
  // requires a `<Router>` ancestor. Calling it from `App()`
  // would throw on every render because `App()` is the
  // *parent* of `<BrowserRouter>`, not a descendant.

  return (
    <Theme
      appearance={resolvedAppearance}
      accentColor="blue"
      grayColor="slate"
      radius="medium"
      scaling="100%"
    >
      <ErrorBoundary>
        <ToastProvider>
          <BrowserRouter>
            <RouterShell isServerAvailable={isServerAvailable} />
          </BrowserRouter>
        </ToastProvider>
      </ErrorBoundary>
    </Theme>
  );
}

/**
 * Lives inside `<BrowserRouter>` so hooks that depend on the
 * router context (`useNavigate`, `useLocation`, etc.) can run
 * during render.
 *
 * Every removed catalogue page lands on the catch-all redirect: a
 * bookmarked `/tools` or a stale in-app link opens the conversation
 * instead of a dead route.
 */
function RouterShell({ isServerAvailable }: { isServerAvailable: boolean }) {
  useKeyboardShortcuts();
  return (
    <Routes>
      <Route element={<MainLayout isServerAvailable={isServerAvailable} />}>
        <Route path="/" element={<Navigate to="/chat" replace />} />
        <Route path="/chat" element={<ChatPage />} />
        <Route path="/chat/:sessionId" element={<ChatPage />} />
        <Route path="/chat/:sessionId/agent/:agentName" element={<ChatPage />} />
        <Route path="/settings" element={<Navigate to="/settings/general" replace />} />
        <Route
          path="/settings/:section"
          element={
            <Suspense fallback={<RouteFallback testId="page-fallback-settings" />}>
              <SettingsPage />
            </Suspense>
          }
        />
        <Route path="*" element={<Navigate to="/chat" replace />} />
      </Route>
    </Routes>
  );
}

/**
 * Shown inside `<Suspense>` while the settings chunk is in flight.
 * Re-uses `<SkeletonList>` so the fallback looks like the eventual
 * content (a list of card placeholders) — no surprise layout shift when
 * the chunk resolves.
 */
function RouteFallback({ testId }: { testId: string }) {
  return (
    <Box px="4" py="3" data-testid={testId}>
      <SkeletonList count={3} />
    </Box>
  );
}
