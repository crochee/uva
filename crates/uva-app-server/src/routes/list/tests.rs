//! The list recipe, tested through its interface.
//!
//! One key vocabulary stands in for the six routes: two keys, and a
//! descending default. That is the shape `sessions` and `tasks` have —
//! and the shape the old six-copy recipe could not express, because the
//! default lived in a `unwrap_or` on the validation call while the
//! ordering read a different value.

use std::cmp::Ordering;

use uva::core::Error;

use super::{Direction, SortKey};
use crate::api::{List, PageQuery, encode_cursor};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name: &'static str,
    rank: u32,
}

fn row(name: &'static str, rank: u32) -> Row {
    Row { name, rank }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Name,
    Rank,
}

impl SortKey<Row> for Key {
    const ALL: &'static [Self] = &[Self::Name, Self::Rank];

    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Rank => "rank",
        }
    }

    fn compare(self, a: &Row, b: &Row) -> Ordering {
        match self {
            Self::Name => a.name.cmp(b.name),
            Self::Rank => a.rank.cmp(&b.rank),
        }
    }

    fn default_sort() -> (Self, Direction) {
        (Self::Rank, Direction::Descending)
    }
}

fn id_of(row: &Row) -> &str {
    row.name
}

/// Two rows share a rank: the tiebreaker is what decides their order.
fn tied_rows() -> Vec<Row> {
    vec![
        row("delta", 2),
        row("alpha", 3),
        row("charlie", 1),
        row("bravo", 2),
    ]
}

/// No two rows share a value under either key, so a mirrored order is
/// unambiguous.
fn distinct_rows() -> Vec<Row> {
    vec![
        row("delta", 1),
        row("charlie", 2),
        row("bravo", 3),
        row("alpha", 4),
    ]
}

fn query() -> PageQuery {
    PageQuery::new()
}

fn names(list: &List<Row>) -> Vec<&'static str> {
    list.data.iter().map(|r| r.name).collect()
}

// --- the default sort ---------------------------------------------------

#[test]
fn a_route_that_names_no_sort_gets_its_declared_default() {
    // Default is (rank, descending). Ordering is (rank, name) ascending
    // and then reversed as a whole, so equal ranks also reverse — the
    // direction is a fact about the whole comparator, not just the key.
    let list = Key::sort_and_page(tied_rows(), &query(), id_of).unwrap();
    assert_eq!(names(&list), ["alpha", "delta", "bravo", "charlie"]);
}

#[test]
fn naming_a_key_orders_that_key_ascending() {
    let list =
        Key::sort_and_page(tied_rows(), &query().with_sort("rank"), id_of)
            .unwrap();
    assert_eq!(names(&list), ["charlie", "bravo", "delta", "alpha"]);
}

#[test]
fn a_minus_prefix_turns_the_named_key() {
    let list =
        Key::sort_and_page(tied_rows(), &query().with_sort("-name"), id_of)
            .unwrap();
    assert_eq!(names(&list), ["delta", "charlie", "bravo", "alpha"]);
}

// --- the key declaration ------------------------------------------------

#[test]
fn every_declared_key_has_a_distinct_wire_spelling() {
    // Parsing is derived from the declaration, so the invariant worth
    // pinning is injectivity: two keys sharing a spelling would make
    // the second one unreachable from the wire.
    let mut spellings: Vec<&str> =
        Key::ALL.iter().map(|k| k.as_str()).collect();
    let declared = spellings.len();
    spellings.sort_unstable();
    spellings.dedup();
    assert_eq!(spellings.len(), declared, "two keys share a spelling");
}

#[test]
fn every_declared_key_mirrors_under_descending() {
    for key in Key::ALL {
        let up = Key::sort_and_page(
            distinct_rows(),
            &query().with_sort(key.as_str()),
            id_of,
        )
        .unwrap();
        let down = Key::sort_and_page(
            distinct_rows(),
            &query().with_sort(format!("-{}", key.as_str())),
            id_of,
        )
        .unwrap();

        let mut mirrored = names(&up);
        mirrored.reverse();
        assert_eq!(
            names(&down),
            mirrored,
            "key `{}` does not mirror when turned",
            key.as_str()
        );
    }
}

// --- what the recipe refuses -------------------------------------------

#[test]
fn an_unknown_key_is_refused() {
    let err =
        Key::sort_and_page(tied_rows(), &query().with_sort("nope"), id_of)
            .unwrap_err();
    assert!(matches!(err, Error::InvalidItem { .. }), "got {err:?}");
}

#[test]
fn a_value_that_names_no_field_is_refused() {
    // The old whitelist check refused all four before the recipe
    // existed; the point of keeping them refused is that a client who
    // sent something meaningless is told, not handed the default order.
    for raw in ["", "   ", "-", " - "] {
        let err =
            Key::sort_and_page(tied_rows(), &query().with_sort(raw), id_of)
                .unwrap_err();
        assert!(
            matches!(err, Error::Validation { .. }),
            "`{raw}` should name no field, got {err:?}"
        );
    }
}

#[test]
fn an_undecodable_cursor_is_refused() {
    let err = Key::sort_and_page(
        tied_rows(),
        &query().with_cursor("not-base64!!"),
        id_of,
    )
    .unwrap_err();
    assert!(matches!(err, Error::InvalidItem { .. }), "got {err:?}");
}

// --- tiebreaking --------------------------------------------------------

#[test]
fn equal_keys_take_their_order_from_the_cursor_key() {
    let list =
        Key::sort_and_page(tied_rows(), &query().with_sort("rank"), id_of)
            .unwrap();
    // bravo and delta both rank 2; ascending order is by name.
    assert_eq!(names(&list), ["charlie", "bravo", "delta", "alpha"]);
}

#[test]
fn the_tiebreak_turns_with_the_key() {
    let list =
        Key::sort_and_page(tied_rows(), &query().with_sort("-rank"), id_of)
            .unwrap();
    // Same pair, now descending as a whole: delta before bravo.
    assert_eq!(names(&list), ["alpha", "delta", "bravo", "charlie"]);
}

// --- paging -------------------------------------------------------------

#[test]
fn pages_resume_after_the_row_the_cursor_names() {
    let first = Key::sort_and_page(
        distinct_rows(),
        &query().with_sort("name").with_limit(2),
        id_of,
    )
    .unwrap();
    assert_eq!(names(&first), ["alpha", "bravo"]);
    assert_eq!(first.total, Some(4));
    let cursor = first.next_cursor.clone().expect("a second page exists");

    let second = Key::sort_and_page(
        distinct_rows(),
        &query().with_sort("name").with_limit(2).with_cursor(cursor),
        id_of,
    )
    .unwrap();
    assert_eq!(names(&second), ["charlie", "delta"]);
    assert_eq!(second.next_cursor, None);
    assert_eq!(second.total, Some(4));
}

#[test]
fn a_cursor_past_the_end_is_an_empty_page_that_still_counts() {
    let list = Key::sort_and_page(
        distinct_rows(),
        &query().with_sort("name").with_cursor(encode_cursor("zulu")),
        id_of,
    )
    .unwrap();
    assert!(names(&list).is_empty());
    assert_eq!(list.next_cursor, None);
    assert_eq!(list.total, Some(4));
}

#[test]
fn an_empty_input_is_an_empty_page() {
    let list = Key::sort_and_page(Vec::new(), &query(), id_of).unwrap();
    assert_eq!(list.total, Some(0));
    assert!(names(&list).is_empty());
}

#[test]
fn a_page_larger_than_the_set_holds_everything_and_offers_no_cursor() {
    let list = Key::sort_and_page(
        distinct_rows(),
        &query().with_sort("name").with_limit(99),
        id_of,
    )
    .unwrap();
    assert_eq!(names(&list), ["alpha", "bravo", "charlie", "delta"]);
    assert_eq!(list.next_cursor, None);
}
