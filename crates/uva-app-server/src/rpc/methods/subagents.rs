//! The delegated-children methods: `subagents/list`,
//! `subagents/cancel`, `subagents/steer`.
//!
//! The host's door to the children the `task` tool spawns — the
//! three moves a client needs when a delegated child misbehaves:
//! see it, stop it, redirect it.

use super::{names, parse};
use crate::{
    routes::subagents::{
        SubagentCancelParams,
        SubagentListParams,
        SubagentSteerParams,
        cancel_subagent,
        list_subagents,
        steer_subagent,
    },
    rpc::{RpcRouter, encode},
};

/// Register the sub-agent methods.
pub fn register(router: &mut RpcRouter) {
    router.register(names::SUBAGENTS_LIST, |state, raw| async move {
        let params: SubagentListParams = parse(raw)?;
        encode(list_subagents(state, params).await?)
    });
    router.register(names::SUBAGENTS_CANCEL, |state, raw| async move {
        let params: SubagentCancelParams = parse(raw)?;
        encode(cancel_subagent(state, params).await?)
    });
    router.register(names::SUBAGENTS_STEER, |state, raw| async move {
        let params: SubagentSteerParams = parse(raw)?;
        encode(steer_subagent(state, params).await?)
    });
}
