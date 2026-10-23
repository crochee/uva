//! [`AgentRegistry`] - multi-agent catalog implementing [`synthia_core::registry::Registry`] - and [`AgentEntry`], the per-entry view it stores. The descriptor cache is write-once outside tests.
use std::sync::Arc;

use async_trait::async_trait;
use synthia_core::{
    agent::{AgentDescriptor, AgentFilter},
    error::Error,
    registry::{Registry, RegistryItem, paginate_registry_list},
};

use super::Agent;

/// The registry's view of one agent, built by [`AgentRegistry`] at registration.
#[derive(Clone)]
pub struct AgentEntry {
    agent: Arc<dyn Agent>,
    descriptor: AgentDescriptor,
}

impl std::fmt::Debug for AgentEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEntry")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl AgentEntry {
    #[must_use]
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        let descriptor = agent.descriptor().clone();
        Self { agent, descriptor }
    }

    #[must_use]
    pub fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    /// Test-only write handle for the cached descriptor: `#[cfg(test)] pub(crate)`,
    /// not `pub` — no consumer wants a write handle on a write-once cache.
    #[cfg(test)]
    pub(crate) fn descriptor_mut(&mut self) -> &mut AgentDescriptor {
        &mut self.descriptor
    }

    #[must_use]
    pub(crate) fn agent(&self) -> Arc<dyn Agent> {
        Arc::clone(&self.agent)
    }
}

impl RegistryItem for AgentEntry {
    fn name(&self) -> &str {
        self.descriptor.name()
    }

    fn description(&self) -> &str {
        self.descriptor.description()
    }
}

/// `Item = AgentEntry`; `list_paginate` delegates the slicing/cursor/envelope
/// work to [`synthia_core::registry::paginate_registry_list`] after applying
/// the filter + sort.
pub struct AgentRegistry {
    inner: synthia_core::NameTable<AgentEntry>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            inner: synthia_core::NameTable::new(),
        }
    }

    /// Current monotonic version of the registry.
    pub fn version(&self) -> u64 {
        self.inner.version()
    }

    /// For callers that cannot await.
    pub fn resolve_sync(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<dyn Agent>> {
        self.inner.get(name).map(|entry: AgentEntry| entry.agent())
    }

    /// Synchronous snapshot of all registered agent names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.inner.names()
    }

    /// First registered agent name, without materialising the full name list.
    pub fn first_name(&self) -> Option<String> {
        self.inner.names().into_iter().next()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True when no agent is registered.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Registry for AgentRegistry {
    type Filter = AgentFilter;
    type Item = AgentEntry;

    async fn put(&self, item: Self::Item) -> Result<(), Error> {
        let name = item.name().to_string();
        self.inner
            .insert_if_absent(name.clone(), item)
            .map_err(|_| Error::already_exists(name))
    }

    async fn delete(&self, name: &str) -> Result<(), Error> {
        if self.inner.remove(name).is_none() {
            return Err(Error::not_found(name));
        }
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<Self::Item>, Error> {
        Ok(self.inner.get(name))
    }

    async fn list_paginate(
        &self,
        cursor: Option<String>,
        limit: u64,
        _sort: Option<String>,
        filter: Option<Self::Filter>,
    ) -> Result<synthia_core::registry::RegistryList<Self::Item>, Error> {
        let filter = filter.unwrap_or_default();
        let mut out: Vec<AgentEntry> = self
            .inner
            .values()
            .into_iter()
            .filter(|e| matches_filter(&filter, e.descriptor()))
            .collect();
        out.sort_by(|a, b| a.name().cmp(b.name()));
        // Sort is applied above; cursor + limit + envelope come
        // from the shared pagination primitive.
        paginate_registry_list(out, cursor.as_deref(), limit)
    }
}

fn matches_filter(filter: &AgentFilter, d: &AgentDescriptor) -> bool {
    filter.kind.as_deref().is_none_or(|k| d.kind == k)
        && filter
            .capability
            .as_deref()
            .is_none_or(|c| d.capabilities.iter().any(|t| t == c))
        && filter
            .tool
            .as_deref()
            .is_none_or(|t| d.tools.iter().any(|n| n == t))
        && filter
            .min_version
            .as_deref()
            .is_none_or(|v| d.version.as_str() >= v)
        && filter
            .handoff
            .as_deref()
            .is_none_or(|h| d.handoffs.iter().any(|n| n == h))
        && filter
            .owner
            .as_deref()
            .is_none_or(|o| d.owner.as_deref() == Some(o))
        && filter
            .domain
            .as_deref()
            .is_none_or(|o| d.domain.as_deref() == Some(o))
}

#[cfg(test)]
mod tests;
