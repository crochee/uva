//! The `uva_core::registry::Registry` impl for
//! [`ToolRegistry`](super::ToolRegistry).
//!
//! A module of its own (not `impl Registry` at the crate root) so
//! the `Registry` trait impl — and its `ToolFilter` associated
//! type — is not visible outside the crate. External callers
//! reach the same behavior through the inherent methods on
//! `ToolRegistry`.

use async_trait::async_trait;
use serde::Serialize;
use uva_core::registry::{Registry, RegistryItem, paginate_registry_list};

use super::*;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolFilter {
    pub name_prefix: Option<String>,
}

#[async_trait]
impl Registry for ToolRegistry {
    type Filter = ToolFilter;
    type Item = ToolEntry;

    async fn put(
        &self,
        item: Self::Item,
    ) -> std::result::Result<(), uva_core::Error> {
        let name = RegistryItem::name(&item).to_string();
        if self.register_entry(item) {
            Ok(())
        } else {
            // A Core tool occupies the name; report it instead of
            // silently dropping the registration.
            Err(uva_core::Error::already_exists(name))
        }
    }

    async fn delete(
        &self,
        name: &str,
    ) -> std::result::Result<(), uva_core::Error> {
        let mut inner = self.inner.write();
        let removed = inner.tools.remove(name).is_some();
        if removed {
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        } else {
            Err(uva_core::Error::not_found(name))
        }
    }

    async fn get(
        &self,
        name: &str,
    ) -> std::result::Result<Option<Self::Item>, uva_core::Error> {
        Ok(self.get(name))
    }

    async fn list_paginate(
        &self,
        cursor: Option<String>,
        limit: u64,
        _sort: Option<String>,
        filter: Option<Self::Filter>,
    ) -> std::result::Result<
        uva_core::registry::RegistryList<Self::Item>,
        uva_core::Error,
    > {
        let filter = filter.unwrap_or_default();
        let inner = self.inner.read();
        let result: Vec<ToolEntry> = inner
            .tools
            .values()
            .filter_map(|entries| entries.last())
            .filter(|entry| {
                let name = entry.tool.name();
                match &filter.name_prefix {
                    Some(prefix) => name.starts_with(prefix),
                    None => true,
                }
            })
            .map(entry_from_provider)
            .collect();
        // Sort is intentionally ignored — the registry stores
        // tools in registration order and the HTTP surface
        // doesn't expose sort yet. Cursor + limit + envelope
        // come from the shared pagination primitive.
        paginate_registry_list(result, cursor.as_deref(), limit)
    }
}
