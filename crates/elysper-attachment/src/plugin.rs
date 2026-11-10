//! The `attachments` service: the content-addressed multimodal store as
//! a native kernel plugin.
//!
//! # The service is the store
//!
//! [`AttachmentPlugin`] builds the [`AttachmentStore`] from
//! [`AttachmentPluginConfig::dir`] and
//! [`AttachmentPluginConfig::clock`] inside
//! [`apply`](elysper_kernel::Plugin::apply) and publishes it under
//! [`KEY_ATTACHMENTS`]. An image or audio upload is saved by hash, its
//! [`ImageAttachmentRef`] travels in the message instead of inline
//! bytes, and the provider adapter renders it with
//! [`attachment_to_content_part`]
//! for `ContentPart::Image` / `ContentPart::Audio`. The plugin adds no
//! storage policy of its own: the store's backends, MIME allow-list,
//! size limit, and hash verification are what a consumer gets.
//!
//! # Tool output passes through untouched
//!
//! A multimodal-aware tool returns its media as the same provider
//! `ContentPart` variants; nothing in this plugin projects, truncates,
//! or re-encodes them. The text projection may be rewritten by the
//! caller while the media part stays byte-identical — the store is the
//! only thing that owns the bytes, and it verifies the hash on read.
//!
//! # The consumer boundary is the app-server's
//!
//! Nothing in this crate decides that a message *may* carry an image:
//! the session/HTTP layer that admits a request resolves the store from
//! the kernel, and that is where the rejection policy lives. The
//! plugin's contribution is the service seam, so that consumer holds no
//! `AppState` field of its own; unloading the plugin withdraws the
//! service.
//!
//! # The service value's shape
//!
//! [`AttachmentStore`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_ATTACHMENTS, Arc<AttachmentStore>)` and a
//! consumer writes `ctx.require::<AttachmentStore>(KEY_ATTACHMENTS)`.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service; stored bytes stay where the backend put them. The store
//! is built from [`AttachmentPluginConfig`] during `apply`, so the
//! manifest injects nothing.

use std::{path::PathBuf, sync::Arc};

use elysper_core::SharedClock;
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::AttachmentStore;

/// The service key the `attachments` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_ATTACHMENTS: &str = "attachments";

/// The plugin id the `attachments` manifest declares.
const ATTACHMENTS_ID: &str = "attachments";

/// What the `attachments` plugin builds its store from.
#[derive(Clone, Debug, Default)]
pub struct AttachmentPluginConfig {
    /// The root directory the filesystem-backed store writes under.
    ///
    /// `apply` creates the directory if absent. A root the store
    /// refuses — or an empty path, which has no root at all — degrades
    /// the plugin to an in-memory store rather than failing the mount.
    pub dir: PathBuf,
    /// The wall-clock source the store stamps `saved_at` from.
    ///
    /// `None` leaves the store on its default system clock; the boot
    /// hands over the deployment's shared clock so attachment
    /// timestamps agree with the rest of the app.
    pub clock: Option<SharedClock>,
}

/// The `attachments` kernel plugin: provides [`KEY_ATTACHMENTS`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the media pass-through and the consumer
/// boundary.
pub struct AttachmentPlugin {
    manifest: PluginManifest,
    config: AttachmentPluginConfig,
}

impl AttachmentPlugin {
    #[must_use]
    /// Build the plugin over the store root and clock `config` names.
    ///
    /// The store itself is constructed in
    /// [`apply`](elysper_kernel::Plugin::apply): the mount is the
    /// moment the deployment's configuration becomes the running
    /// service, including the wall clock its refs are stamped from.
    pub fn new(config: AttachmentPluginConfig) -> Self {
        let mut manifest =
            PluginManifest::new(ATTACHMENTS_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_ATTACHMENTS.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for AttachmentPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let store = if self.config.dir.as_os_str().is_empty() {
            AttachmentStore::default()
        } else {
            AttachmentStore::new_fs(&self.config.dir)
                .unwrap_or_else(|_| AttachmentStore::default())
        };
        let store = match &self.config.clock {
            Some(clock) => store.with_clock(clock.clone()),
            None => store,
        };
        Ok(vec![ctx.provide_arc(KEY_ATTACHMENTS, Arc::new(store))?])
    }
}
