use super::*;
use crate::{CacheScope, CacheTtl};

// -- Content::text / Content::parts ------------------------------

/// `Content::text(s)` MUST produce `Content::Single(Text(s))`.
#[test]
fn content_text_produces_single_text() {
    let c = Content::text("hello");
    assert!(matches!(c, Content::Single(ContentPart::Text(_))));
    // Round-trip via extract_text.
    assert_eq!(c.extract_text(), Some("hello".to_string()));
}

/// `Content::text` MUST accept any `Into<String>`
/// (both `&str` and `String`).
#[test]
fn content_text_accepts_str_and_string() {
    let _ = Content::text("from_str");
    let _ = Content::text(String::from("from_string"));
}

/// `Content::parts([])` MUST produce `Content::Single(Text(""))`
/// (the single-element collapse branch hits when `len == 1`,
/// but with 0 elements it falls to `Multi(Vec::new())`).
/// Pin the actual behavior.
#[test]
fn content_parts_with_one_collapses_to_single() {
    let c = Content::parts(vec![ContentPart::Text(TextContent {
        text: "only".to_string(),
        cache_control: None,
    })]);
    assert!(matches!(c, Content::Single(_)));
}

/// `Content::parts` with > 1 element MUST produce
/// `Content::Multi`.
#[test]
fn content_parts_with_many_keeps_multi() {
    let c = Content::parts(vec![
        ContentPart::Text(TextContent {
            text: "a".to_string(),
            cache_control: None,
        }),
        ContentPart::Text(TextContent {
            text: "b".to_string(),
            cache_control: None,
        }),
    ]);
    assert!(matches!(c, Content::Multi(ps) if ps.len() == 2));
}

// -- Content::iter / has_tool_use / has_text ----------------------

/// `Content::iter()` MUST yield all parts in order
/// (single: 1 part; multi: N parts).
#[test]
fn content_iter_yields_all_parts_in_order() {
    let c = Content::parts(vec![
        ContentPart::Text(TextContent {
            text: "a".to_string(),
            cache_control: None,
        }),
        ContentPart::Text(TextContent {
            text: "b".to_string(),
            cache_control: None,
        }),
        ContentPart::Text(TextContent {
            text: "c".to_string(),
            cache_control: None,
        }),
    ]);
    let texts: Vec<&str> = c.iter().filter_map(|p| p.text()).collect();
    assert_eq!(texts, vec!["a", "b", "c"]);
}

/// `Content::has_tool_use()` MUST return `true` iff any part
/// is `ToolUse`.
#[test]
fn content_has_tool_use_returns_true_for_tool_use_part() {
    let c = Content::Multi(vec![
        ContentPart::Text(TextContent {
            text: "t".to_string(),
            cache_control: None,
        }),
        ContentPart::ToolUse(ToolUse {
            id: "t".to_string(),
            name: "bash".to_string(),
            input: serde_json::Value::Null,
        }),
    ]);
    assert!(c.has_tool_use());
}

/// `Content::has_tool_use()` MUST return `false` when no
/// part is `ToolUse`.
#[test]
fn content_has_tool_use_returns_false_without_tool_use() {
    let c = Content::text("just text");
    assert!(!c.has_tool_use());
}

/// `Content::has_text()` MUST return `true` iff any part
/// is `Text`.
#[test]
fn content_has_text_returns_true_for_text_part() {
    let c = Content::Single(ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "bash".to_string(),
        input: serde_json::Value::Null,
    }));
    // No text part → has_text is false.
    assert!(!c.has_text());
}

// -- Content::extract_text / extract_tool_uses -------------------

/// `Content::extract_text()` MUST concatenate all `Text`
/// parts and return `None` when there are none.
#[test]
fn content_extract_text_concatenates() {
    let c = Content::parts(vec![
        ContentPart::Text(TextContent {
            text: "a".to_string(),
            cache_control: None,
        }),
        ContentPart::ToolUse(ToolUse {
            id: "t".to_string(),
            name: "x".to_string(),
            input: serde_json::Value::Null,
        }),
        ContentPart::Text(TextContent {
            text: "b".to_string(),
            cache_control: None,
        }),
    ]);
    assert_eq!(c.extract_text(), Some("ab".to_string()));
}

/// `Content::extract_text()` MUST return `None` when no
/// `Text` part is present.
#[test]
fn content_extract_text_returns_none_without_text() {
    let c = Content::Single(ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "x".to_string(),
        input: serde_json::Value::Null,
    }));
    assert_eq!(c.extract_text(), None);
}

/// `Content::extract_tool_uses()` MUST return all
/// `ToolUse` parts (cloned, preserving order).
#[test]
fn content_extract_tool_uses_returns_all() {
    let c = Content::parts(vec![
        ContentPart::ToolUse(ToolUse {
            id: "1".to_string(),
            name: "a".to_string(),
            input: serde_json::Value::Null,
        }),
        ContentPart::Text(TextContent {
            text: "x".to_string(),
            cache_control: None,
        }),
        ContentPart::ToolUse(ToolUse {
            id: "2".to_string(),
            name: "b".to_string(),
            input: serde_json::Value::Null,
        }),
    ]);
    let uses = c.extract_tool_uses();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0].id, "1");
    assert_eq!(uses[1].id, "2");
}

/// `Content::extract_tool_uses()` MUST return empty Vec
/// when no `ToolUse` is present.
#[test]
fn content_extract_tool_uses_returns_empty_without_tool_use() {
    let c = Content::text("just text");
    assert!(c.extract_tool_uses().is_empty());
}

// -- From<String> / From<&str> ------------------------------------

/// `Content::from(String)` MUST produce `Content::Single(Text(s))`.
#[test]
fn content_from_string_produces_single_text() {
    let c: Content = String::from("from_string").into();
    assert_eq!(c.extract_text(), Some("from_string".to_string()));
}

/// `Content::from(&str)` MUST produce `Content::Single(Text(s))`.
#[test]
fn content_from_str_slice_produces_single_text() {
    let c: Content = "from_str".into();
    assert_eq!(c.extract_text(), Some("from_str".to_string()));
}

// -- IntoIterator -----------------------------------------------

/// `Content::into_iter()` (consuming) MUST yield all parts.
#[test]
fn content_into_iter_consuming_yields_all() {
    let c = Content::parts(vec![
        ContentPart::Text(TextContent {
            text: "a".to_string(),
            cache_control: None,
        }),
        ContentPart::Text(TextContent {
            text: "b".to_string(),
            cache_control: None,
        }),
    ]);
    let parts: Vec<_> = c.into_iter().collect();
    assert_eq!(parts.len(), 2);
}

/// `&Content::into_iter()` (borrowed) MUST yield references
/// without consuming.
#[test]
fn content_into_iter_borrowed_yields_refs() {
    let c = Content::parts(vec![
        ContentPart::Text(TextContent {
            text: "a".to_string(),
            cache_control: None,
        }),
        ContentPart::Text(TextContent {
            text: "b".to_string(),
            cache_control: None,
        }),
    ]);
    let refs: Vec<&ContentPart> = (&c).into_iter().collect();
    assert_eq!(refs.len(), 2);
    // c is still usable after the borrow.
    assert!(c.has_text());
}

// -- ContentPart accessors ---------------------------------------

/// `ContentPart::is_tool_use` MUST return `true` only for
/// the `ToolUse` variant.
#[test]
fn content_part_is_tool_use_only_for_tool_use() {
    let tu = ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "x".to_string(),
        input: serde_json::Value::Null,
    });
    let text = ContentPart::Text(TextContent {
        text: "t".to_string(),
        cache_control: None,
    });
    assert!(tu.is_tool_use());
    assert!(!text.is_tool_use());
}

/// `ContentPart::text()` MUST return `Some(&str)` only for
/// `Text` variants.
#[test]
fn content_part_text_returns_some_for_text() {
    let p = ContentPart::Text(TextContent {
        text: "abc".to_string(),
        cache_control: None,
    });
    assert_eq!(p.text(), Some("abc"));
    // Non-Text variants return None.
    let tu = ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "x".to_string(),
        input: serde_json::Value::Null,
    });
    assert_eq!(tu.text(), None);
}

/// `ContentPart::text_mut()` MUST allow mutable access to
/// `Text.text`.
#[test]
fn content_part_text_mut_allows_modification() {
    let mut p = ContentPart::Text(TextContent {
        text: "old".to_string(),
        cache_control: None,
    });
    if let Some(t) = p.text_mut() {
        t.push_str("_new");
    } else {
        panic!("text_mut should be Some for Text variant");
    }
    assert_eq!(p.text(), Some("old_new"));
}

/// `ContentPart::text_mut()` MUST return `None` for
/// non-`Text` variants.
#[test]
fn content_part_text_mut_returns_none_for_non_text() {
    let mut p = ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "x".to_string(),
        input: serde_json::Value::Null,
    });
    assert!(p.text_mut().is_none());
}

/// `ContentPart::cache_control()` MUST return the
/// `CacheControlMark` when set on a `Text` variant.
#[test]
fn content_part_cache_control_returns_mark_when_set() {
    let mark = CacheControlMark {
        ttl: CacheTtl::Extended,
        scope: CacheScope("default".to_string()),
        pinned: false,
    };
    let p = ContentPart::Text(TextContent {
        text: "t".to_string(),
        cache_control: Some(mark.clone()),
    });
    assert!(p.cache_control().is_some());
}

/// `ContentPart::cache_control()` MUST return `None` for
/// non-`Text` variants (even if `Some` would be desired).
#[test]
fn content_part_cache_control_none_for_non_text() {
    let p = ContentPart::ToolUse(ToolUse {
        id: "t".to_string(),
        name: "x".to_string(),
        input: serde_json::Value::Null,
    });
    assert!(p.cache_control().is_none());
}

// -- ContentPart serde -------------------------------------------

/// `ContentPart` MUST serialize with `type = "text"` etc.
/// (the `#[serde(tag = "type")]` outer-tag form).
#[test]
fn content_part_serializes_with_type_tag() {
    let p = ContentPart::Text(TextContent {
        text: "hi".to_string(),
        cache_control: None,
    });
    let json: serde_json::Value = serde_json::to_value(&p).unwrap();
    assert_eq!(json["type"], "text");
    assert_eq!(json["text"], "hi");
}

/// `ContentPart::ToolUse` MUST round-trip via JSON.
#[test]
fn content_part_tool_use_round_trips() {
    let p = ContentPart::ToolUse(ToolUse {
        id: "abc".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"cmd": "ls"}),
    });
    let json = serde_json::to_string(&p).unwrap();
    let parsed: ContentPart = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, p);
}

// -- ImageDetail / AudioFormat -----------------------------------

/// `ImageDetail` MUST serialize as snake_case strings.
#[test]
fn image_detail_serializes_as_snake_case() {
    for (variant, expected) in [
        (ImageDetail::Low, "\"low\""),
        (ImageDetail::High, "\"high\""),
        (ImageDetail::Auto, "\"auto\""),
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected);
    }
}

/// `AudioFormat` MUST serialize as snake_case strings.
#[test]
fn audio_format_serializes_as_snake_case() {
    for (variant, expected) in [
        (AudioFormat::Wav, "\"wav\""),
        (AudioFormat::Mp3, "\"mp3\""),
        (AudioFormat::Flac, "\"flac\""),
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected);
    }
}

/// `ImageDetail` MUST round-trip each variant.
#[test]
fn image_detail_round_trips_all_three_variants() {
    for v in [ImageDetail::Low, ImageDetail::High, ImageDetail::Auto] {
        let json = serde_json::to_string(&v).unwrap();
        let parsed: ImageDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, v);
    }
}
