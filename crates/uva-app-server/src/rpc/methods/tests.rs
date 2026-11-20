//! The method table's own tests.
//!
//! The names table ([`names`]) and the router are two views of one
//! thing: the methods the process answers. They must agree in both
//! directions —
//!
//! - a method the router registers but the table does not list is a
//!   method nobody declared: a client cannot name it, and the table
//!   claims to be the whole registry;
//! - a method the table lists but the router does not register is a
//!   name nobody serves: every client that reads the table and calls
//!   it gets a JSON-RPC "method not found".
//!
//! The table is what the clients read (the terminal names its
//! requests with these constants, the front-end method gate reads
//! this file), so a drift in either direction is a wire break.
use std::collections::BTreeSet;

use super::{names, register_all};
use crate::rpc::RpcRouter;

/// The router with every method the process answers.
fn router() -> RpcRouter {
    let mut router = RpcRouter::new();
    register_all(&mut router);
    router
}

/// The names the router serves, as a set.
fn registered() -> BTreeSet<&'static str> {
    router().method_names().into_iter().collect()
}

/// The names the table lists, as a set.
fn listed() -> BTreeSet<&'static str> {
    names::ALL.iter().copied().collect()
}

#[test]
fn every_method_the_router_serves_is_listed_in_the_names_table() {
    let missing: Vec<&str> =
        registered().difference(&listed()).copied().collect();
    assert!(
        missing.is_empty(),
        "the router serves methods the names table does not list: {missing:?}"
    );
}

#[test]
fn every_method_the_names_table_lists_is_served_by_the_router() {
    let missing: Vec<&str> =
        listed().difference(&registered()).copied().collect();
    assert!(
        missing.is_empty(),
        "the names table lists methods the router does not serve: {missing:?}"
    );
}

#[test]
fn the_names_table_lists_no_name_twice() {
    assert_eq!(
        listed().len(),
        names::ALL.len(),
        "the names table lists a name twice"
    );
}

/// The constants the file declares, with their values.
///
/// The file is read as text, because the question is a question about
/// the file: a constant that is declared but never registered is a
/// spelling with no method behind it, and nothing else in the crate
/// can see it.
fn declared() -> Vec<(&'static str, &'static str)> {
    include_str!("names.rs")
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("pub const ")?;
            let (name, value) = rest.split_once(": &str = \"")?;
            let (value, _) = value.split_once('"')?;
            Some((name, value))
        })
        .collect()
}

/// Every constant the file declares is in the table.
///
/// The table is the whole registry, so a constant that is declared
/// and not listed is a name the clients cannot see — and the gates
/// that read this file would take it for a method the server
/// serves. The table and the declarations must be the same set.
#[test]
fn every_constant_the_file_declares_is_in_the_table() {
    let mut missing = Vec::new();
    for (name, value) in declared() {
        if !names::ALL.contains(&value) {
            missing.push(format!("{name} = {value:?}"));
        }
    }
    assert!(
        missing.is_empty(),
        "the file declares constants the table does not list: {missing:?}"
    );
}
