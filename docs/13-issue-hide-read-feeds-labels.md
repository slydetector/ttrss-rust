# Bug: "Labels" category hidden when "Hide read feeds" is enabled

## Symptom

With **Preferences → Settings → "Hide read feeds"** enabled (and
"Always show special feeds" checked), the top-level special feeds
(All Articles, Fresh, Starred, Published, Recently Read) remain visible,
but the **Labels** category node and all its children disappear from the
feed tree once all labeled articles are read.

## Root Cause

The feed tree rows get CSS classes assigned in `tt-rss/js/FeedTree.js`
(line 247–249):

```js
// Individual labels (bare_id in [-1024, 0)) get "Special":
if (item.bare_id >= App.LABEL_BASE_INDEX && item.bare_id < 0 && !is_cat
    || item.bare_id === Feeds.FEED_ARCHIVED && !is_cat) rc += " Special";

// Only the "Special" category (bare_id === -1) gets "AlwaysVisible":
if (item.bare_id === Feeds.CATEGORY_SPECIAL && is_cat) rc += " AlwaysVisible";

// Labels below LABEL_BASE_INDEX get "Label":
if (item.bare_id < App.LABEL_BASE_INDEX) rc += " Label";
```

The "Labels" category has `bare_id = -2` (`Feeds::CATEGORY_LABELS = -2`).
It receives **neither** `AlwaysVisible` **nor** `Special`.

The active CSS rule (with `hide-read-shows-special="true"`):

```css
body[hide-read-feeds="true"][hide-read-shows-special="true"]
  #feedTree .dijitTreeRow:not(.Unread):not(.AlwaysVisible):not(.Special) {
    display: none;
}
```

The Labels category row matches `:not(.Unread):not(.AlwaysVisible):not(.Special)`
→ gets `display: none` → all children hidden.

## Fix

One line in `tt-rss/js/FeedTree.js` (line 248) — also give the Labels
category the `AlwaysVisible` class:

```diff
- if (item.bare_id === Feeds.CATEGORY_SPECIAL && is_cat) rc += " AlwaysVisible";
+ if ((item.bare_id === Feeds.CATEGORY_SPECIAL || item.bare_id === Feeds.CATEGORY_LABELS) && is_cat) rc += " AlwaysVisible";
```

Need to confirm `Feeds.CATEGORY_LABELS` is accessible from FeedTree.js
scope (it's defined in `Feeds.js` as a property of the `Feeds` object;
`FeedTree.js` already references `Feeds.CATEGORY_SPECIAL` so
`Feeds.CATEGORY_LABELS` should work the same way).

## Why "Always show special fixes" doesn't help

That setting (`HIDE_READ_SHOWS_SPECIAL`) controls whether the CSS rule
excludes `.Special` rows. It's already `true` in the user's prefs. The
issue is that the **category** node doesn't get `.Special` or
`.AlwaysVisible` — it's a category, not a special feed.

## Status

Not yet fixed. One-line JS change; can be applied as a local patch or
upstreamed to the tt-rss project.
