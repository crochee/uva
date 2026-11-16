//! Print the frontend's wire contract as JSON Schema on stdout.
//!
//! `uva-web/src/api/types.ts` is generated from this document — run the
//! generator rather than this binary by hand:
//!
//! ```text
//! cd uva-web && npm run generate:types
//! ```
//!
//! The document is deterministic (sorted keys, no timestamps, no
//! filesystem paths), so two runs over the same sources are
//! byte-identical. `scripts/check-web-contract.sh` is what compares a
//! fresh run against the committed TypeScript.
//!
//! `make examples` executes this binary too: stdout carries the
//! document and stderr carries the one-line proof of what it holds.

use std::io::Write as _;

fn main() {
    let entries = uva_app_server::api::contract::catalog().len();
    let document = uva_app_server::api::contract::document_json();
    eprintln!(
        "contract: {entries} catalogued types, {} bytes",
        document.len()
    );
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(document.as_bytes())
        .expect("stdout accepts the document");
    stdout.flush().expect("stdout flushes");
}
