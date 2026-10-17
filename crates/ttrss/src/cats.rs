//! Virtual feed and category pseudo-ids, shared across the feed modules.
//!
//! One source of truth for the PHP sentinel ids in
//! `tt-rss/classes/Feeds.php` (`Feeds::FEED_*` / `Feeds::CATEGORY_*`) used by
//! `Feeds`, `Headlines`, `API`, and the counter queries. The port's `CAT_*`
//! names are the PHP `CATEGORY_*` constants.

pub const CAT_UNCATEGORIZED: i32 = 0;
pub const CAT_SPECIAL: i32 = -1;
pub const CAT_LABELS: i32 = -2;
pub const CAT_ALL_EXCEPT_VIRTUAL: i32 = -3;
pub const CAT_ALL: i32 = -4;

/// `Feeds::FEED_*` — the virtual-feed pseudo-ids (negative = special feed,
/// `0` = archived).
pub const FEED_ARCHIVED: i32 = 0;
pub const FEED_STARRED: i32 = -1;
pub const FEED_PUBLISHED: i32 = -2;
pub const FEED_FRESH: i32 = -3;
pub const FEED_ALL: i32 = -4;
pub const FEED_RECENTLY_READ: i32 = -6;
pub const FEED_ERROR: i32 = -7;

/// `PLUGIN_FEED_BASE_INDEX` (functions.php): virtual plugin feeds occupy
/// `(LABEL_BASE_INDEX, PLUGIN_FEED_BASE_INDEX)`.
pub const PLUGIN_FEED_BASE_INDEX: i32 = -128;
