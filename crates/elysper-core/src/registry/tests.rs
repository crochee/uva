use async_trait::async_trait;
use parking_lot::RwLock;

use super::*;
use crate::error::Error;

/// A minimal item used by the mock registry.
#[derive(Clone, Debug, PartialEq)]
struct MockItem {
    name: String,
    description: String,
}

impl MockItem {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            description: format!("desc for {name}"),
        }
    }
}

impl RegistryItem for MockItem {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }
}

/// An in-memory mock registry backed by a `Vec` to preserve
/// insertion order (so tests have a stable, predictable ordering
/// without relying on the default impl's sort).
struct MockRegistry {
    items: RwLock<Vec<MockItem>>,
}

impl MockRegistry {
    fn new(items: Vec<MockItem>) -> Self {
        Self {
            items: RwLock::new(items),
        }
    }
}

#[async_trait]
impl Registry for MockRegistry {
    type Filter = EmptyFilter;
    type Item = MockItem;

    async fn put(&self, item: MockItem) -> Result<(), Error> {
        let mut items = self.items.write();
        if items.iter().any(|i| i.name == item.name) {
            return Err(Error::already_exists(item.name.clone()));
        }
        items.push(item);
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<(), Error> {
        let mut items = self.items.write();
        let before = items.len();
        items.retain(|i| i.name != name);
        if items.len() == before {
            return Err(Error::not_found(name));
        }
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<MockItem>, Error> {
        Ok(self.items.read().iter().find(|i| i.name == name).cloned())
    }

    async fn list_paginate(
        &self,
        cursor: Option<String>,
        limit: u64,
        _sort: Option<String>,
        _filter: Option<Self::Filter>,
    ) -> Result<RegistryList<MockItem>, Error> {
        let items = self.items.read().clone();
        // Sort is intentionally ignored — tests rely on
        // insertion order for stable assertions.
        paginate_registry_list(items, cursor.as_deref(), limit)
    }
}

fn items_named(names: &[&str]) -> Vec<MockItem> {
    names.iter().map(|n| MockItem::new(n)).collect()
}

// --- put / delete / get ---

#[tokio::test]
async fn put_inserts_and_get_returns_clone() {
    let reg = MockRegistry::new(Vec::new());
    reg.put(MockItem::new("a")).await.unwrap();
    let got = reg.get("a").await.unwrap();
    assert_eq!(got, Some(MockItem::new("a")));
}

#[tokio::test]
async fn put_duplicate_returns_error() {
    let reg = MockRegistry::new(items_named(&["a"]));
    let err = reg.put(MockItem::new("a")).await.unwrap_err();
    assert!(matches!(err, Error::AlreadyExists { .. }), "got {err:?}");
}

#[tokio::test]
async fn delete_removes_and_unknown_returns_error() {
    let reg = MockRegistry::new(items_named(&["a", "b"]));
    reg.delete("a").await.unwrap();
    assert!(reg.get("a").await.unwrap().is_none());
    let err = reg.delete("nope").await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "got {err:?}");
}

// --- first page ---

#[tokio::test]
async fn first_page_returns_limit_items_with_cursor() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));
    let page = reg.list_paginate(None, 2, None, None).await.unwrap();
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].name, "a");
    assert_eq!(page.data[1].name, "b");
    assert!(page.next_cursor.is_some());
    assert_eq!(page.total, Some(5));
}

// --- middle page ---

#[tokio::test]
async fn middle_page_resumes_after_cursor_id() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));
    // Cursor encodes "b" → resume at "c"
    let cursor = encode_registry_cursor("b");
    let page = reg
        .list_paginate(Some(cursor), 2, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].name, "c");
    assert_eq!(page.data[1].name, "d");
    assert!(page.next_cursor.is_some());
    // next_cursor should encode "d"
    let next_id =
        decode_registry_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
    assert_eq!(next_id, "d");
    assert_eq!(page.total, Some(5));
}

// --- last page ---

#[tokio::test]
async fn last_page_returns_remaining_with_no_cursor() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));
    // Cursor encodes "d" → resume at "e" (only 1 item left)
    let cursor = encode_registry_cursor("d");
    let page = reg
        .list_paginate(Some(cursor), 10, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].name, "e");
    assert!(page.next_cursor.is_none());
    assert_eq!(page.total, Some(5));
}

// --- past-end (cursor at last item, no more data) ---

#[tokio::test]
async fn cursor_at_last_item_returns_empty_page() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));
    // Cursor encodes "e" (the last item) → start_idx == len → empty
    let cursor = encode_registry_cursor("e");
    let page = reg
        .list_paginate(Some(cursor), 10, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 0);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.total, Some(5));
}

// --- deleted cursor (cursor ID no longer in list) ---

#[tokio::test]
async fn deleted_cursor_returns_empty_page_not_error() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c"]));
    // Cursor points to an ID that doesn't exist (deleted)
    let cursor = encode_registry_cursor("deleted_id");
    let page = reg
        .list_paginate(Some(cursor), 10, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 0);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.total, Some(3));
}

// --- invalid cursor (bad base64) ---

#[tokio::test]
async fn invalid_cursor_returns_invalid_item_error() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c"]));
    let err = reg
        .list_paginate(Some("not-valid-base64!!!".to_string()), 10, None, None)
        .await
        .unwrap_err();
    match err {
        Error::InvalidItem { item, .. } => {
            assert_eq!(item, "cursor");
        }
        other => panic!("expected Error::InvalidItem, got {other:?}"),
    }
}

// --- empty-string cursor treated as first page ---

#[tokio::test]
async fn empty_string_cursor_treated_as_first_page() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c"]));
    let page = reg
        .list_paginate(Some(String::new()), 2, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].name, "a");
    assert!(page.next_cursor.is_some());
}

// --- empty registry ---

#[tokio::test]
async fn empty_registry_returns_empty_page_no_cursor() {
    let reg = MockRegistry::new(Vec::new());
    let page = reg.list_paginate(None, 10, None, None).await.unwrap();
    assert_eq!(page.data.len(), 0);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.total, Some(0));
}

// --- limit boundaries ---

#[tokio::test]
async fn limit_zero_returns_validation_error() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c"]));
    let err = reg.list_paginate(None, 0, None, None).await.unwrap_err();
    match err {
        Error::Validation { message, .. } => {
            assert!(message.contains("limit"), "message = {message}");
        }
        other => panic!("expected Error::Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn limit_above_max_is_truncated() {
    // Build a registry with MAX_LIMIT + 5 items, ask
    // for a page larger than MAX_LIMIT, expect exactly
    // MAX_LIMIT back.
    let names: Vec<String> = (0..(MAX_LIMIT + 5))
        .map(|i| format!("item_{i:03}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let reg = MockRegistry::new(items_named(&refs));
    let page = reg
        .list_paginate(None, MAX_LIMIT + 50, None, None)
        .await
        .unwrap();
    assert_eq!(page.data.len() as u64, MAX_LIMIT);
    assert!(page.next_cursor.is_some());
    assert_eq!(page.total, Some(MAX_LIMIT + 5));
}

#[tokio::test]
async fn exactly_limit_items_yields_no_cursor() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c"]));
    let page = reg.list_paginate(None, 3, None, None).await.unwrap();
    assert_eq!(page.data.len(), 3);
    assert!(page.next_cursor.is_none());
}

#[tokio::test]
async fn one_more_than_limit_yields_cursor() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d"]));
    let page = reg.list_paginate(None, 3, None, None).await.unwrap();
    assert_eq!(page.data.len(), 3);
    assert!(page.next_cursor.is_some());
    let next_id =
        decode_registry_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
    assert_eq!(next_id, "c");
}

// --- walk a 5-item list page-by-page ---

#[tokio::test]
async fn walk_three_pages_then_terminate() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));

    // Page 1: a, b → next_cursor encodes "b"
    let p1 = reg.list_paginate(None, 2, None, None).await.unwrap();
    assert_eq!(
        p1.data.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(p1.next_cursor.is_some());

    // Page 2: c, d → next_cursor encodes "d"
    let p2 = reg
        .list_paginate(p1.next_cursor, 2, None, None)
        .await
        .unwrap();
    assert_eq!(
        p2.data.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["c", "d"]
    );
    assert!(p2.next_cursor.is_some());

    // Page 3: e → no next_cursor
    let p3 = reg
        .list_paginate(p2.next_cursor, 2, None, None)
        .await
        .unwrap();
    assert_eq!(
        p3.data.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["e"]
    );
    assert!(p3.next_cursor.is_none());
}

// --- sort parameter is ignored by default impl ---

#[tokio::test]
async fn sort_parameter_is_ignored_by_default_impl() {
    let reg = MockRegistry::new(items_named(&["b", "a", "c"]));
    // Default impl should return items in insertion order
    // regardless of the sort parameter.
    let page = reg
        .list_paginate(None, 10, Some("-name".to_string()), None)
        .await
        .unwrap();
    assert_eq!(
        page.data
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "a", "c"]
    );
}

// --- list default impl ---

#[tokio::test]
async fn list_default_returns_full_data_without_pagination() {
    let reg = MockRegistry::new(items_named(&["a", "b", "c", "d", "e"]));
    let all = reg.list(None).await.unwrap();
    assert_eq!(
        all.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d", "e"]
    );
}

#[tokio::test]
async fn list_default_on_empty_registry_returns_empty_vec() {
    let reg = MockRegistry::new(Vec::new());
    let all = reg.list(None).await.unwrap();
    assert!(all.is_empty());
}

// --- default start / stop lifecycle hooks ---

#[tokio::test]
async fn start_default_is_noop_returning_ok_for_lifecycle_free_registry() {
    // MockRegistry does NOT override `start`; the trait default
    // must be inherited and return Ok(()) for any name.
    let reg = MockRegistry::new(Vec::new());
    let result = Registry::start(&reg, "anything").await;
    assert!(result.is_ok(), "expected Ok(()), got {result:?}");
}

#[tokio::test]
async fn stop_default_is_noop_returning_ok_for_lifecycle_free_registry() {
    // MockRegistry does NOT override `stop`; the trait default
    // must be inherited and return Ok(()) for any name.
    let reg = MockRegistry::new(Vec::new());
    let result = Registry::stop(&reg, "anything").await;
    assert!(result.is_ok(), "expected Ok(()), got {result:?}");
}

// --- MAX_LIMIT spec pin ---

/// The page-size cap is `100` because the wire-level
/// pagination cap in `elysper-server` (which the server
/// aliases to this constant) was set to `100` long ago.
/// Pin the value here so a refactor that silently changed
/// it would fail this test, and the
/// `elysper-server::constants_match_spec` test would
/// follow on its own.
#[test]
fn max_limit_matches_spec() {
    assert_eq!(MAX_LIMIT, 100);
}
