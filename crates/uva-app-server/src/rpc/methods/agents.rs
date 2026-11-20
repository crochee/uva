//! The definition surfaces: agents, skills and tools.
use serde::Deserialize;

use super::{names, parse};
use crate::{
    api::PageQuery,
    routes::{
        agents::{self, AgentDescriptorRequest},
        skills::{self, CreateSkillRequest},
        tool::{self, RegisterToolRequest},
    },
    rpc::{RpcRouter, encode},
};

/// Register the agent, skill and tool methods.
pub fn register(router: &mut RpcRouter) {
    router.register(names::AGENTS_LIST, |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(agents::list_agents(state, page).await?)
    });
    router.register(names::AGENTS_CREATE, |state, raw| async move {
        let request: AgentDescriptorRequest = parse(raw)?;
        encode(agents::create_agent(state, request).await?)
    });
    router.register(names::AGENTS_DEFAULT, |state, _raw| async move {
        encode(agents::get_default_agent(state).await?)
    });
    router.register(names::AGENTS_GET, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(agents::get_agent(state, params.name).await?)
    });
    router.register(names::AGENTS_DELETE, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(agents::delete_agent(state, params.name).await?)
    });
    router.register(names::SKILLS_LIST, |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(skills::list_skills(state, page).await?)
    });
    router.register(names::SKILLS_CREATE, |state, raw| async move {
        let request: CreateSkillRequest = parse(raw)?;
        encode(skills::create_skill(state, request).await?)
    });
    router.register(names::SKILLS_RELOAD, |state, _raw| async move {
        encode(skills::reload_skills(state).await?)
    });
    router.register(names::SKILLS_GET, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(skills::get_skill(state, params.name).await?)
    });
    router.register(names::SKILLS_DELETE, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(skills::delete_skill(state, params.name).await?)
    });
    router.register(names::TOOLS_LIST, |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(tool::list_tools(state, page).await?)
    });
    router.register(names::TOOLS_REGISTER, |state, raw| async move {
        let request: RegisterToolRequest = parse(raw)?;
        encode(tool::register_tool(state, request).await?)
    });
    router.register(names::TOOLS_GET, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(tool::get_tool(state, params.name).await?)
    });
    router.register(names::TOOLS_UNREGISTER, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(tool::unregister_tool(state, params.name).await?)
    });
}
/// The one-parameter shape: `{"name": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct NameParams {
    name: String,
}
