//! Special/label category IDs shared across the feed modules.
//!
//! These mirror the PHP sentinel `cat_id` values (`-1` = Special, `-2` =
//! Labels, `0` = Uncategorized) used by `Feeds`, `Headlines`, `API`, and the
//! counter queries.

pub const CAT_UNCATEGORIZED: i64 = 0;
pub const CAT_SPECIAL: i64 = -1;
pub const CAT_LABELS: i64 = -2;
