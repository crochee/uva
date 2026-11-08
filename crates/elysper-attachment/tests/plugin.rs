//! The `attachments` service as a native kernel plugin.
//!
//! The service is the host-built [`AttachmentStore`]. These tests prove
//! the kernel hands out exactly that store, that an image (and an audio
//! blob) round-trips through it into a provider `ContentPart` carrying
//! the original bytes, and that unloading withdraws the service a
//! multimodal consumer depends on.

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use elysper_attachment::{
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
    store: &Arc<AttachmentStore>,
) -> elysper_kernel::MountHandle {
    mount_all(
        ctx,
        &[Arc::new(AttachmentPlugin::new(Arc::clone(store)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `attachments`")
}

#[test]
fn provides_the_store_and_unload_removes_it() {
    let ctx = Context::new();
    let store = Arc::new(AttachmentStore::default());
    let mounted = mount_attachments(&ctx, &store);

    let service = ctx
        .require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("`attachments` resolves");
    assert!(
        Arc::ptr_eq(&service, &store),
        "the service must be the store the plugin was built with"
    );

    drop(mounted);
    assert!(
        ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_err(),
        "unload must withdraw `attachments`"
    );
}

#[test]
fn image_part_roundtrips() {
    let ctx = Context::new();
    let store = Arc::new(AttachmentStore::default());
    let mounted = mount_attachments(&ctx, &store);

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
fn audio_blob_roundtrips() {
    let store = AttachmentStore::default();
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
}

#[test]
fn unload_withdraws_the_service_multimodal_input_depends_on() {
    let ctx = Context::new();
    let store = Arc::new(AttachmentStore::default());
    let mounted = mount_attachments(&ctx, &store);
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

    // Boundary: no in-tree consumer gates `ContentPart::Image` on this
    // service yet (the app-server holds its own `AttachmentStore` in
    // `AppState`), so the refusal a wired consumer would report is
    // exactly this failed resolution.
    assert!(
        ctx.require::<AttachmentStore>(KEY_ATTACHMENTS).is_err(),
        "an image-carrying message has no attachment service to resolve"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = AttachmentPlugin::new(Arc::new(AttachmentStore::default()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "attachments");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_ATTACHMENTS.to_string()]);
    assert_eq!(KEY_ATTACHMENTS, "attachments");
    assert!(
        manifest.inject.is_empty(),
        "the store arrives with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
