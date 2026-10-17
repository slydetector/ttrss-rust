//! Special/label category IDs shared across the feed modules.
//!
//! These mirror the PHP sentinel `cat_id` values (`-1` = Special, `-2` =
//! Labels, `-3` = All-except-virtual, `-4` = All, `0` = Uncategorized) used by
//! `Feeds`, `Headlines`, `API`, and the counter queries.

pub const CAT_UNCATEGORIZED: i32 = 0;
pub const CAT_SPECIAL: i32 = -1;
pub const CAT_LABELS: i32 = -2;
pub const CAT_ALL_EXCEPT_VIRTUAL: i32 = -3;
pub const CAT_ALL: i32 = -4;
