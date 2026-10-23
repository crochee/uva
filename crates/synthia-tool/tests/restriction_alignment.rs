//! Cross-cutting contract test for `ToolRestriction`.
//!
//! The harness applies a `ToolRestriction` in **two** places:
//!   - at projection time (`compose_tool_definitions` in
//!     `synthia-harness`), to keep denied tools out of the model-facing
//!     `ToolDefinition` list;
//!   - at dispatch time (`pre_execution_veto` in
//!     `synthia-harness`'s `loop_/seams/mod.rs`), to refuse a model-issued
//!     call whose name the restriction rejects.
//!
//! Both sites ask the same question — "is this tool name relevant
//! here?" — and both go through [`ToolRestriction::is_relevant`]. The
//! harness's `tests/tool_restriction.rs` covers the **behavioural**
//! alignment end-to-end (a denied tool is hidden AND refused); this
//! file pins the **API contract** that makes the alignment possible:
//!
//! 1. `is_relevant` is the **only** method callers need. The two
//!    enforcement sites must not invent their own filtering logic.
//! 2. The method is pure (same `(restriction, name, is_own)` -> same
//!    answer) — the harness relies on that when it filters the
//!    projection and the dispatch against the same value.
//! 3. `is_relevant("name", is_own=true)` is always `true` regardless
//!    of `allow` / `deny` — child's own registrations are exempt from
//!    the parent's restriction.
//! 4. A composed `intersect(parent, child)` restriction permits
//!    exactly the tools both sides permit (and nothing else).
//!
//! These four assertions are the structural pin the harness relies
//! on; a future change that broke any of them would silently let a
//! projection and a dispatch disagree on the same tool name.

use synthia_tool::ToolRestriction;

#[test]
fn deny_list_blocks_inherited_tool_but_never_owns() {
    let r = ToolRestriction::deny(["shell"]);
    // The inherited tool is hidden from the agent's perspective...
    assert!(!r.is_relevant("shell", false));
    // ...and remains visible to the child's own registration.
    assert!(r.is_relevant("shell", true));
    // A tool the restriction does not name is untouched.
    assert!(r.is_relevant("read", false));
    assert!(r.is_relevant("read", true));
}

#[test]
fn allow_list_permits_only_listed_tools() {
    let r = ToolRestriction::allow(["read"]);
    assert!(r.is_relevant("read", false));
    // Own-registration exemption is documented as always-true for
    // any restriction — the allow-list only constrains inherited
    // tools, not a tool the child itself registered. This matches
    // `is_relevant`'s early-return: if is_own, return true.
    assert!(r.is_relevant("read", true));
    assert!(r.is_relevant("write", true));
    assert!(!r.is_relevant("write", false));
    // ... but is not on the list, so inherited usage is denied.
    assert!(!r.is_relevant("write", false));
}

#[test]
fn empty_restriction_is_pure_passthrough() {
    let r = ToolRestriction::none();
    assert!(r.is_empty());
    for name in ["read", "write", "shell", "any_tool"] {
        assert!(r.is_relevant(name, false));
        assert!(r.is_relevant(name, true));
    }
}

#[test]
fn intersect_shrinks_to_the_common_allow_set() {
    // Parent allows {read, write}; child allows {read, shell};
    // intersection permits {read} only.
    let parent = ToolRestriction::allow(["read", "write"]);
    let child = ToolRestriction::allow(["read", "shell"]);
    let both = parent.intersect(&child);
    assert!(both.is_relevant("read", false));
    assert!(!both.is_relevant("write", false));
    assert!(!both.is_relevant("shell", false));
}

#[test]
fn intersect_unions_deny_lists() {
    // Either side may deny a tool; the result denies it regardless.
    let parent = ToolRestriction::deny(["shell"]);
    let child = ToolRestriction::deny(["write"]);
    let both = parent.intersect(&child);
    assert!(!both.is_relevant("shell", false));
    assert!(!both.is_relevant("write", false));
    assert!(both.is_relevant("read", false));
}

#[test]
fn intersect_with_an_empty_side_returns_the_other() {
    let none = ToolRestriction::none();
    let real = ToolRestriction::deny(["shell"]);
    assert!(!none.intersect(&real).is_relevant("shell", false));
    assert!(!real.intersect(&none).is_relevant("shell", false));
    // Empty ∩ empty == empty (pass-through).
    assert!(none.intersect(&none).is_empty());
}

#[test]
fn is_relevant_is_pure() {
    // Same input -> same answer. A loop body that recomputes this on
    // every iteration must be safe; the value is determined by the
    // three inputs alone.
    let r = ToolRestriction::deny(["shell"]);
    for _ in 0..16 {
        assert!(!r.is_relevant("shell", false));
        assert!(r.is_relevant("shell", true));
        assert!(r.is_relevant("read", false));
    }
}
