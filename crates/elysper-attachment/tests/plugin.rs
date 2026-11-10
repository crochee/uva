//! The `attachments` service as a native kernel plugin.
//!
//! The plugin builds its [`AttachmentStore`] from
//! [`AttachmentPluginConfig`] inside `apply`. These tests mount the
//! plugin over a temporary root and prove the resolved service stores
//! and returns bytes, stamps entries from the configured clock, that an
//! image (and an audio blob) round-trips through it into a provider
//! `ContentPart` carrying the original bytes, that a refused root
//! degrades to the in-memory store, and that unloading withdraws the
//! service a multimodal consumer depends on.

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use elysper_attachment::{
    AttachmentPluginConfig,
    AttachmentStore,
    attachment_to_content_part,
    plugin::{AttachmentPlugin, KEY_ATTACHMENTS},
    sha256_hex,
};
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_provider::{
    ContentPart,
    ImageDetail,
    types::{AudioContent, AudioFormat},
};

fn mount_attachments(
    ctx: &Context,
    dir: &std::path::Path,
) -> elysper_kernel::MountHandle {
    mount_attachments_with(
        ctx,
        AttachmentPluginConfig {
            dir: dir.to_path_buf(),
            clock: None,
        },
    )
}

fn mount_attachments_with(
    ctx: &Context,
    config: AttachmentPluginConfig,
) -> elysper_kernel::MountHandle {
    mount_all(
        ctx,
        &[Arc::new(AttachmentPlugin::new(config)) as Arc<dyn KernelPlugin>],
    )
    .expect("mount `attachments`")
}

#[test]
fn the_configured_clock_stamps_saved_entries() {
    let root = tempfile::tempdir().expect("a temporary root");
    let ctx = Context::new();
    let clock =
        elysper_core::SharedClock::fixed_from_rfc3339("2026-01-02T03:04:05Z");
    let mounted = mount_attachments_with(
        &ctx,
        AttachmentPluginConfig {
            dir: root.path().to_path_buf(),
            clock: Some(clock),
        },
    );

    let store = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");
    let ref_ = store
        .save_image(b"\x89PNG\r\n\x1a\nclocked", "image/png")
        .expect("the store accepts a png");

    assert_eq!(
        ref_.saved_at.to_rfc3339(),
        "2026-01-02T03:04:05+00:00",
        "the configured clock, not the wall clock, stamps the entry"
    );

    drop(mounted);
    assert!(ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_err());
}

#[test]
fn provides_a_working_store_and_unload_removes_it() {
    let root = tempfile::tempdir().expect("a temporary root");
    let ctx = Context::new();
    let mounted = mount_attachments(&ctx, root.path());

    let service = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");

    // The service is a functioning store, not merely a registered value.
    let bytes = b"\x89PNG\r\n\x1a\nelysper service probe";
    let ref_ = service
        .save_image(bytes, "image/png")
        .expect("the service saves a png");
    assert_eq!(ref_.hash, sha256_hex(bytes));
    assert_eq!(
        service
            .read_image(&ref_)
            .expect("the service returns the bytes"),
        bytes
    );

    drop(mounted);
    assert!(
        ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_err(),
        "unload must withdraw `attachments`"
    );
}

#[test]
fn image_part_roundtrips() {
    let root = tempfile::tempdir().expect("a temporary root");
    let ctx = Context::new();
    let mounted = mount_attachments(&ctx, root.path());

    let service = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");

    // The store validates the MIME allow-list, not the image codec, so
    // arbitrary bytes exercise the content-addressing path.
    let bytes = b"\x89PNG\r\n\x1a\nelysper image bytes";
    let ref_ = service
        .save_image(bytes, "image/png")
        .expect("the store accepts a png");
    assert_eq!(ref_.hash, sha256_hex(bytes));

    let inline = service.read_base64(&ref_).expect("the ref reads back");
    let part =
        attachment_to_content_part(&ref_, &inline, Some(ImageDetail::High));
    let ContentPart::Image(image) = part else {
        panic!("the adapter must produce an image part");
    };
    assert_eq!(image.mime_type, "image/png");
    assert_eq!(image.detail, Some(ImageDetail::High));
    assert_eq!(
        STANDARD
            .decode(image.data.as_bytes())
            .expect("the part carries base64"),
        bytes,
        "the provider block carries the stored bytes unchanged"
    );
    assert_eq!(
        service.read_image(&ref_).expect("the bytes read back"),
        bytes
    );

    drop(mounted);
}

#[test]
fn audio_blob_roundtrips_through_the_mounted_service() {
    let root = tempfile::tempdir().expect("a temporary root");
    let ctx = Context::new();
    let mounted = mount_attachments(&ctx, root.path());
    let store = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");

    let bytes = b"RIFF\x24\x00\x00\x00WAVEfmt ";
    let ref_ = store
        .save_blob(bytes, "audio/wav")
        .expect("the store accepts an audio blob");

    let inline = store.read_base64(&ref_).expect("the ref reads back");
    let part = ContentPart::Audio(AudioContent {
        data: inline,
        mime_type: ref_.mime_type.clone(),
        format: Some(AudioFormat::Wav),
    });
    let ContentPart::Audio(audio) = part else {
        panic!("the test built an audio part");
    };
    assert_eq!(audio.mime_type, "audio/wav");
    assert_eq!(audio.format, Some(AudioFormat::Wav));
    assert_eq!(
        STANDARD
            .decode(audio.data.as_bytes())
            .expect("the part carries base64"),
        bytes,
        "the provider block carries the stored bytes unchanged"
    );

    drop(mounted);
}

#[test]
fn the_configured_directory_is_the_storage_root() {
    let root = tempfile::tempdir().expect("a temporary root");
    let bytes = b"\x89PNG\r\n\x1a\npersisted across mounts";

    let saved = {
        let ctx = Context::new();
        let _mounted = mount_attachments(&ctx, root.path());
        let store = ctx
            .require::<AttachmentStore>(KEY_ATTACHMENTS)
            .expect("`attachments` resolves");
        store
            .save_image(bytes, "image/png")
            .expect("the store accepts a png")
    };

    // A fresh plugin over the same root sees the bytes the first mount
    // wrote, so `apply` really rooted the store at the configured path.
    let ctx = Context::new();
    let _mounted = mount_attachments(&ctx, root.path());
    let store = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");
    assert_eq!(
        store
            .read_image(&saved)
            .expect("the remounted store reads back"),
        bytes
    );
}

#[test]
fn a_refused_root_degrades_to_the_in_memory_store() {
    let root = tempfile::tempdir().expect("a temporary root");
    // A path that is a file, not a directory: `new_fs` refuses it.
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").expect("write the blocker");

    let ctx = Context::new();
    let mounted = mount_attachments(&ctx, &blocked);
    let store = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("the mount degrades instead of failing");

    let bytes = b"\x89PNG\r\n\x1a\ndegraded to memory";
    let ref_ = store
        .save_image(bytes, "image/png")
        .expect("the in-memory fallback stores");
    assert_eq!(
        store.read_image(&ref_).expect("the fallback reads back"),
        bytes
    );

    drop(mounted);
}

#[test]
fn unload_withdraws_the_service_multimodal_input_depends_on() {
    let root = tempfile::tempdir().expect("a temporary root");
    let ctx = Context::new();
    let mounted = mount_attachments(&ctx, root.path());
    let image_part = ContentPart::Image(elysper_provider::ImageContent {
        data: "aW1hZ2U=".to_string(),
        mime_type: "image/png".to_string(),
        detail: None,
    });

    // A consumer that admits a message carrying image parts needs the
    // service; with the plugin mounted, that read succeeds.
    assert!(matches!(image_part, ContentPart::Image(..)));
    assert!(ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_ok());

    drop(mounted);

    assert!(
        ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_err(),
        "an image-carrying message has no attachment service to resolve"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = AttachmentPlugin::new(AttachmentPluginConfig {
        dir: std::path::PathBuf::from("/tmp/attachments-manifest-probe"),
        clock: None,
    });
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "attachments");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_ATTACHMENTS.to_string()]);
    assert_eq!(KEY_ATTACHMENTS, "attachments");
    assert!(
        manifest.inject.is_empty(),
        "the store is built in apply; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
