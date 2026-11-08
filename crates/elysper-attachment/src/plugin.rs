//! The `attachments` service: the content-addressed multimodal store as
//! a native kernel plugin.
//!
//! # The service is the store
//!
//! [`AttachmentPlugin`] publishes the host-built [`AttachmentStore`]
//! under [`KEY_ATTACHMENTS`]. An image or audio upload is saved by hash,
//! its [`ImageAttachmentRef`] travels in the
//! message instead of inline bytes, and the provider adapter renders it
//! with [`attachment_to_content_part`]
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
//! the session/HTTP layer that admits a request holds the store and is
//! where the rejection policy lives. The plugin's contribution is the
//! service seam, so that consumer can resolve the store from the kernel
//! instead of an `AppState` field; until it does, unloading the plugin
//! withdraws the service without changing the already-booted app-server.
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
//! arrives with the plugin value, so the manifest injects nothing.

use std::sync::Arc;

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

/// The `attachments` kernel plugin: provides [`KEY_ATTACHMENTS`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the media pass-through and the consumer
/// boundary.
pub struct AttachmentPlugin {
    manifest: PluginManifest,
    /// The store the service hands out.
    ///
    /// Public because the host keeps the same handle for its HTTP
    /// routes; the service publishes exactly this value.
    pub store: Arc<AttachmentStore>,
}

impl AttachmentPlugin {
    /// Publish `store` as the `attachments` service.
    ///
    /// The host builds the store over its data root (or an in-memory
    /// backend for tests); the plugin only publishes it.
    pub fn new(store: Arc<AttachmentStore>) -> Self {
        let mut manifest =
            PluginManifest::new(ATTACHMENTS_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_ATTACHMENTS.to_string());
        Self { manifest, store }
    }
}

impl KernelPlugin for AttachmentPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide_arc(KEY_ATTACHMENTS, Arc::clone(&self.store))?,
        ])
    }
}
