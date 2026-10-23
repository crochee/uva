//! `AgentRegistry` contract tests: the production file holds
//! the catalog (put/get/delete, sync resolve, versioned
//! listing); this sibling holds the `StubAgent` fixture and
//! the CRUD + filter-matrix + pagination tests.

use std::sync::Arc;

use synthia_core::{
    error::Error,
    registry::{Registry, RegistryItem},
};

use super::*;
use crate::agent::{Agent, AgentDescriptor, AgentEntry};

struct StubAgent {
    desc: AgentDescriptor,
}

#[async_trait::async_trait]
impl Agent for StubAgent {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.desc
    }

    async fn run(
        &self,
        _input: crate::input::AgentInput,
        _cancel: std::sync::Arc<dyn synthia_core::CancelToken>,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = crate::events::AgentEvent>
                + Send
                + 'static,
        >,
    > {
        Box::pin(futures::stream::empty())
    }
}

impl RegistryItem for StubAgent {
    fn name(&self) -> &str {
        &self.desc.name
    }

    fn description(&self) -> &str {
        &self.desc.description
    }
}

fn stub(name: &str, kind: &str, caps: &[&str]) -> AgentEntry {
    AgentEntry::new(Arc::new(StubAgent {
        desc: AgentDescriptor {
            name: name.into(),
            description: format!("desc for {name}"),
            kind: kind.into(),
            version: "1.0.0".into(),
            instructions: String::new(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            tools: Vec::new(),
            model_hint: None,
            handoffs: Vec::new(),
            handoff_hint: None,
            output_schema: None,
            owner: None,
            domain: None,
            persona: None,
            display_name: None,
            max_iterations: None,
        },
    }))
}

#[tokio::test]
async fn put_get_delete() {
    let reg = AgentRegistry::new();
    let e = stub("a", "react", &["tools"]);
    reg.put(e).await.unwrap();

    let got = reg.get("a").await.unwrap().unwrap();
    assert_eq!(got.name(), "a");

    reg.delete("a").await.unwrap();
    assert!(reg.get("a").await.unwrap().is_none());
}

#[tokio::test]
async fn duplicate_put_returns_error() {
    let reg = AgentRegistry::new();
    reg.put(stub("a", "react", &[])).await.unwrap();
    let err = reg.put(stub("a", "react", &[])).await.unwrap_err();
    assert!(matches!(err, Error::AlreadyExists { .. }), "got {err:?}");
}

#[tokio::test]
async fn unknown_delete_returns_not_found() {
    let reg = AgentRegistry::new();
    let err = reg.delete("nope").await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn list_sorted_by_name() {
    let reg = AgentRegistry::new();
    reg.put(stub("c", "react", &[])).await.unwrap();
    reg.put(stub("a", "react", &[])).await.unwrap();
    reg.put(stub("b", "react", &[])).await.unwrap();
    let items = reg.list(None).await.unwrap();
    assert_eq!(
        items.iter().map(|i| i.name()).collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}

#[tokio::test]
async fn list_filter_kind() {
    let reg = AgentRegistry::new();
    reg.put(stub("a", "react", &[])).await.unwrap();
    reg.put(stub("b", "pipeline", &[])).await.unwrap();
    let only_react = reg
        .list(Some(AgentFilter {
            kind: Some("react".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only_react.len(), 1);
    assert_eq!(only_react[0].name(), "a");
}

#[tokio::test]
async fn list_filter_capability() {
    let reg = AgentRegistry::new();
    reg.put(stub("a", "react", &["tools"])).await.unwrap();
    reg.put(stub("b", "react", &["streaming"])).await.unwrap();
    let only_tools = reg
        .list(Some(AgentFilter {
            capability: Some("tools".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only_tools.len(), 1);
    assert_eq!(only_tools[0].name(), "a");
}

#[tokio::test]
async fn list_filter_min_version() {
    let reg = AgentRegistry::new();
    let mut v_low = stub("a", "react", &[]);
    v_low.descriptor_mut().version = "0.9.0".into();
    let mut v_high = stub("b", "react", &[]);
    v_high.descriptor_mut().version = "1.0.0".into();
    reg.put(v_low).await.unwrap();
    reg.put(v_high).await.unwrap();
    let only_recent = reg
        .list(Some(AgentFilter {
            min_version: Some("1.0.0".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only_recent.len(), 1);
    assert_eq!(only_recent[0].name(), "b");
}

#[tokio::test]
async fn list_combined_filter_is_conjunctive() {
    let reg = AgentRegistry::new();
    reg.put(stub("a", "react", &["tools"])).await.unwrap();
    reg.put(stub("b", "pipeline", &["tools"])).await.unwrap();
    let only = reg
        .list(Some(AgentFilter {
            kind: Some("react".into()),
            capability: Some("tools".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name(), "a");
}

#[tokio::test]
async fn resolve_sync_returns_arc_dyn_agent() {
    let reg = AgentRegistry::new();
    reg.put(stub("a", "react", &[])).await.unwrap();
    let arc = reg.resolve_sync("a").unwrap();
    assert_eq!(arc.descriptor().name, "a");
    assert!(reg.resolve_sync("nope").is_none());
}

#[tokio::test]
async fn list_paginate_reuses_core_default() {
    let reg = AgentRegistry::new();
    for n in ["a", "b", "c"] {
        reg.put(stub(n, "react", &[])).await.unwrap();
    }
    let page = reg.list_paginate(None, 2, None, None).await.unwrap();
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].name(), "a");
    assert!(page.next_cursor.is_some());
    assert_eq!(page.total, Some(3));
}

#[tokio::test]
async fn list_filter_tool() {
    let reg = AgentRegistry::new();
    let mut a = stub("a", "react", &[]);
    a.descriptor_mut().tools = vec!["read_file".into()];
    let mut b = stub("b", "react", &[]);
    b.descriptor_mut().tools = vec!["shell".into()];
    reg.put(a).await.unwrap();
    reg.put(b).await.unwrap();
    let only = reg
        .list(Some(AgentFilter {
            tool: Some("shell".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name(), "b");
}

#[tokio::test]
async fn list_filter_handoff() {
    let reg = AgentRegistry::new();
    let mut a = stub("a", "react", &[]);
    a.descriptor_mut().handoffs = vec!["planner".into()];
    let b = stub("b", "react", &[]);
    reg.put(a).await.unwrap();
    reg.put(b).await.unwrap();
    let only = reg
        .list(Some(AgentFilter {
            handoff: Some("planner".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name(), "a");
}

#[tokio::test]
async fn list_filter_owner() {
    let reg = AgentRegistry::new();
    let mut a = stub("a", "react", &[]);
    a.descriptor_mut().owner = Some("team-a".into());
    let mut b = stub("b", "react", &[]);
    b.descriptor_mut().owner = Some("team-b".into());
    reg.put(a).await.unwrap();
    reg.put(b).await.unwrap();
    let only = reg
        .list(Some(AgentFilter {
            owner: Some("team-b".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name(), "b");
}

#[tokio::test]
async fn list_filter_domain() {
    let reg = AgentRegistry::new();
    let mut a = stub("a", "react", &[]);
    a.descriptor_mut().domain = Some("coding".into());
    let mut b = stub("b", "react", &[]);
    b.descriptor_mut().domain = Some("research".into());
    reg.put(a).await.unwrap();
    reg.put(b).await.unwrap();
    let only = reg
        .list(Some(AgentFilter {
            domain: Some("research".into()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name(), "b");
}
