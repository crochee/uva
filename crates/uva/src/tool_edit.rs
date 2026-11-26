//! [`uva_tool_plugins::edit`] — the `edit` tool plugin: the in-place
//! editor. It replaces an exact piece of a file's text (the old text
//! must appear verbatim and, unless `replace_all` is set, exactly
//! once), so a one-line change does not rewrite the whole file.

pub use uva_tool_plugins::edit::*;
