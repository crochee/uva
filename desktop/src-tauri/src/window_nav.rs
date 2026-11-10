//! The window's URL decision when a new `Ready` state arrives.
//!
//! Pure on purpose: the supervisor's narration, the window handle,
//! and the threading all live in [`crate::run`]; what can be tested
//! without a webview is exactly this decision.

/// Whether a new `Ready` generation must move the window.
///
/// `current` is the URL the window shows (the shell's own
/// bookkeeping — webviews cannot be asked portably); `dev_url` is
/// this session's dev-server address, if any; `base_url` is the new
/// gateway address. `Some(url)` means "navigate there".
///
/// Rules, in order (design D2 and plan Review Focus #3):
///
/// 1. the window already shows `base_url` — nothing to do (the
///    supervisor reuses its port across restarts, so this is the
///    common case after a sidecar death);
/// 2. the window shows the dev server — leave it there: a port
///    change is absorbed by the Vite proxy and the front-end's own
///    reconnect, and yanking the page out from under HMR is worse;
/// 3. anything else (including "no window URL yet") — navigate.
pub fn next_window_url(
    current: Option<&str>,
    dev_url: Option<&str>,
    base_url: &str,
) -> Option<String> {
    if current == Some(base_url) {
        return None;
    }
    if let (Some(current), Some(dev)) = (current, dev_url) {
        if current == dev {
            return None;
        }
    }
    Some(base_url.to_owned())
}
