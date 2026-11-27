//! The session messenger: how one session's message reaches another.
//!
//! The `session_send_message` tool holds a
//! [`SessionMessenger`](uva::tool_session_message::SessionMessenger)
//! seam; this module is the host's side of it. The delivery rides the
//! same path a host's steer rides: the target's controller takes a
//! live run's inbox at its next step boundary, or queues the message
//! as the target's next turn — and the message arrives **attributed**
//! (dsh's own shape: "Session X sent a message: …"), so the recipient
//! knows who was talking.
//!
//! The messenger closes over the deployment's state, which exists
//! only after the kernel mount — so the boot hands the tools a slot
//! and fills it the moment the state is assembled (the same
//! hand-off the `sessions` service uses).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use uva::tool_session_message::{Delivery, SessionMessenger};

use crate::state::AppState;

/// The deployment's messenger, resolved once the state exists.
pub struct SessionMessages {
    state: OnceLock<Arc<AppState>>,
}

impl std::fmt::Debug for SessionMessages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionMessages")
            .field("resolved", &self.state.get().is_some())
            .finish_non_exhaustive()
    }
}

impl SessionMessages {
    /// The messenger over the slot the boot fills after the mount.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: OnceLock::new(),
        }
    }

    /// Resolve the messenger over the deployment's state.
    ///
    /// # Errors
    ///
    /// The state is not assembled yet — a call this early cannot
    /// reach any session.
    pub fn resolve(&self) -> Result<Arc<AppState>, String> {
        self.state
            .get()
            .cloned()
            .ok_or_else(|| "the deployment is still assembling".to_string())
    }

    /// Fill the slot; the boot calls this once the state exists.
    pub fn set(&self, state: Arc<AppState>) {
        let _ = self.state.set(state);
    }
}

impl Default for SessionMessages {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionMessenger for SessionMessages {
    async fn send(
        &self,
        caller: &str,
        target: &str,
        message: &str,
    ) -> Result<Delivery, String> {
        let state = self.resolve()?;
        // A typo must not mint a session: the target has to exist the
        // same way a host's read of it would require.
        let user = state.default_user_id().to_string();
        let controller = state
            .must_exist_session_controller(&user, target)
            .await
            .map_err(|error| match error {
                crate::state::MustExistError::Absent { session_id } => {
                    format!("the session '{session_id}' is not known")
                }
                crate::state::MustExistError::Open(error) => {
                    format!(
                        "the session '{target}' could not be opened: {error}"
                    )
                }
            })?;
        // dsh's attribution: the recipient is told who sent it, in
        // the message itself.
        let attributed = format!("Session {caller} sent a message: {message}");
        let delivery = controller
            .steer(attributed, None)
            .await
            .map_err(|error| error.to_string())?;
        Ok(match delivery {
            crate::session::controller::SteerDelivery::Steer => Delivery::Steer,
            crate::session::controller::SteerDelivery::Queue => {
                Delivery::Queued
            }
        })
    }
}
