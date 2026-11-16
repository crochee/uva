//! Tool-definition projection: the memoised model-facing tool list and the FNV-1a fingerprints the hot path compares.
use std::{collections::HashSet, sync::Arc};

use uva_provider::{Message, ToolDefinition};

use crate::AgentCaps;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[inline]
fn fnv1a_byte(hash: u64, byte: u8) -> u64 {
    (hash ^ byte as u64).wrapping_mul(FNV_PRIME)
}

pub(crate) fn fnv1a_str(acc: u64, name: &str) -> u64 {
    let mut h = acc;
    for &b in name.as_bytes() {
        h = fnv1a_byte(h, b);
    }
    h
}

#[inline]
fn fnv1a_u64(acc: u64, value: u64) -> u64 {
    let mut h = acc;
    for shift in (0..8).map(|i| i * 8) {
        h = fnv1a_byte(h, (value >> shift) as u8);
    }
    h
}

pub(crate) fn tool_definitions(
    caps: &AgentCaps,
    projection: &mut ToolProjectionCache,
) -> Arc<Vec<ToolDefinition>> {
    let descriptors = caps.tools.registry.descriptors_cached();
    projection.refresh_deferred(
        caps.tools.registry.version(),
        descriptors
            .iter()
            .any(|d| d.exposure == uva_tool::ToolExposure::Deferred),
    );
    let fp = fingerprint(caps.tools.registry.version(), projection);
    // The build closure needs `projection` for the called-set
    // and the counter, so the memo lookup borrows `projection`
    // mutably through one reborrow.
    memoized(&mut projection.value, fp, || {
        projection.builds = projection.builds.saturating_add(1);
        Arc::new(compose_tool_definitions(
            &descriptors,
            caps.tools.surface.as_deref(),
            caps.tools.restriction.as_deref(),
            &projection.called,
        ))
    })
}

#[inline]
fn fingerprint(registry_version: u64, cache: &ToolProjectionCache) -> u64 {
    let called_fp = if cache.has_deferred {
        cache.called_fingerprint
    } else {
        0
    };
    compute_fingerprint(registry_version, called_fp)
}

/// The tool definitions this run offers the model: the registry's
/// projection — surface policy and restriction applied. The request
/// header hashes this, so the row names the tool set the model was
/// offered rather than the registry it was drawn from.
pub(crate) fn offered_tools(caps: &AgentCaps) -> Arc<Vec<ToolDefinition>> {
    let mut projection = ToolProjectionCache::new();
    tool_definitions(caps, &mut projection)
}

pub(crate) fn tool_names_hash(tools: &[ToolDefinition]) -> u64 {
    let mut h = FNV_OFFSET;
    for t in tools {
        h = fnv1a_str(h, &t.name);
    }
    h
}

#[inline]
pub(crate) fn compute_fingerprint(
    registry_version: u64,
    called_fingerprint: u64,
) -> u64 {
    let mut h = FNV_OFFSET;
    h = fnv1a_u64(h, registry_version);
    h = fnv1a_u64(h, called_fingerprint);
    h
}

// ===== memo slot =====================================================

/// The closure is consumed lazily: an eager signature paid the full rebuild
/// on every hit before throwing the result away.
pub(crate) fn memoized<T>(
    slot: &mut Option<(u64, Arc<T>)>,
    current_fp: u64,
    build: impl FnOnce() -> Arc<T>,
) -> Arc<T> {
    match slot {
        Some((fp, val)) if *fp == current_fp => Arc::clone(val),
        slot => {
            let arc = build();
            *slot = Some((current_fp, Arc::clone(&arc)));
            arc
        }
    }
}

#[derive(Default)]
pub(crate) struct ToolProjectionCache {
    /// Names of every tool this run has called. Monotonic.
    pub called: HashSet<String>,
    /// Order-sensitive fingerprint of `called`, so the memo
    /// invalidates when the called set reorders across turns.
    pub called_fingerprint: u64,
    /// Refreshed only on a version bump, so the per-iteration scan does
    /// not walk every descriptor.
    pub has_deferred: bool,
    /// The `registry.version()` `has_deferred` was computed against.
    pub registry_version: u64,
    /// The cached `Arc<Vec<ToolDefinition>>`.
    pub value: Option<(u64, Arc<Vec<ToolDefinition>>)>,
    /// Test instrumentation. Pin: `1` for a static tool set, `2` after one
    /// `Deferred` promotion.
    pub builds: u32,
}

impl ToolProjectionCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The memo key mixes `registry.version()` in, so this is the same
    /// invalidation trigger.
    pub(crate) fn refresh_deferred(
        &mut self,
        registry_version: u64,
        has_deferred: bool,
    ) {
        if self.registry_version != registry_version {
            self.registry_version = registry_version;
            self.has_deferred = has_deferred;
        }
    }

    pub(crate) fn record_call(&mut self, name: &str) {
        if !self.called.contains(name) {
            self.called.insert(name.to_string());
            self.called_fingerprint = fnv1a_str(self.called_fingerprint, name);
        }
    }

    pub(crate) fn seed(&mut self, messages: &[Message]) {
        let mut called = HashSet::new();
        let mut fp = 0u64;
        for message in messages {
            for part in &message.content {
                record_part(&mut called, &mut fp, part);
            }
        }
        self.called = called;
        self.called_fingerprint = fp;
    }
}

fn extract_called_name(part: &uva_provider::ContentPart) -> Option<String> {
    match part {
        uva_provider::ContentPart::ToolUse(call) => Some(call.name.clone()),
        uva_provider::ContentPart::ToolResult(result) => {
            result.tool_name.clone()
        }
        _ => None,
    }
}

fn record_part(
    called: &mut HashSet<String>,
    fp: &mut u64,
    part: &uva_provider::ContentPart,
) {
    let Some(name) = extract_called_name(part) else {
        return;
    };
    if called.insert(name.clone()) {
        *fp = fnv1a_str(*fp, &name);
    }
}

// ===== model-facing tool list ========================================

pub(crate) fn compose_tool_definitions(
    descriptors: &[uva_tool::ToolDescriptor],
    surface: Option<&uva_tool::ToolSurfacePolicy>,
    restriction: Option<&uva_tool::ToolRestriction>,
    called: &HashSet<String>,
) -> Vec<uva_provider::ToolDefinition> {
    // The visible-name set is computed once per call and applied
    // verbatim. `None` means "advertise every non-hidden tool", so
    // the set is only built when a filter is configured or a
    // descriptor is an MCP self tool (which the projector does
    // not filter).
    let mut visible: Option<HashSet<String>> = None;
    if surface.is_some()
        || restriction.is_some()
        || descriptors
            .iter()
            .any(|d| uva_tool::is_mcp_self_tool(&d.name))
    {
        let mut set = visible_names(descriptors, surface, restriction);
        set.retain(|name| !uva_tool::is_mcp_self_tool(name));
        visible = Some(set);
    }
    uva_tool::project_tool_definitions(descriptors, called, visible.as_ref())
}

fn visible_names(
    descriptors: &[uva_tool::ToolDescriptor],
    surface: Option<&uva_tool::ToolSurfacePolicy>,
    restriction: Option<&uva_tool::ToolRestriction>,
) -> HashSet<String> {
    match surface {
        Some(policy) => {
            let mut names = policy.visible_tool_names(descriptors);
            if let Some(r) = restriction {
                names.retain(|name| r.is_relevant(name, false));
            }
            names
        }
        None => descriptors
            .iter()
            .map(|d| d.name.clone())
            .filter(|name| {
                restriction.is_none_or(|r| r.is_relevant(name, false))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    //! The memoised-projection contract: a hit must reuse the
    //! cached `Arc` without re-running the build closure. The
    //! counter is the test's only oracle — a regression to the
    //! eager-build ordering would run the closure on every
    //! iteration regardless of fingerprint.
    use super::*;

    #[test]
    fn memo_hit_does_not_re_run_the_build_closure() {
        let mut slot: Option<(u64, Arc<Vec<u64>>)> = None;
        let arc1 = memoized(&mut slot, 7, || Arc::new(vec![1, 2, 3]));
        assert_eq!(*arc1, vec![1, 2, 3]);
        assert!(slot.is_some());
        // Same fingerprint: reuse the cached `Arc`, do not rebuild.
        let arc2 = memoized(&mut slot, 7, || {
            panic!("hit path must not rebuild");
        });
        assert!(Arc::ptr_eq(&arc1, &arc2));
        // Different fingerprint: rebuild.
        let arc3 = memoized(&mut slot, 8, || Arc::new(vec![4]));
        assert!(!Arc::ptr_eq(&arc1, &arc3));
        assert_eq!(*arc3, vec![4]);
    }

    #[test]
    fn tool_projection_cache_record_call_is_idempotent() {
        let mut cache = ToolProjectionCache::new();
        cache.record_call("a");
        let fp = cache.called_fingerprint;
        cache.record_call("a");
        assert_eq!(cache.called_fingerprint, fp);
        cache.record_call("b");
        assert_ne!(cache.called_fingerprint, fp);
        assert_eq!(cache.called.len(), 2);
    }
}
