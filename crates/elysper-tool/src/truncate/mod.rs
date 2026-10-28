//! Output truncation configuration and asynchronous truncation with
//! managed-file spill.

mod bound_output;

pub use bound_output::{
    OutputBound,
    OverflowStrategy,
    SanitizationPolicy,
    bound_output,
};
