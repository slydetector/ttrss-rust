# PHP→Rust Port Gap Scan

**Date:** 2026-09-27 · **Scope:** from-scratch scan of the frozen PHP reference
(`tt-rss/`) against the Rust port (`crates/ttrss/src/`) to surface *pieces that were
missed in the port*. The PHP reference (commit `2f7d831`) is the spec.

This is a research report, not a work order. Every finding below was confirmed by
reading the actual Rust code — not just a "it's not in a grep" observation.

---

## TL;DR

The port is **highly complete**. Scanning each major surface of the PHP reference and
cross-checking the port found:

- **Every major surface is ported and faithful:** the external JSON API (all 24 ops),
  the internal `backend.php` RPC (every (op, method) the frozen frontend sends),
  the `public.php` surface, the full pref system (all 45 prefs + profile blacklist +
  default table), and **all 18 first-party plugins with every hook they register
  actually dispatched.**
- **A handful of genuine, mostly low-severity entry-point gaps** (see §Gaps): 30 of
  53 plugin hooks have no dispatch site, the plugin-API-method dispatch seam is
  half-wired, the PHP DB-logging subsystem is replaced by `tracing`, and the web
  `dbupdate` op is absent.
- **A set of *behavioral* partials** the entry-point scan cannot see (see
  §Behavioral partials). The standouts: the API **`getHeadlines` ignores 4 of 19
    params — including `search` and `force_update`, so API full-text search is broken
    and pre-read refresh is a no-op** (B5, found by param-diffing all 24 API ops); and **feed-date parsing handles only RFC 2822/3339**
  where PHP's `strtotime` accepts dozens more (B1). Plus deliberate deviations
  (`checkforupdates` stubbed, the "daemon not updating" warning dropped, the 30 s
  feed-content cache dropped — B2–B4).
- **The rest is consciously skipped** (daemon/update CLI by design decision D1=B, the
  admin CLI + schema-migration engine as M8) plus the already-documented deferrals in
  `plan.md` (Mailer/`lettre`, timezone snapshot, `getFeedIcon` headers,
  `html_to_text`, …).

Nothing found that breaks a first-party plugin or a primary read/write path. The gaps
are API-completeness, behavioral-fidelity, admin-diagnostic, and M8 items.

> **Why the entry-point scan under-finds.** "Is this op/method/pref/hook present?"
> finds almost nothing because the port is *structurally* complete. The real gaps are
> one level down — **response JSON shapes, parameter handling, SQL semantics,
> branching/edge cases, error codes, and i18n string coverage.** Those are *not*
> exhaustively findable by source inspection; they're what the retired differential
> oracle (record/replay, retired 2026-09-19, see AGENTS.md) was built to catch. The
> §Behavioral partials below are the *self-documented* subset of that category; the
> *undocumented* silent divergences are the long tail a source scan can't enumerate.

---

## Method

1. **Enumerated each PHP surface directly** from `tt-rss/`: the `API.php` op list,
   the `backend.php` handler-class methods, the `Handler_Public` (public.php) methods,
   the `Prefs` registry, the `PluginHost`/`Plugin` hook constants, the `update.php`
   `options_map`, and the class inventory.
2. **Cross-checked each against the port** in `crates/ttrss/src/`.
3. For the backend RPC surface I used the port's own maintained tool
   `browser/diff_rpc_surface.py` (it diffs the frozen frontend's sent (op, method)
   pairs against the live dispatch in `rpc.rs`) — **then verified its one reported
   gap by hand** (it was a false positive; see Appendix B).
4. Classified each miss as **(a) genuinely missing**, **(b) deliberately deferred**
   (M8 / documented), or **(c) a representation deviation**.

> **Caveat — my own detectors were wrong three times.** Early automated checks
> missed real dispatches because of (i) turbofish call syntax
> (`chain_hooks_callback::<…>(HOOK_X)`), (ii) helper functions that dispatch
> internally (`prefs_chain(state, sess, HOOK_X, …)`), and (iii) a const the port
> renamed `PROFILE_BLACKLIST` (no leading underscore) so a quoted
> `"_PROFILE_BLACKLIST"` grep found nothing. Each "gap" below was re-verified against
> the source after the detector was fixed; a couple of initial "gaps" turned out to be
> fully present and were **retracted** (notably the profile blacklist — see §Prefs).
> This is the "trust the code, not the ✅/grep" principle applied to my own tooling.

---

## Surfaces verified complete

| Surface | What was checked | Result |
| --- | --- | --- |
| **External API** (level 23) | all 24 `API.php` ops | all handled in `api.rs` (22 in the dispatch `match`, `getFeedIcon` intercepted before it for raw bytes) |
| **Backend RPC** (`backend.php`) | 90 distinct (op, method) the frozen JS + PHP form fields send | all handled; the tool's 1 flagged gap is a commented-out JS call (Appendix B) |
| **public.php** | the 10 `Handler_Public` methods | all handled except `dbupdate` (§Gap G4) |
| **Prefs** | all 45 active `Prefs` constants + `PROFILE_BLACKLIST` (13) + the `_DEFAULTS` table | all present (`prefs.rs`; `PROFILE_BLACKLIST` is byte-for-byte the 13 PHP prefs) |
| **First-party plugins** | all 18 plugins; every hook each registers | all 22 registered hooks have a dispatch site (Appendix E) — **no broken plugin** |
| **Core subsystems** | the ~45 `classes/` files | all have a Rust module except `Db_Migrations` (M8), `Cache_Adapter`/`Logger_SQL` (see §Gaps) |

### Prefs — a retracted false positive

An initial per-pref grep reported `_PROFILE_BLACKLIST` as "missing." It is **not**
missing: the port defines it as a const named `PROFILE_BLACKLIST` (no leading
underscore) at `prefs.rs:91`, and it contains **exactly** the 13 PHP prefs
(`PURGE_OLD_DAYS`, `PURGE_UNREAD_ARTICLES`, `DIGEST_ENABLE`, `DIGEST_CATCHUP`,
`BLACKLISTED_TAGS`, `ENABLE_API_ACCESS`, `ENABLE_API_ACCESS_BROWSER`,
`DEFAULT_UPDATE_INTERVAL`, `USER_TIMEZONE`, `SSL_CERT_SERIAL`,
`DIGEST_PREFERRED_TIME`, `DIGEST_MIN_SCORE`, `_PREFS_MIGRATED`), enforced at the
profile-resolution and write sites (`prefs.rs:483`, `:518`, `pref_ui.rs:566`,
`settings.rs:212`). The ported pref surface is **complete**.

---

## Gaps found (genuinely missing, not already in the plan's Deferred list)

### G1 — ~~22 of 53 `Plugin` hooks have no dispatch site~~ — closed *(low)*

The `Plugin` trait declares **53** `HOOK_*` constants. The 22 that had no
dispatch site are now accounted for:

**Dispatched** (the port is now at dispatch-parity with the PHP reference for
every hook the reference fires; `tests/dispatch_completion.rs`,
`tests/view_pipeline_hooks.rs`, `tests/api_render_article_api.rs` and the
feed-pipeline tests pin the sites):

```
HOOK_ACTION_ITEM                     (shell template slot; index.php:321)
HOOK_ARTICLE_EXPORT_FEED             (syndication export, per article)
HOOK_ARTICLE_LEFT_BUTTON             (view row's `buttons_left` slot)
HOOK_FEED_TREE                       (shell template slot; index.php:175)
HOOK_FORMAT_ARTICLE                  (share plugin's article page)
HOOK_GET_FULL_TEXT                   (create_published_article, fresh owner host)
HOOK_HEADLINES_BEFORE                (view, first page; prepended to `content`)
HOOK_HEADLINES_CUSTOM_SORT_MAP       (API `custom_sort_types`)
HOOK_HEADLINES_CUSTOM_SORT_OVERRIDE  (order_to_override_query, all 3 callers)
HOOK_HEADLINE_TOOLBAR_BUTTON         (view toolbar `plugin_buttons` slot)
HOOK_HOTKEY_INFO                     (hotkey_help, over the info map)
HOOK_LOGINFORM_ADDITIONAL_BUTTONS    (login page, system-plugins host)
HOOK_PREFS_TABS                      (prefs page tab bar)
HOOK_QUERY_HEADLINES                 (view per-row + syndication export)
HOOK_TOOLBAR_BUTTON                  (shell template slot; index.php:263)
HOOK_UNSUBSCRIBE_FEED                (remove_feed, `run_hooks_until(true)`)
HOOK_VALIDATE_SESSION                (session::validate_session, last-result-wins)
```

Plus `HOOK_RENDER_ARTICLE_API` on the **getArticle** route (the port ran it
only in getHeadlines; PHP `API.php:360-366` runs both).

**Not dispatched — by design (not gaps):**
- *Dead in the PHP reference itself:* `HOOK_ARTICLE_FILTER_ACTION` and
  `HOOK_HEADLINE_TOOLBAR_SELECT_MENU_ITEM` (v1) — declared in `Plugin.php`
  but never fired anywhere in the frozen tree (the view uses v2). The port
  keeps the trait methods for contract parity and, like PHP, never fires them.
- *Tied to an unported feature:* `HOOK_SEND_MAIL`, `HOOK_SEND_LOCAL_FILE`
  (the `Mailer`/`lettre` milestone isn't in yet).
- *Deliberate, behavior-identical:* `HOOK_SEARCH` — no stock plugin
  implements it, so PHP always falls through to `_search_to_sql`; the port's
  direct call is equivalent.

**Historical note:** the seven feed-pipeline hooks (`HOOK_FEED_FETCHED`,
`HOOK_FEED_PARSED`, `HOOK_ENCLOSURE_IMPORTED`, `HOOK_FILTER_TRIGGERED`,
`HOOK_ARTICLES_MARK_TOGGLED`, `HOOK_ARTICLES_PUBLISH_TOGGLED`,
`HOOK_HOUSE_KEEPING` (both sites), `HOOK_UPDATE_TASK`) were wired earlier in
`updates.rs` (`tests/feed_pipeline_hooks.rs`).

### G2 — `API::index()` plugin-API-method dispatch is half-wired *(low, latent)*

`PluginHost::add_api_method` / `get_api_method` **are** ported
(`pluginhost/host.rs:384` / `:395`). But the API entry point's catch-all arm
(`api.rs` `other => (STATUS_ERR, …E_UNKNOWN_METHOD…)`) never calls
`get_api_method` — so a plugin can *register* an API method but it is **never
dispatched**. PHP's `API::index($method)` does look it up. **Latent:** no first-party
plugin registers an API method, so nothing observable differs today; it only bites a
third-party plugin that uses `add_api_method`.

### G3 — PHP DB-logging subsystem replaced by `tracing`; no `ttrss_logs` *(low / representation)*

The PHP `Logger` family (`Logger`, `Logger_SQL`, `Logger_Syslog`,
`Logger_Stdout`, `Logger_Adapter`) is not ported. The port uses Rust's
`tracing` (`main.rs:21` sets up `tracing_subscriber` + `EnvFilter`). Consequences:
- Nothing is written to the `ttrss_logs` table (`Logger_SQL` is absent).
- The `Cache_Adapter` abstraction (the named cache-backend trait) is likewise absent
  (the port uses `DiskCache` / `Cache_Local`-equivalents directly).

The frozen frontend does **not** appear to send a "view logs" RPC (the
`diff_rpc_surface` tool found no such op), so this is primarily an observability /
representation difference rather than a broken UI path — but the "logs to the DB"
behavior is genuinely not replicated.

### G4 — `public.php` `dbupdate` (web-based schema update) not ported *(low; M8)*

`Handler_Public::dbupdate` (`Handler_Public.php:612`) — the in-browser "update
database schema" flow — has no port equivalent (not in the `http.rs` public-op
dispatch). It is inherently tied to the schema-migration engine, which is M8 (skipped).
All the other public ops (`login`, `logout`, `rss`/syndication, `feed_icon`,
`cached`, `healthcheck`, `forgotpass`, `pluginhandler`) are handled.

### G5 — ~~`HOOK_VALIDATE_SESSION` chain not run~~ — closed *(low)*

Covered by G1, called out separately because it is a *core-behavior* hook (not a
UI/plugin nicety). Now dispatched: `session::validate_session` takes the
`AppState` and runs the chain after the default checks, keeping the *last*
hook's result (the loop stops at the first `false`) as the return value —
matching PHP `Sessions.php:144`. `tests/dispatch_completion.rs` pins it.

---

## Behavioral partials & deliberate deviations (self-documented, verified)

The category the entry-point scan cannot see. I pulled the plan's **86**
self-documented partials (`simplified` / `no-op` / `placeholder` / `approximated` /
`instead of` / `partial` / `deferred`), then **re-verified each candidate against the
code** — most turn out to be historical (closed by a later milestone). The ones that
are **still true** are below.

### B1 — Feed-date parsing handles only RFC 2822 / RFC 3339 *(moderate)*

PHP `FeedItem_RSS::get_date` / `FeedItem_Atom::get_date` parse with `strtotime()`
(`FeedItem_RSS.php:20`, `FeedItem_Atom.php:22/28/34`) — very permissive (date-only,
partial, relative, epoch-like, and dozens of other forms). The port's `parse_date`
(`core/feed/datetime.rs:15`) tries only RFC 3339 then RFC 2822 and returns `None`
otherwise; the source comment concedes it (`"strtotime() is far more permissive than
this… extend here if the functional tests surface more"`). **Effect:** a `pubDate`/
`dc:date` that isn't strict RFC 2822/3339 (e.g. date-only `2024-01-15`) parses in PHP
but yields `None` here → the article's `updated`/`created` is 0/NULL → wrong or missing
display date **and** broken age-based logic (the *Fresh* feed's
`FRESH_ARTICLE_MAX_AGE` window, sorting, age-based purge). This is the port's own M6a.2
note ("Feed dates simplified to RFC 2822 / RFC 3339"), never revisited.

### B2 — `checkforupdates` is a stub *(deliberate)*

`rpc.rs:707` returns `{ "changeset": [], "plugins": [] }` (comment: "TODO: stub (M2
close-out)… known intentional mismatch"). PHP's `checkforupdates` polls for upstream
tt-rss releases; the port has no PHP-upstream channel, so "check for updates" always
reports up-to-date. (The separate `checkforpluginupdates` at `rpc.rs:619` is handled.)

### B3 — "daemon not updating feeds" stale-warning dropped *(deliberate, accepted)*

`make_runtime_info` (`rpc.rs:910-911`) stubs `daemon_is_running` and `daemon_stamp_ok`
to a constant `1`. PHP derives these from the updater stampfile, and when the 30-min
"not updating" window is exceeded the SPA shows "Update daemon is not updating feeds."
Because the port runs the updater in-process and dropped the stampfile (plan M6c
decision 6), that warning is gone: **if the in-process updater loop stalls, the UI will
not warn.** Accepted in the plan.

### B4 — 30 s feed-content de-dup cache dropped *(deliberate)*

PHP caches fetched feed content in `DiskCache::instance('feeds')` (one
`sha1(feed_url).xml`, pruned after `CACHE_MAX_AGE`) and re-serves it in a short window
to dedupe rapid refetches. Plan M6c decision 5 drops it ("(c) Drop it. No 30 s
feed-content de-dup cache"). **Effect:** concurrent / rapid refetches of one feed hit
the network each time instead of the cache. Low severity (performance, not
correctness).

### B5 — ~~API `getHeadlines` ignores 4 of 19 params~~ — closed *(moderate)*

Closed by the M7.6 work (`api.rs::get_headlines`): `search` is threaded into
`HeadlinesParams` (→ `_search_to_sql`), `force_update` runs the pre-read
`update_rss_feed($feed_id, true)` with the 120 s flood control (a
cache-images feed is not fetched synchronously — its stamps are reset to the
epoch, as in PHP), and `check_first_id` drives the changed-result-set
signal. `filter` is a *dead param in the PHP itself* (`_api_get_headlines`
reads it and never uses it), so ignoring it is faithful.

**Coverage + method.** I param-diffed **all 24 external API ops** (PHP
`$_REQUEST[…]` reads per op method vs the port's `body.get(…)`/helper reads).
`getHeadlines` was the only op with a real param gap (now closed) —
`catchupFeed`, `getArticle`, `getFeeds`, `getCategories`, `setArticleLabel`,
`updateArticle`, `subscribeToFeed`, `getFeedTree` (which hardcodes `mode=2`
and derives `force_show_empty` from `include_empty`) etc. all read what PHP
reads. The automated diff under-counts params the port reads via closures or
multi-line `.get`/`.insert`, so I hand-verified every reported gap — the
*missing* list has no false positives (false negatives possible, and none
material here).

### Verified *closed* by later work — **not** current gaps

Checked and cleared, so they don't read as misses:

| Plan note (dated) | Current state |
| --- | --- |
| `Config::get_version()` "placeholder const" (M2) | now the real frozen-commit version `"26.08-2f7d831f"` (`core/config.rs:21`) |
| `DiskCache` lifecycle "not ported" (M2) | ported — `put`/`get`/`get_mtime`/… (`diskcache.rs:319/324/351/…`) |
| admin logout "not yet ported, returns `E_UNAUTHORIZED` instead of 302+shell" (M2) | ported — `logout_flow` (`http.rs:438`) |
| "NSFW partial (prefs tab, image placeholder, shared render not wired)" (pre-M7.22) | wired in M7.22 — all five NSFW hooks now dispatched (Appendix E) |
| filters-index "`__()` is the identity" (M4/M5) | i18n landed (M10) |
| "cleanup empty categories pass is a no-op" (M4) | **not a gap** — the port matches PHP, whose own pass is a no-op (`//outline[@title]` vs `text`) |

---

## M8-deferred (skipped per your direction — known, not accidental)

These are the `update.php` CLI / schema-migration work the plan tracks as M8, which
you've set aside. Confirmed absent, consistent with that decision:

- **CLI task interface.** The port's binary is **server-only** (`main.rs`, 86 lines,
  no task parsing). None of the `update.php` `options_map` subcommands are wired as
  CLI. Two different reasons, worth keeping separate:
  - **The daemon/update CLI was *deliberately dropped* by design** (decision D1=B,
    strictly single-process, plan M6c.6): no standalone `tt-rss daemon` / one-shot
    `tt-rss update`, no cross-process lock. The **periodic** work (feed updates,
    housekeeping, scheduled tasks) runs in-process via the M6 scheduler
    (`main.rs` daemon loop + `scheduler.rs`), so `feeds` / `daemon` / `update-feed`
    have no CLI form by design, not by deferral.
  - **The admin/ops CLI is *M8-deferred***: `user-list` / `user-add` / `user-remove` /
    `user-set-password` / `user-set-access-level` / `user-enable-api` / `user-exists` /
    `user-check-password`, `opml-export` / `opml-import`, `gen-encryption-key`,
    `gen-search-idx`, `cleanup-tags`, `send-digests`, `debug-feed`, `force-update` /
    `force-refetch` / `force-rehash`, `plugins-list`, `update-schema`.
- **Schema migration engine (`Db_Migrations`).** Not ported. The port declares
  `SCHEMA_VERSION = 151` (`core/config.rs:16`) and connects to a live pool
  (`core/db.rs:54` `connect_url`) but does **not** create or migrate the schema
  itself and ships no bootstrap/seed SQL — it assumes a pre-existing v151 schema
  (the same one the frozen PHP `update.php update-schema` produces).

## Already documented in `plan.md` "Deferred" — confirmed present, not new misses

Re-verified these are the same items the plan already parks (so this scan did not
"discover" them as new): the `Mailer`/`lettre` send (digest + password-recovery
"email" steps are logged-but-not-sent no-ops), the dynamic timezone list (hardcoded
599-id snapshot), `html_to_text` for `tsvector_combined`, `getFeedIcon` response
headers + MIME/`finfo` gates, the `send_headlines_digests` no-op, and the plugin-scoped
config knobs living in the global `Config`.

---

## Appendix

### A. External API ops (`API.php` → `api.rs`) — 24/24 handled

`getVersion`, `getApiLevel`, `login`, `logout`, `isLoggedIn` (port: `isloggedin`),
`getUnread`, `getCounters`, `getFeeds`, `getCategories`, `getHeadlines`,
`updateArticle`, `getArticle`, `getConfig`, `updateFeed`, `catchupFeed`, `getPref`,
`getLabels`, `setArticleLabel`, `shareToPublished`, `unsubscribeFeed`,
`subscribeToFeed`, `getFeedTree`, `getFeedIcon`, plus the `index` plugin-API fallback
(**not** wired — see G2). `getFeedIcon` is intercepted before the dispatch `match`
(`api.rs:95`) because it returns raw image bytes rather than the JSON envelope.

### B. Backend RPC (`backend.php`) — 1 tool false-positive

`browser/diff_rpc_surface.py` (run 2026-09-27) reports 90 distinct app-sent (op,
method) pairs and exactly **one** unhandled: `op=Pref_Filters method=newaction`
(sourced from `CommonFilters.js`). Verified that the source line
(`CommonFilters.js:316`) is inside a `/* … */` comment — the app never sends it, and
PHP has no `newaction` method either (its actions route through `add`/`edit`/
`editSave`). So the backend surface is effectively **complete**.

Two tool limitations to be aware of for future runs: it parses the backend from
`rpc.rs` and only recognizes `"<OP>" => match method {` arms, so it (a) misses the
`PluginHandler` arm (which is `match <dispatch_result>` and *is* handled at
`rpc.rs:356`) and (b) can pick up commented-out JS literals.

### C. public.php (`Handler_Public`) — 9/10 handled

`login`, `logout`, `rss` (syndication), `feed_icon`, `cached`, `healthcheck`,
`forgotpass`, `pluginhandler`, `getprofiles` — handled. `dbupdate` — **not** (G4).
`index` (the landing) is served by the SPA-shell root route rather than as a named op.

### D. Prefs — complete

45 active `Prefs` constants all present in the port; `PROFILE_BLACKLIST` (13) matches
PHP exactly; the `Prefs::_DEFAULTS` table is ported as `PrefDef` (`prefs.rs`). No gap.

### E. Plugin hooks — 53 defined, 23 fired, 30 dead

Fired by the core (22): `HOOK_ARTICLE_BUTTON`, `HOOK_ARTICLE_FILTER`,
`HOOK_ARTICLE_IMAGE`, `HOOK_AUTH_USER`, `HOOK_ENCLOSURE_ENTRY`, `HOOK_FEED_BASIC_INFO`,
`HOOK_FETCH_FEED`, `HOOK_FORMAT_ENCLOSURES`, `HOOK_HOTKEY_MAP`, `HOOK_IFRAME_WHITELISTED`,
`HOOK_MAIN_TOOLBAR_BUTTON`, `HOOK_POST_LOGOUT`, `HOOK_PREFS_EDIT_FEED`,
`HOOK_PREFS_SAVE_FEED`, `HOOK_PREFS_TAB`, `HOOK_PREFS_TAB_SECTION`, `HOOK_PRE_SUBSCRIBE`,
`HOOK_RENDER_ARTICLE_API`, `HOOK_RENDER_ARTICLE_CDM`, `HOOK_RENDER_ENCLOSURE`,
`HOOK_SANITIZE`, `HOOK_SUBSCRIBE_FEED`. Fired by a plugin route (1):
`HOOK_RENDER_ARTICLE` (the `share` public-`get` path). **Every hook any ported plugin
registers is in this fired set** — the ported plugin set has no dead hooks. The 30
dead hooks are listed in G1.

### F. CLI tasks (`update.php` `options_map`) — all M8

The full `options_map` (see §M8) is present in the reference and unported in the
server-only port binary.

### G. Subsystem class → module map

| PHP class | Port | Notes |
| --- | --- | --- |
| `API` | `api.rs` | complete |
| `Article`, `Feeds`, `Counters`, `Labels`, `Prefs`, `Prefs::*` | `article.rs`, `feeds.rs`, `counters.rs`, `labels.rs`, `prefs.rs`, `pref_*.rs` | complete |
| `Sanitizer`, `FeedParser`, `FeedItem{,_Atom,_RSS}`, `FeedEnclosure`, `RSSUtils` | `sanitizer.rs`, `core/feed`, `rss_utils.rs` | complete |
| `Opml`, `Digest`, `Scheduler`, `Sessions`, `TimeHelper`, `UrlHelper`, `UserHelper`, `Crypt`, `DiskCache`, `Cache_Local` | `opml.rs`, `digest.rs`, `scheduler.rs`, `session.rs`, `time.rs`, `urlhelper.rs`, `users.rs`, (crypt), `diskcache.rs` | present |
| `Mailer` | — | **deferred** (`lettre`); sends are no-ops |
| `Logger`, `Logger_SQL`/`_Syslog`/`_Stdout` | `tracing` | **representation deviation** (G3); no `ttrss_logs` |
| `Cache_Adapter` | — | trait abstraction absent; `DiskCache` used directly (G3) |
| `Db_Migrations` | — | **M8**; port assumes pre-existing schema v151 |
| `Handler_Public::dbupdate` | — | **G4** |
