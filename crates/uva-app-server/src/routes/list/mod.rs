//! The list recipe: one declaration per route, one implementation.
//!
//! Every cursor-paginated list route answers the same six questions —
//! which sort keys it accepts, how each orders rows, which sort a
//! caller that names none gets, which way a `-` prefix turns it, which
//! row a page resumes after, and what the envelope holds. Written out
//! per route, those six answers were six copies of one recipe, and the
//! copies had drifted: a route promised a key it never sorted by, a
//! route accepted a direction it never applied, a route's declared
//! default never reached its ordering, two routes had no tiebreaker.
//!
//! This module is the one implementation. A route declares a
//! [`SortKey`] and nothing else:
//!
//! ```text
//! impl SortKey<AgentDetail> for AgentSort {
//!     const ALL: &'static [Self] = &[Self::Name];
//!     fn as_str(self) -> &'static str { … }
//!     fn compare(self, a: &AgentDetail, b: &AgentDetail) -> Ordering { … }
//!     fn default_sort() -> (Self, Direction) { (Self::Name, Direction::Ascending) }
//! }
//!
//! // in the handler:
//! AgentSort::sort_and_page(rows, &page, |d| d.name.as_str())
//! ```
//!
//! Three things follow from that shape, and they are the point:
//!
//! - **A declared key cannot go unimplemented.** [`SortKey::compare`]
//!   matches on the key type, so a variant with no arm does not
//!   compile. The old whitelist and the old `match` were two
//!   declarations and could disagree silently.
//! - **A direction cannot be forgotten.** The flip lives here, not in
//!   the caller. The old slicing helper documented "the caller MUST
//!   sort" as prose and could not tell whether they had.
//! - **A page boundary is always defined.** The cursor key is also the
//!   tiebreaker, so rows that compare equal still have one order. The
//!   old recipe appended a tiebreaker in two routes out of six.
//!
//! The slicing arithmetic itself is not here: it is
//! [`uva::core::registry::paginate_registry_list`], the workspace's one
//! cursor primitive. This module decides *what order* and *which page*;
//! that primitive decides *how a page is cut*.

use std::cmp::Ordering;

use uva::core::Error;

use crate::api::{List, PageQuery};

/// Which way a sort key orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Ascending,
    Descending,
}

/// One list route's sort vocabulary.
///
/// The three facts a caller can reach — the wire spelling of a key, how
/// it orders rows, and the sort a caller who names none gets — are
/// declared together here, which is why a route cannot promise one
/// without implementing it.
///
/// `Row` is the route's own row type, not necessarily its wire type: a
/// route whose sort key is not a field on the wire row (a skill's
/// creation time is its definition file's mtime) sorts a pair and
/// projects afterwards.
pub(crate) trait SortKey<Row>: Copy + 'static {
    /// Every key this route accepts, in a stable order. Tests walk it
    /// to prove each declaration sorts both ways.
    const ALL: &'static [Self];

    /// The key's wire spelling — the one name a client writes and the
    /// one name an error message quotes.
    fn as_str(self) -> &'static str;

    /// Parse a wire spelling (already stripped of its `-` prefix).
    /// `None` means "not one of mine".
    ///
    /// Derived from [`ALL`](Self::ALL) and [`as_str`](Self::as_str), so
    /// a key cannot be declared without being parseable, and cannot be
    /// parsed without being declared. A route that spells two keys the
    /// same way only ever reaches the first — its test catches that.
    fn from_str(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|key| key.as_str() == s)
    }

    /// Order two rows under this key. Ordering *between* rows, never
    /// direction: the direction is applied once, here in the recipe.
    fn compare(self, a: &Row, b: &Row) -> Ordering;

    /// The key and direction a caller who names no sort gets. A route
    /// whose list reads newest-first declares `Descending` here — the
    /// default is a fact about the route, not about the parameter.
    fn default_sort() -> (Self, Direction);

    /// The one parse rule every route shares.
    ///
    /// An absent `sort` names the route's default. A present one must
    /// name a key: empty, whitespace-only and a bare `-` are refused,
    /// so a client that sent something meaningless is told so rather
    /// than silently handed the default order.
    ///
    /// The key is parsed from the **raw** wire value, not from a
    /// resolved page's `sort_field`: resolution collapses an absent
    /// `sort` and a whitespace-only one into the same value, and this
    /// rule must tell them apart.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a value that names no field;
    /// [`Error::InvalidItem`] for a field this route does not accept.
    fn parse(sort: Option<&str>) -> Result<(Self, Direction), Error> {
        let Some(raw) = sort else {
            return Ok(Self::default_sort());
        };
        let trimmed = raw.trim();
        let (field, direction) = match trimmed.strip_prefix('-') {
            Some(rest) => (rest, Direction::Descending),
            None => (trimmed, Direction::Ascending),
        };
        if field.is_empty() {
            return Err(Error::validation("sort field cannot be empty"));
        }
        let key =
            Self::from_str(field).ok_or_else(|| Error::invalid_item(field))?;
        Ok((key, direction))
    }

    /// Sort `rows` by this route's key and slice them into one page.
    ///
    /// The caller hands over the **unsorted** rows and the request; this
    /// answers the order, the page, the cursor and the envelope. The
    /// cursor's key is the tiebreaker, so a page boundary never falls
    /// between two rows that compare equal.
    ///
    /// The cursor travels to the slicing primitive as the **raw** wire
    /// value: that primitive is what decodes it, so decoding here first
    /// would hand it an id it would then try to decode again.
    ///
    /// # Errors
    ///
    /// Whatever [`SortKey::parse`] refuses, a `limit` of zero, and the
    /// cursor rules the slicing primitive enforces (an undecodable
    /// cursor is [`Error::InvalidItem`]).
    fn sort_and_page(
        rows: Vec<Row>,
        page: &PageQuery,
        id_of: impl Fn(&Row) -> &str,
    ) -> Result<List<Row>, Error> {
        let (key, direction) = Self::parse(page.sort.as_deref())?;
        let limit = crate::api::v1::cursor::normalize_limit(page.limit)?;

        let mut rows = rows;
        // `compare` gives the key's order; the cursor key breaks ties,
        // so `reverse` below cannot leave equal rows in a page-order
        // the next request cannot resume.
        rows.sort_by(|a, b| {
            key.compare(a, b).then_with(|| id_of(a).cmp(id_of(b)))
        });
        if direction == Direction::Descending {
            rows.reverse();
        }

        let page = uva::core::registry::paginate_registry_list(
            rows,
            page.cursor.as_deref(),
            limit,
            id_of,
        )?;

        Ok(List {
            data: page.data,
            next_cursor: page.next_cursor,
            total: page.total,
        })
    }
}

#[cfg(test)]
mod tests;
