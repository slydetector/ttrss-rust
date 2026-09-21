# tt-rss PHP → Rust Backend: Migration Plan (project: **ttrss**)

Delivery is tracked as a milestone ladder (**M0–M9**, §3). §3a keeps the original
per-subsystem phase notes (P0–P8) as the detailed work-item reference. Locked
decisions: §6.

---

## 1. Codebase Analysis

### 1.1 Inventory (what exists in `tt-rss/`)

| Area | Size | Notes |
| --- | --- | --- |
| Core PHP (`classes/`, `include/`, entry points) | ~43.5k LOC, 130 files | `classes/` alone is 58 files / 21.2k LOC |
| Bundled plugins (`plugins/`) | ~2.8k LOC, 32 files, 15 plugins | auth_internal, auth_remote, af_*filters, nsfw, note, share, bookmarklets, cache_starred_images, hotkeys_*, etc. |
| JS frontend (`js/`) | ~9.6k LOC, 27 files | Dojo 1.x + "fox" custom widgets; SPA driven by `backend.php` |
| Dojo libs (`lib/`) | ~13 MB | vendored dojo/dijit, MiniTemplator, gettext loader |
| SQL (PostgreSQL) | base `schema.sql` (407 LOC) + **151 numbered migrations** | ~30 tables; MySQL files are empty — **this fork is PostgreSQL-only** |
| Locales | ~30 gettext locales (`.po`/`.mo`) | i18n via `__()` / `T_sprintf()` |
| Themes | light, night, night_blue, compact, high-contrast | prebuilt CSS + LESS sources |
| Tests | PHPUnit unit, functional, integration (live API via curl), Selenium | fixtures in `tests_functional/` |
| Entry points | 7 | `index.php`, `prefs.php`, `backend.php`, `public.php`, `api/index.php`, `update.php` (CLI), `update_daemon2.php` (daemon) |

### 1.2 Runtime architecture

The system is **three runtimes sharing one codebase**:

1. **Web server (PHP-FPM)** with five dispatch surfaces:
   - `index.php` / `prefs.php` — per-session HTML *shell* pages for the Dojo SPA (ETag/conditional revalidation, embedded CSRF token, theme/JS/CSS assembly). The SPA does all dynamic work via XHR.
   - `backend.php` — **internal JSON RPC**: `POST op=<HandlerClass>&method=<m>&csrf_token=…`. Handlers: `RPC` (legacy), `Feeds`, `Article`, `Counters`, `Pref_Prefs`, `Pref_Feeds`, `Pref_Filters`, `Pref_Labels`, `Pref_Users`, `Pref_System`, plus plugin-registered handlers. CSRF-validated, session-scoped, access-level-gated.
   - `public.php` — unauthenticated/public surface: `login`, `logout`, `rss` (Atom/JSON syndication via access key), `forgotpass` (password recovery w/ captcha + email), `dbupdate` (web schema migrator), `cached` (disk cache files), `feed_icon`, `healthcheck`, and `plugin--method` public plugin methods.
   - `api/index.php` — **external JSON API** (API level 23) used by third-party clients (ttrss-reader, mobile apps): `op=<method>` → `API` class methods (`login`, `logout`, `isLoggedIn`, `getVersion`, `getApiLevel`, `getConfig`, `getFeeds`, `getCategories`, `getHeadlines`, `getArticle`, `updateArticle`, `updateFeed`, `catchupFeed`, `subscribeToFeed`, `unsubscribeFeed`, `getUnread`, `getCounters`, `getPref`, `getLabels`, `setArticleLabel`, `getFeedTree`, `getFeedIcon`, `shareToPublished`, plus plugin-registered API methods). JSON envelope: `{seq, status, content}`.
   - Static assets: `js/`, `lib/`, `themes/`, `images/`, `locale/`, `manifest.webmanifest`.
2. **CLI tool** (`update.php`): ~25 subcommands — user management (`user-add/remove/set-password/set-access-level/enable-api/check-password/list`), `opml-export/import`, `update-schema`, `feeds`, `daemon`, `daemon-loop`, `update-feed`, `debug-feed`, `force-update`, `force-refetch`, `force-rehash`, `cleanup-tags`, `gen-search-idx` (PostgreSQL FTS backfill), `gen-encryption-key`, `plugins-list`, `send-digests`, plus plugin-registered commands.
3. **Update daemon** (`update_daemon2.php`): master process that `pcntl_fork`s up to N worker processes; each worker execs `update.php --daemon-loop --task N`; lockfiles (`update_daemon.lock`, `update_daemon-<pid>.lock`) for liveness; child runtime cap; spawn interval (min 60 s).

### 1.3 Core subsystems (the actual domain)

| Subsystem | Key files | Responsibility |
| --- | --- | --- |
| **Config** | `Config.php` (757 L) | ~60 env-var (`TTRSS_*`) + type-hinted settings; self-URL detection (proxy-aware); sanity checks; version detection (git) |
| **Prefs** | `Prefs.php`, `Pref_*` | Per-user, per-profile settings; registry table `ttrss_prefs` (name/type/default/help); ~50 keys; profiles (`ttrss_settings_profiles`) |
| **Users & auth** | `UserHelper.php`, `Auth_Base.php`, `IAuthModule(2)`, plugins `auth_internal`/`auth_remote` | Plugin-chained auth; password hashes (SSHA-512/256, MODE2, SHA1X, SHA1 — PHP crypt formats); TOTP (OTPHP); app passwords; access levels (disabled/readonly/user/poweruser/admin); auto-create/auto-login; session validation hook |
| **Sessions** | `Sessions.php` | DB-backed (`ttrss_sessions`), base64, optional **xchacha20poly1305-ietf** encryption (`Crypt.php`, sodium); cookie lifetime, remember-me |
| **Feeds** | `Feeds.php` (2413 L) | Feed/category trees, subscription flow (fetch→parse→dedup→store), icon fetching, access keys (syndication), catchup, purge, counters (w/ cache tables), virtual feeds (starred/published/fresh/all/recently-read/archived/error/labels), search→SQL, ordering |
| **Articles** | `Article.php`, `Labels.php`, `Counters.php` | Per-user entry state (unread/marked/published/score/note), tags, labels (colors, feed-id mapping), enclosures, OG/flavor images, catchup, orphan purge |
| **Feed pipeline** | `RSSUtils.php` (2019 L), `FeedParser.php`, `FeedItem_*.php`, `UrlHelper.php` (610 L) | Fetch (Guzzle: proxy, timeouts, 429 throttle, conditional, gzip, binary detect), parse (RDF/RSS/Atom + namespaces), stable article hashing, GUID fallback, **article filters → actions** (score/label/tag/hide), enclosure + favicon caching to disk cache, failed-feed auto-disable, housekeeping tasks |
| **Sanitization** | `Sanitizer.php` (319 L) | DOMDocument-based HTML sanitizer: allowed elements, disallowed attrs, iframe whitelist (plugin hook), word highlighting, URL rewriting to disk cache, image stripping |
| **HTTP client** | `UrlHelper.php` | Guzzle wrapper; URL validation; **SSRF guard** (disallowed private/reserved IPs, redirect resolution); `srcset` parse; YouTube id extraction |
| **Output** | `Templator` (MiniTemplator in `lib/`), `OPML.php`, `Digest.php`, `Mailer.php` | `${VAR}` / `$BeginBlock` templates for Atom export, email digests, password reset; OPML import/export; SMTP via PHP `mail()` + plugin hooks |
| **Scheduler** | `Scheduler.php`, `cron-expression` | Named tasks with cron expressions (per `ttrss_scheduled_tasks`), run during housekeeping; core tasks: purge orphans, cache expire, disable failed feeds, icon cleanup, login-limit log, error-log expire, lock-file expire, send digests |
| **Plugin system** | `PluginHost.php` (943 L), `Plugin.php` (743 L) | **~40 hook points** (auth, fetch, parse, headlines, render, sanitize, prefs tabs, toolbar, API methods, virtual feeds, CLI commands, scheduled tasks, public methods, CSS/JS injection, session validation…); per-user plugin enablement; `ttrss_plugin_storage`; plugin SQL migrations (`ttrss_migrations_plugin_*`); plugin update/install via git |
| **Search** | `Feeds::_search_to_sql`, `af_psql_trgm` plugin | PostgreSQL full-text (`tsvector_combined`), per-language; trigram fallback plugin |
| **Logging** | `Logger.php`, `Debug.php`, `Errors.php` | Event log in DB (`ttrss_error_log`), stdout/syslog destinations; JSON error envelope |

### 1.4 Data model (PostgreSQL, ~30 tables)

`ttrss_users`, `ttrss_app_passwords`, `ttrss_feeds` (self-referencing `parent_feed`, auth creds, update meta), `ttrss_feed_categories` (tree), `ttrss_archived_feeds`, `ttrss_entries` (shared articles, tsvector), `ttrss_user_entries` (per-user state, `int_id` cache), `ttrss_entry_comments`, `ttrss_tags`, `ttrss_enclosures`, `ttrss_filters2` + `_rules` + `_actions`, `ttrss_filter_types`, `ttrss_filter_actions`, `ttrss_labels2`, `ttrss_user_labels2`, `ttrss_prefs` (registry), `ttrss_user_prefs`/`ttrss_user_prefs2`, `ttrss_settings_profiles`, `ttrss_sessions`, `ttrss_counters_cache`, `ttrss_cat_counters_cache`, `ttrss_error_log`, `ttrss_scheduled_tasks`, `ttrss_scheduled_updates`, `ttrss_feedbrowser_cache`, `ttrss_access_keys`, `ttrss_linked_instances`, `ttrss_linked_feeds`, `ttrss_plugin_storage`, `ttrss_version`, + plugin migration tables.

Schema evolution: `Db_Migrations` runs numbered `.sql` files (split on `;`, per transaction), version stored in `ttrss_version.schema_version`; same mechanism reused per-plugin. **SCHEMA_VERSION = 151.**

### 1.5 Compatibility surface (what a Rust port must preserve)

| # | Surface | Consumers |
| --- | --- | --- |
| A | PostgreSQL schema v151 + migration semantics | existing data; coexistence with PHP instances |
| B | `backend.php` internal JSON RPC (exact JSON shapes, loose param types) | the Dojo JS frontend (27 files, ~9.6k LOC) |
| C | External JSON API level 23 (`{seq,status,content}`, error codes) | third-party clients (ttrss-reader, mobile, scripts) |
| D | `public.php`: login/logout flows, Atom/JSON syndication (byte-stable-ish feed output), feed icons, cached file serving, healthcheck, password recovery, web dbupdate | users, feed readers (syndication), ops |
| E | HTML shells (`index.php`, `prefs.php`, login/OTP forms) incl. ETag 304 behavior, CSRF embedding, plugin CSS/JS injection | browsers |
| F | Filesystem layout: `themes/`, `js/`, `lib/`, `images/`, `locale/`, `cache/{images,export,upload,feed-icons}`, `lock/` | static serving + daemon |
| G | Credential formats: SSHA-512/256, MODE2, SHA1X, SHA1 password hashes; TOTP secrets; app passwords; session rows (encrypted format) | existing users; login interop |
| H | CLI (`update.php`) subcommands + daemon lockfiles/behavior | ops scripts, systemd, existing Docker images |
| I | i18n: gettext `.po`/`.mo` (30 locales), Accept-Language negotiation, `T_sprintf` | all UI strings incl. API responses |
| J | Config: `TTRSS_*` env vars + optional `config.php` (env overrides) | deployment |

---

## 2. Target Architecture

### 2.1 Workspace layout

```
ttrss/                        # the repo (was tt-rust)
├── Cargo.toml                # workspace (GPL-3.0)
├── crates/
│   ├── ttrss/                # the app: lib + `ttrss` binary; `src/core/` = pure domain
│   ├── test-db/              # dedicated Postgres test server (testcontainers) + fixtures
│   └── harness/              # record/replay/diff oracle harness (clap binary)
├── oracle/                   # record/replay tooling: compose stack, seed, golden data
├── docs/                     # plan, research reports, spike decisions
└── tt-rss/                   # frozen PHP reference (commit 2f7d831) — the spec
```

Binary: one **`ttrss`** binary with subcommands `serve` (web server), `update` (CLI ops), `daemon` (long-running updater); `update --daemon` preserved for exact PHP parity. Legacy `--opt:value` flag surface kept (see RESEARCH-REPORT §2.5 for the full keep/propose list — **to be confirmed before implementation**).

### 2.2 Crate choices (vetted, mainstream — no custom infrastructure)

| Concern | Crate(s) | Notes |
| --- | --- | --- |
| HTTP server | **axum** + tokio + tower | matches PHP entry-point dispatch; middleware for sessions/CSRF/i18n |
| HTTP client (feeds) | **reqwest** (rustls) | Guzzle replacement; proxies, timeouts, 429 handling |
| Postgres | **sqlx** (runtime queries) | codebase is full of dynamic SQL (search, filters, counters); runtime SQL is the pragmatic fit. `sqlx::query` + manual row mapping |
| Serialization | serde / serde_json | |
| XML parsing | **xml** (pull parser) — or `quick-xml` | port of `FeedParser`/`FeedItem_*`; RDF/RSS/Atom quirks are tt-rss-specific, so a thin custom parser over a vetted pull parser beats `feed-rs` for fidelity |
| HTML parse/sanitize | **html5ever** + **markup5ever** (+ `ammonia` as reference) | PHP DOMDocument replacement; sanitizer logic ported 1:1 |
| Crypt | **chacha20poly1305** (RustCrypto; `XChaCha20Poly1305`), **sha2**/**sha1**, `sha-crypt` | matches sodium `xchacha20poly1305_ietf`; port PHP `$6$`/`$5$`/legacy crypt formats |
| TOTP | **totp** (RustCrypto ecosystem) | replaces OTPHP; verify PHP-generated codes |
| QR codes | **qrcode** | replaces chillerlan/php-qrcode (OTP setup) |
| Email | **lettre** | replaces PHP `mail()`; SMTP + optional sendmail |
| Cron | **croner** | replaces dragonmantank/cron-expression |
| Time/i18n | **chrono** + **iana-time-zone**, **gettext** (rust-gettext, pure-Rust `.mo` loader) + `accept-language` | TimeHelper port; gettext replaces PHP gettext lib |
| Templates | **minijinja** | MiniTemplator `${VAR}`/`$BeginBlock` syntax is trivially translatable; or a ~150-line MiniTemplator port — either way tiny |
| CLI | **clap** | |
| Locks | **flock** | lockfile semantics for daemon |
| Logging | **tracing** + tracing-subscriber (file/stdout/syslog) | Logger/Debug replacement; DB event log kept |
| Misc | url, idna, base64, rand, regex, cookie, image, mime_guess, md5 (ETag) | |

### 2.3 Key design decisions

1. **PostgreSQL-only**, reusing the exact base schema + 151 migration files. The Rust migration runner mirrors `Db_Migrations` (same table names incl. `ttrss_migrations_plugin_*`) so PHP and Rust can share/interoperate on one database.
2. **Serve the existing JS frontend unchanged.** Rust serves `js/`, `lib/`, `themes/`, `images/` as static files and replicates the shell pages + `backend.php` RPC byte-shape-for-byte. This is the single biggest scope lever: no frontend rewrite.
3. **Loose-type fidelity layer.** PHP's dynamic typing (`"f"` vs `false`, string ints, checkbox `"on"`, `_param_to_bool`, `sql_bool_to_bool`) is a real compatibility hazard → a small type-coercion module implementing the exact coercion rules + property tests against recorded PHP behavior.
4. **Plugin system re-implemented natively in Rust** (trait-based hooks mirroring `PluginHost`), first-party plugins ported; **community PHP plugins cannot run** (see open question #2 — this is the biggest product risk).
5. **One process, two binaries' worth of capability**: web server (tokio) and update workers (tokio tasks) share `ttrss::core`; lockfiles kept for ops compatibility.

---

## 3. Delivery Plan — milestone ladder (M0–M9)

Restructured per `research-report.md` (strangler-fig / vertical-slice / golden-master methodology). **Rule: every milestone ends with a live, demoable end-to-end system and a green Rust test suite (unit/DB tests, PHP test-parity) for everything it claims. *(The oracle-diff harness is retired 2026-09-19 — read-only scope plus wall-clock fixture decay made its maintenance cost outweigh the signal; see the note in `oracle/README.md`. Historical “harness match” references below are records of when they held.)*** Port order = usage-census order (hot ops first), respecting the read → write → background ladder. §3 (this section) is the **delivery order** (which milestone to demo next); §3a is the **detailed work-item reference** (per-subsystem, consulted as each milestone is started).

| M | Milestone | Demo / exit criteria |
| --- | --- | --- |
| **M0** | Oracle + harness | Dockerized PHP oracle (pinned `sha-` image) up with populated **golden DB** (`pg_dump` fixture); request-corpus recorder; structural JSON/HTML/Atom diff tool with non-determinism normalizers; harness proven against a stub Rust server; PHP perf baseline recorded |
| **M1** | Walking skeleton | `ttrss serve` serves all static assets + login page; real login (`auth_internal`, SSHA-512) against golden DB; SPA shell renders in a browser; DB sessions (encrypted) read/write |
| **M2** | **Read-only UI on real data** (confidence marker) | Feed tree + counters + headlines + article content + search + labels correct on golden dataset; `backend.php` read ops + external API read ops (level 23) pass diff harness; Atom/JSON syndication, feed icons, ETag shells |
| **M3** | Mutations I: article state | mark/publish/star/unread, catchup (feed/cat/all), notes, scores, labels, tags; harness checks responses **and** resulting DB state vs PHP |
| **M4** | Mutations II: configuration | prefs/profiles, feed CRUD (create/subscribe → M6) + categories + icons + ordering + OPML, filter CRUD + execution, label CRUD, access keys |
| **M5** | External API completeness + admin | Full level-23 API; user admin (`Pref_Users`), system (log, test email), password recovery (OTP/QR shipped in M4) |
| **M5a** | **External API client compatibility** (M5 scope completion; gaps found by the tt-rss-android gap analysis, 2026-09-15) | The official Android client (`org.fox.ttrss`) works end-to-end against the port: `POST /api/` routed; `getFeeds` / `getCategories` / `getHeadlines` return the exact entry shapes and param semantics the client parses; a scripted replay of the client's full call sequence diffs clean vs the PHP oracle |
| **M6** | The engine: updates | Fetch/parse/hash/dedup/filters/enclosures/favicons, counter-cache maintenance, failed-feed disabling, cron housekeeping, digest emails; **`Feeds::add` (feed subscribe, moved from M4)**. **Hosted in the server's tokio runtime (single process, not a separate daemon); `ttrss update` CLI retained for one-shot/standalone** — see `docs/unified-architecture.md`. Verified via ported PHP piece tests + a batch smoke test (golden update-run diff dropped). |
| **M7** | Plugin system + all first-party plugins | Rust plugin API (static loading; contract pinned 1:1 with PHP — loader swappable, WASM route preserved); all 18 first-party plugins; plugin storage portable (PHP-serialize interop, read+write); installer deferred (listing only, already landed in M4). Decomposed in §3a Phase 7: M7.1–M7.21 |
| **M8** | CLI/daemon parity + migrations | All `update.php` subcommands, lockfile semantics, `--update-schema` reusing exact `.sql` files, exit codes |
| **M9** | Hardening & performance | Fuzz (sanitizer/parser/URL), security matrix, perf vs PHP baseline (target: faster), Docker images, docs, cutover runbook |

### Where we are (current state, 2026-09-11)

> **The single reference for "what's done vs. not done / what's next."** Per-milestone
> detail lives in the status sections below (M0–M2) and the §3a Phase 4 status bullets
> (M3–M5); if those drift from this table, **this table wins**. ✅ = complete, ⬜ = not started.

| M | Status | Done | Remaining (tracked in §3a / below) |
| --- | --- | --- | --- |
| **M0** | ✅ | Oracle + record/replay/diff harness (golden DB, corpus, normalizers). | — |
| **M1** | ✅ | Serve static + login/OTP, SPA shell, encrypted DB sessions. | — |
| **M2** | ✅ | Read-only UI (tree/counters/headlines/content/search/labels), `Feeds::view` + `search`, `RPC::getAllCounters`, level-23 API **reads**, Atom/JSON syndication, feed icons, ETag/304 shells. | Disabled corpus steps (intentional, not bugs): `checkforupdates`, `previewDigest`, web admin logout 051–052, otpuser 058–061. |
| **M3** | ✅ | Article-state mutations (mark/star/publish/unread, catchup feed/cat/all, score, tags, labels, `getmetadatabyid`, `redirect`) — 21/21 clean vs the M3 `db_state` corpus. | `notes` handler → M7 (plugin). |
| **M4** | ✅ | `Pref_Prefs` (profiles, app passwords, save+reset, `index`/`index_auth` + OTP writes), feed CRUD + categories + icons + ordering + access keys + OPML, filter CRUD + execution, label CRUD. | `Feeds::add` (subscribe) → M6 (needs fetch/parse). |
| **M5** | ✅ | Level-23 API **writes** + `logout`, `Pref_Users`, `Pref_System`, password recovery, batch-add-feeds, case-insensitive dispatch. | `updateFeed`/`subscribeToFeed` → M6 (done in M6c.5); plugin management → M7; client-compat gaps → **M5a**. |
| **M5a** | ✅ | M5a.1–M5a.4: `POST /api/` routing, `getCategories` `enable_nested`, `getHeadlines` `has_sandbox`, faithful `getFeeds` | — |
| **M6** | ✅ | M6a.1 (`FeedParser`, 20) + M6a.2 (`FeedItem` field extraction, 35) + M6a.3 (enclosures/`xml:base`/`c14n` + `tests_functional/FeedItem{Atom,Common}Test`, 92) + M6a.4 (gzip `is_gzipped`/`gzip_decode`, 9) + **M6b.1 (RSSUtils pure helpers: GUID/srcset/function_enabled/labels + filter-action helpers, 48)** + **M6b.2a (`calculate_article_hash`, 3) + M6b.2b (pipeline core `update_rss_feed`: hash→dedup→upsert→metadata, 7) + M6b.2c (filter/label/tag application: `load_filters` + eval→score/unread/marked/publish/catchup, labels [other]+[filters], tags + `ignore-tag` + blacklist, 8)** + **M6c.1 (fetch layer: `UrlHelper::fetch` → `reqwest`, 11)** + **M6c.2 (single-feed workhorse `fetch_one_feed`: 304/429/error stamping + conditional-refetch decision, 6)** + **M6c.3 (daemon/scheduler loop: due-feed selection/claim, `Semaphore(2)` `JoinSet`, graceful shutdown, 8)** + **M6c.4 (housekeeping: `croner`-based cron scheduler + per-feed `purge_feed`, 10)** + **M6c.5 (on-demand / web-triggered updates + `Feeds::add` first update: `Feeds::_subscribe` engine + `Feeds::add` + API `subscribeToFeed`/`updateFeed` + `rss_utils::update_basic_info`, 4)** + **M6c.7 (batch smoke + config/pool wiring)** green. | — (digest **email send** awaits the `lettre` Mailer — tracked in §3a Deferred; M6c.6 CLI subcommands dropped per D1=B). |
| **M7** | 🚧 M7.1–M7.16 done | M7.1 (plugin contract, registry, lifecycle — substrate, no behavior change), M7.2 (plugin storage on `core::phpser` — PHP-serialize interop, read+write: the "storage portable" exit criterion), M7.3 (`PluginHandler` backend dispatch + the public `op=pluginhandler` endpoint: gates, the exact JSON error envelopes, plugin-level CSRF), M7.4 (`note` — article notes: `edit`/`setNote` + the button-row hook; 11 DB tests), M7.5 (`nsfw` — tag-based content hiding: `save` + the render/image/prefs-tab hooks; 10 tests), M7.6 (`no_iframes` — `HOOK_SANITIZE` iframe removal; the read-side `sanitize` is now a single async entry (sync `sanitize_core` + hook stage) wired into all four read-side sites; 7 tests), M7.7 (`shorten_expanded` — `get_js`/`get_css`-only, assets `include_str!` from the reference), M7.8 (`af_zz_vidmute` — `get_js`-only, same), M7.9 (`af_comics` — the update-pipeline `HOOK_ARTICLE_FILTER` wiring + the 12-filter port), M7.10 (`auto_assign_labels` — the `HOOK_ARTICLE_FILTER` label auto-assign + the pipeline seeding/storing `article["labels"]`), M7.11 (`af_youtube_embed` — `HOOK_RENDER_ENCLOSURE` player + the `HOOK_IFRAME_WHITELISTED` YouTube whitelist; the read-side `sanitize` gives whitelisted iframes the loose PHP sandbox value, closing the M5a.3 residual), M7.12 (`cache_starred_images` — the `@hourly` cache queue + `@daily` expiry scheduled tasks: scan starred-uncached articles, download media to the `starred-images` `DiskCache` with a `.status` attempt file (cap 5), mark done; expiry unlinks files for gone articles; `DiskCache` substrate + `HostCtx::cache_dir` + `Config::base_cache_dir`; **scheduler-driven** (not a mark-toggled hook, correcting the draft); the M6c.4 scheduler variant that invokes the tasks is deferred; 3 DB tests), M7.13 (`share` — article sharing by unique URL: the key is a short uuid in `ttrss_user_entries.uuid` (keyed `int_id` + `owner_uid`); protected path: `newkey`/`unshare`/`clearArticleKeys`/`shareDialog`; public path (no account): `get` (serves the article page by key, or 404); the `MethodOutput::Response` variant (custom status + Content-Type) added for the public route; the `format_article` port reuses the existing sanitizer/article/util functions; 9 DB tests), M7.14 (`bookmarklets` — prefs-tab UI with two `javascript:` bookmarklets (subscribe + share) + the `sharepopup` public method (creates a published article + closes the window); the `subscribe` public method (the full `Feeds::_subscribe` engine: all result codes 0-5, the "Edit subscription options" link, the multiple-feed-URLs select form); `HostCtx` now carries `&Config` (the PHP `Config` singleton equivalent), un-deferring `subscribe` + enabling the future `auth_internal`/`auth_remote` plugins; 7 DB tests), M7.15 (`af_psql_trgm` — the trigram-similarity filter: `save` (stores similarity/min_title_length/enable_globally), `showrelated` (the Related-articles dialog: `SIMILARITY` over 2 weeks, top 10), `hook_article_filter` (the core filter: `pg_trgm` availability → gating → threshold → min-length → `MAX(SIMILARITY)` over 1 day → `force_catchup`), `hook_prefs_tab` (the AccordionPane UI + `pg_trgm` availability check), `hook_prefs_edit_feed`/`hook_prefs_save_feed` (per-feed enable/disable), `hook_article_button` (the `bookmark_outline` icon); `CREATE EXTENSION pg_trgm` added to the test fixtures; 8 DB tests), M7.16 (`toggle_sidebar` — the main-toolbar `chevron_left` button + the `Plugins.Toggle_Sidebar.toggle` JS; 2 DB tests) | Decomposed per §3a Phase 7 (M7.1–M7.21 across 8 phases; all 18 first-party plugins, static loading). Next: M7.17. |

**Next = M6 (the engine)**, decomposed per `docs/unified-architecture.md` §4:

- **M6a — parser** (`FeedParser` + `FeedItem_*`) — pure, architecture-independent. **M6a.1–M6a.4 done** (FeedParser, FeedItem fields, enclosures/`xml:base`/`c14n`, gzip) — the whole parser milestone is complete; next is M6b (the pipeline).
- **M6b — deterministic pipeline** (GUID/hash → dedup → upsert → filter → counters). **M6b.1 ✅ pure helpers** — `rss_utils` GUID/srcset/function_enabled/labels + the filter-action helpers in `core::filters`. **M6b.2a ✅ `calculate_article_hash`** (the article dedup key, pure). **M6b.2b ✅ pipeline core** (`ttrss::updates::update_rss_feed`: parse → per-article guid/hash → dedup → upsert `ttrss_entries` + `ttrss_user_entries` → stamp feed metadata; the fetch is decoupled so the core replays recorded feed XML). **M6b.2c ✅ filter/label/tag application** (`load_filters` + per-article `eval_article_filters` → score/unread/marked/publish/catchup, labels [other]+[filters], tags + `ignore-tag` + blacklist); then M6c = fetch/daemon.
- **M6c — daemon/scheduler shell** (tokio worker loop, fetch, housekeeping, graceful shutdown).

**M6a (incremental):** port `tt-rss/classes/FeedParser.php` (247 L) + `FeedItem.php` (24 L) over `roxmltree` (now a dependency; the existing `quick-xml` is a pull parser, not a namespace DOM). Handle RDF/RSS/Atom + namespaces + content-encoding/gzip detection. Replicate 1:1: `tests/FeedParserTest` + `tests/FeedItemTest` (pure) + `tests_functional/FeedItem{Atom,Common}Test`.

- **M6a.1 ✅** `FeedParser` — type detection + feed-level title/link + item collection (`tests/FeedParserTest`, 20 green).
- **M6a.2 ✅** `FeedItem` core field extraction — RSS/Atom `get_id`/`get_date`/`get_link`/`get_title`/`get_content` + `normalize_categories` (`tests/FeedItemTest`, 35 green). Feed dates simplified to RFC 2822 / RFC 3339; `xml:base` rewriting + `c14n()` delivered in M6a.3.
- **M6a.3 ✅** enclosures, `xml:base` rewriting, `c14n()`, + `tests_functional/FeedItem{Atom,Common}Test` (42 + 50 green).
- **M6a.4 ✅** content-encoding/gzip detection — `rss_utils::is_gzipped` (magic bytes `\x1f\x8b\x08`) + `rss_utils::gzip_decode` (`flate2`); `RSSUtilsTest` is_gzipped (8) + `FunctionsTest` gzdecode (1) green. (Transport-level `Content-Encoding: gzip` is handled by `reqwest`'s `gzip` feature, mirroring PHP curl `CURLOPT_ENCODING`.)
- **M6b.1 ✅** RSSUtils pure helpers — `rss_utils`: `make_guid_from_title` (reuses `util::strip_tags`), `decode_srcset`/`encode_srcset` (+ `SourceItem`), `function_enabled`, `labels_contains_caption` (+ `Label`). The filter-action helpers `has_article_filter_action`/`find_article_filter_actions`/`calculate_article_score` live in `core::filters` (cohesive with the M5 `eval_article_filters` + `FilterAction`, which already had the `test_eval_article_filters_*` cases). `RSSUtilsTest` make_guid (10) + srcset (14) + function_enabled (2) + labels (6) + filter-action (16) = 48 green. NOTE: `function_enabled` is always `true` (Rust has no `disable_functions`); `decode/encode_srcset` duplicate the pre-existing private copies in `diskcache.rs`/`sanitizer.rs` (a later consolidation, not this increment).
- **M6b.2a ✅** `calculate_article_hash` — the article dedup key (`rss_utils::calculate_article_hash`): a stable field-ordered SHA-1 over title/content(link)/tags/author/score_modifier/language/timestamp/num_comments/enclosures, built on `util::sha1_hex` (lowercase-hex SHA-1, no plugins loaded → empty plugin-name prefix). 3 inline cases.
- **M6b.2b ✅** pipeline core (`ttrss::updates`) — `update_rss_feed`/`process_feed`: parse feed XML → per article: GUID (id→link→`make_guid_from_title`), dedup by `content_hash`, upsert `ttrss_entries` (guid in 3 forms, `tsvector_combined`, `content_hash`) + `ttrss_user_entries` (score/marked/unread; no filters → score 0) → stamp feed success/error metadata. Replays the PHP suite's mock feed (`tt-rss/tests_integration/feed.xml`) via `tests/update_rss_feed.rs` (7 cases, 1:1 from `UpdateRssFeedTest.php`). The fetch is M6c; `tsvector_combined` uses `strip_tags(title) . " " . strip_tags(content)` in place of PHP's `Html2Text::convert` (functionally equivalent for FTS; the suite doesn't assert on it). Filter/label/tag application is M6b.2c.
- **M6b.2c ✅** filter/label/tag application (`ttrss::updates`) — `load_filters` (the owner's enabled filters scoped to the feed: a rule naming the feed directly, one in its category chain, or unrestricted; kept only when it has a rule + an action) + `blacklisted_tags` (the user's `BLACKLISTED_TAGS` pref). Per article: `eval_article_filters` → a `filter` action drops the article (base entry kept, no user entry); otherwise `score`/`catchup` drive `unread`, `mark`/score>1000 → `marked`, `publish` → `published`; re-score + `mark_unread_on_update`; labels [other] (re-apply existing) + [filters] (`label` action); tags (feed categories + `tag`/`ignore-tag` actions, minus the blacklist, re-normalized). 8 new DB cases in `tests/update_rss_feed.rs` (filter/score×2/catchup/tag/ignore-tag/blacklist/label). NOTE: the PHP's `$entry_force_catchup` guard (the plugin “force catchup” hook) is omitted — always `false` with no plugins, so both its uses are no-ops until M7 (see the NOTE in `updates.rs`).

**M6c (incremental — sub-decomposition, §5 decisions resolved):** the fetch + daemon/scheduler shell — the “only non-deterministic piece” (§4) that turns M6b's XML-in pipeline into a live-updating reader. Built bottom-up; each sub-increment is verifiable and maps to a §3 keep/replace/simplify commit. All six §5 decisions are resolved (see the decision list below): **D1 = B** (strictly single-process; M6c.6 dropped), **D2 = a** (on-demand blocks, un-gated by the loop's concurrency), **D3 = b** (one shared DB pool, sized as needed), **D4 = N = 2** (config-able), **D5 = c** (no 30s feed-content cache; rely on 304), **D6 = c** (drop the daemon-status files; stub the `getruntimeinfo`/level-23 API responses).

- **M6c.1 ✅ — Fetch layer** (`UrlHelper::fetch` → `reqwest`). Async HTTP fetch: timeout, redirects (followed; effective URL + connecting IP recorded), conditional requests (`If-Modified-Since`/`If-None-Match` → `304`, skipped for POST), gzip, proxy (CONNECT tunnel + `no_proxy` + `HTTP_PROXY_*`), per-feed basic/digest auth, User-Agent (tt-rss agent), POST form, max-size limit. Returns a `FetchResult { body, error, error_code, effective_url, effective_ip, last_modified, content_type }`. Built on `reqwest` (workspace, `rustls-tls` + `gzip`). The **conditional-refetch decision** (`MAX_CONDITIONAL_INTERVAL` 12h) and the **HTTP 429 throttle** (`HTTP_429_THROTTLE_INTERVAL`) are *daemon* concerns (M6c.2/3) — the fetch layer only surfaces the `error_code`/`last_modified`; the binary-response detect is the parser's job (M6b: non-XML fails to parse). PHP ref: `classes/UrlHelper.php` (`fetch`). Tests: 11 — a **loopback mock-HTTP-server** (a `ProxyHandler` + an in-test HTTP/1.1 server, since the network is isolated) for the end-to-end (200 body, `304 Not Modified`, redirects, non-2xx error code, `no_proxy`, basic auth) + unit tests (default User-Agent, proxy/CONNECT, POST form, `max_size` limit, `Last-Modified` parse). Commit: “fetch layer is the only non-deterministic piece.”

- **M6c.2 ✅ — Single-feed update workhorse** (`fetch_one_feed`). Given a feed row, run one update end-to-end: fetch (M6c.1) → conditional-refetch decision (`MAX_CONDITIONAL_INTERVAL` 12h) → on a 200 update `last_modified` (if changed) + run the M6b `process_feed` pipeline; on a 304 confirm unchanged (clear `last_error`, stamp `last_successful_update`); on a 429 throttle (`last_updated` + `last_successful_update` pushed into the future by `HTTP_429_THROTTLE_INTERVAL` × random 1–2); on a network error / parse failure stamp `last_error`. `guard_updatable` gates the feed (owner not read-only / disabled / purged). This is the unit both the loop and the on-demand path call. PHP ref: the per-feed body of `update_daemon_common` + the `update_rss_feed` error path. The **30-day auto-disable** (`DAEMON_UNSUCCESSFUL_DAYS_LIMIT`) is a separate scheduled task (M6c.4), not the workhorse. Fetch-tuning config fields (`FEED_FETCH_TIMEOUT`, `FEED_FETCH_NO_CACHE_TIMEOUT`, `MAX_CONDITIONAL_INTERVAL`, `HTTP_429_THROTTLE_INTERVAL`, `DAEMON_UNSUCCESSFUL_DAYS_LIMIT`, `RSS_EXTENDED_URL_FILTERING`) added to `Config` (from the environment, upstream defaults). `FetchOptions.filtering` (new `Option<bool>`) lets the fetch override the SSRF guard — the tests use a loopback mock server. Tests: 6 (success / 304 / 429 / network error / parse failure / read-only owner). NOTE: the PHP's 30s feed-content de-dup cache is **dropped** (D5 = c) — no local re-fetch-within-30s; the 304 conditional above makes such re-fetches cheap.

- **M6c.3 ✅ — Daemon/scheduler loop** (`daemon_loop`). A background `tokio::time::interval` (`DAEMON_SLEEP_INTERVAL` 120s) task: select due feeds (the `update_daemon_common` SQL — distinct `feed_url`, `DAEMON_FEED_LIMIT` 50 batch) → claim (`last_update_started = NOW()`, kept for in-process on-demand dedup + visibility) → dispatch per-feed tasks bounded by a `Semaphore(N)` (D4 = **N = 2**, config-able) in a `JoinSet` → drain/await: the loop processes one 50-feed batch, **awaits completion, then sleeps the interval** (the faithful sequential daemon cycle — large feed sets cycle over minutes, as in the PHP) → `tokio::signal` graceful shutdown. (No cross-process lock — D1 = B, strictly single-process.) The selection: `update_interval = 0` (use the `DEFAULT_UPDATE_INTERVAL` pref, 30 min) with `last_updated` older than the interval, OR `update_interval > 0` with `last_updated` older than the interval, OR never updated (`last_updated` NULL / epoch); owner not disabled (`-2`) / read-only (`-1`); in-progress feeds (`last_update_started` recent) excluded. The `FeedParser` / `FeedItem` now use `Arc` (not `Rc`) for the parsed document, so the `fetch_one_feed` future is `Send` (required by `JoinSet::spawn`). `tokio-util` added as a dependency (the `CancellationToken`). PHP ref: `update_daemon2.php` (master loop) + `RSSUtils::update_daemon_common` (the selection SQL). Tests: 8 (select-due-feeds: default-interval / not-due / disabled / never-updated / disabled-owner / in-progress + claim-feeds + graceful-shutdown).

- **M6c.4 ✅ — Housekeeping (“task 0”).** The cron-driven scheduled tasks (`ttrss::scheduler`, the port of `classes/Scheduler.php` + `RSSUtils::init_housekeeping_tasks`) + the per-feed purge (`Feeds::_purge`). `housekeeping_common` is exactly `Scheduler::run_due_tasks()` (the plugin-host variant is M8), so the loop runs it after each batch. Each registered task is due when its next scheduled run (strictly after its `last_run`) is in the past — the PHP's `getNextRunDate($last_run) < time()`. The tasks: purge orphaned `ttrss_entries`, disable feeds that failed > `DAEMON_UNSUCCESSFUL_DAYS_LIMIT` days, cleanup feed icons, log the daemon update-login-limit users, expire the error log, expire lock files, send headline digests, and the `@weekly` `purge_orphaned_scheduled_tasks`. **Cron engine = `croner`** (the plan's choice, line 112): it uses the PHP's 5-field `min hour dom mon dow` format + the `@daily`/`@hourly`/… macros exactly (the `cron` crate's 6-field seconds format was a mismatch). `cron.pattern.to_string()` reproduces the PHP's `getExpression()` normalization, so the stored `last_cron_expression` is byte-exact (`@daily` → `0 0 * * *`). No-op tasks (documented): `disk_cache_expire_all` (D5 = c, cache dropped), `expire_lock_files` (D1 = B, no lock files), `send_headlines_digests` (mailer not ported; the digest body is ported). The per-feed `purge_feed` (never-marked entries, plus unread ones only when `PURGE_UNREAD_ARTICLES` is set; `FORCE_ARTICLE_PURGE` overrides) runs per feed during the update (commit → purge → stamp, the PHP's `update_rss_feed` order). PHP ref: `RSSUtils::housekeeping_common` + `classes/Scheduler.php` + `Feeds::_purge`. Tests: 10 (is_due daily/hourly, purge_orphans, disable_failed_feeds, expire_error_log, cleanup_feed_icons, record_run upsert, run_due_tasks due-only + expression normalization, purge_feed, purge_feed disabled).

- **M6c.5 ✅ — On-demand / web-triggered updates + `Feeds::add` first update.** Wire the web actions (check-for-updates / per-feed “refresh now”, subscribe's immediate first update) to the M6c.2 workhorse; the request **blocks and awaits** the update (D2 = a). On-demand updates run **immediately, not gated by the loop's `Semaphore(N)`** (bounded only by the per-feed fetch timeout) — never queued behind the scheduled batch; dedup against the scheduled loop via the in-flight marker (DB `last_update_started` set within the **last 1 minute** → skip; the PHP's `updateFeed` has no such guard — a deliberate M6c.5 choice to avoid a redundant re-fetch of a feed the daemon loop just claimed). The shared subscribe engine `Feeds::_subscribe` (`ttrss::feeds::subscribe`): `UrlHelper::fetch` (respecting `RSS_EXTENDED_URL_FILTERING`) → HTML feed-discovery (`_is_html` + `_get_feeds_from_html`, `kuchikiki`) → `FeedParser::init` validation → existing-feed check (code 0) → create `[Unknown]` feed (code 1) → immediate first update via the new `rss_utils::update_basic_info` (title/site_url from the fetched feed). The RPC `Feeds::add` and the API `subscribeToFeed` delegate to it (subscribe is CSRF-protected, matching the PHP); the API `updateFeed` is the on-demand full update. PHP ref: `Feeds::add` immediate update + the on-demand update ops. Tests: 4 (subscribe first-update title / existing-feed code 0 / `updateFeed` on-demand ingests / `updateFeed` in-flight dedup no-op) in `tests/on_demand_updates.rs` (the shared loopback `MockServer` moved to `tests/common`). Commit: “direct in-process triggering” (§2b, the big win).

- **M6c.6 — CLI subcommands** — **dropped** (D1 = B, strictly single-process: no standalone `tt-rss daemon` / one-shot `tt-rss update`, no cross-process lock, multi-instance door closed). The updater always runs embedded in the web process.

- **M6c.7 — Batch smoke test + config/pool wiring (done).** The §1 exit criterion: record a golden DB, replay a recorded feed-XML **batch** in-process, diff the resulting rows. Single shared DB pool (D3 = b): keep the existing `PgPool`, `max_connections` is a config knob bumped as needed (no second pool). Wire the config surface (`SLEEP_INTERVAL`, `N`, timeouts, conditional interval, 429 interval, feed limit, unsuccessful-days limit, proxy, pool size). `crates/ttrss/tests/batch_smoke.rs` replays a 3-feed batch (6 items, 1 cross-feed dedup) through the workhorse + DB, asserting the 6 invariants (distinct-articles, per-user subscriptions, workhorse count, shared-article dedup, unread count, cross-feed dedup hit). `Config` gains `db_max_connections` (env `TTRSS_DB_MAX_CONNECTIONS`, default 10); `Db::connect` uses it; `Db::connect_pool` exposes the explicit-pool-size variant. Commit: the batch-smoke verification bar (§1).

**M6c decisions to make (§5 “decisions to revisit,” in interview order):**
1. **Process topology** — ✅ **DECIDED: B, strictly single-process.** The updater is only ever embedded in the web process; no cross-process daemon lock, no CLI subcommands (M6c.6 dropped), multi-instance scaling door closed. The `last_update_started` claim is kept for in-process dedup + visibility, not multi-instance safety.
2. **On-demand update path** — ✅ **DECIDED: (a) Block.** The request awaits the on-demand update and returns fresh counts. On-demand updates (add / refresh / check-for-updates) run **immediately, not gated by the loop's `Semaphore(N)`** (infrequent/one-off; bounded only by the per-feed fetch timeout) — so they're never queued behind the scheduled batch. `Feeds::add` blocks on its first update (faithful). Enqueue-for-next-tick is ruled out (§2b = direct in-process triggering).
3. **DB pool(s)** — ✅ **DECIDED: (b) One shared pool.** Keep the single existing `PgPool` (start at `max_connections(10)`); the size is a config knob, increased as needed (to cover web peak + N + on-demand) rather than introducing a second pool now. Revisit two pools only if update load demonstrably starves the web. The pool size doubles as a natural cap on concurrent DB work (an update waiting for a free connection = cheap backpressure), complementing the `Semaphore(N)` fetch cap.
4. **Concurrency `N`** — ✅ **DECIDED: N = 2** (matching PHP's `DAEMON_MAX_JOBS`), config-able. Rationale: proven in production for years — no perceived throughput lack at N=2. The loop processes one 50-feed batch at N=2, awaits completion, sleeps the interval, repeats (faithful sequential daemon cycle). The single pool (10) comfortably covers N=2 + on-demand + web.
5. **Feed-content cache** — ✅ **DECIDED: (c) Drop it.** No 30s feed-content de-dup cache. The question's “disk cache” is the feed-content cache (`DiskCache::instance('feeds')`, one `sha1(feed_url).xml` per feed, pruned after `CACHE_MAX_DAYS`=7d), **not** the read-side image/media cache (`instance('images')`, `MAX_CACHE_FILE_SIZE` — already ported in `diskcache.rs`, unaffected). Rationale: it fires only when a non-auth feed is re-fetched within 30s (rare), has no user-visible impact, and the 304 conditional (ported in M6c.1/2) makes such re-fetches cheap (server → 304 → no re-processing). The PHP's disk version held the full `#feeds × size` footprint for files only ever read within 30s (wasteful); the in-memory alternative held only the 30s window but is unnecessary given (c). A deliberate, unobservable deviation from the PHP.
6. **Stampfile / daemon-status indicator** — ✅ **DECIDED: (c) Drop the file-based mechanism + stub the API responses.** The updater-health indicator is driven by `RPC::getruntimeinfo` (`rpc.rs::make_runtime_info`), which the SPA consumes (`App.js` → `Notify.error("Update daemon is not running.")` when `daemon_is_running !== 1`, `"Update daemon is not updating feeds."` when `daemon_stamp_ok !== 1`). Changes: (1) **RPC** — drop the `.lock`/`.stamp` file reads + the `file_is_locked` helper (if now unused); stub `daemon_is_running` = **`1`** (JSON *integer*, so the UI's strict `v !== 1` check passes), `daemon_stamp_ok` = **`1`** (integer), `daemon_stamp` = **current time** in `Y.m.d, G:i` format; keep `recent_log_events` (a separate DB query, unaffected). (2) **Level-23 API** (`api.rs:296`) — change `daemon_is_running: false` → **`true`** (the updater now runs in-process, always on; this surface carries only `daemon_is_running`, not the stamp fields). Rationale: in the single-process design (D1 = B) the updater is always running, so `daemon_is_running = 1` is the accurate steady-state value; the 30-min “not updating feeds” stale-check is dropped as a feature (if the updater loop ever stalls, the UI will no longer warn — accepted). Removes the file I/O + the cross-process `flock` artifact from the port.

**Open / deferred (not done, each tracked elsewhere):** the disabled M2 corpus steps above (moot — the harness is retired 2026-09-19); `notes` handler (M7); plugin management + the `HOOK_*` no-ops (M7); `updateFeed`/`subscribeToFeed` (M6 — done in M6c.5); `checkforupdates` is an intentional no-op (no PHP-upstream channel).

### M5a — External API client compatibility (planned, 2026-09-15; M5 scope completion)

**Why it exists.** A 2026-09-15 gap analysis of the official Android client (`tt-rss/tt-rss-android`, package `org.fox.ttrss`) — every op it sends plus its one non-API call — found four gaps inside M5's original scope ("External API completeness"). The client is broken at the transport level and on its two list-building ops; everything else it uses (login, getApiLevel, getUnread, getLabels, the getHeadlines param set, updateArticle, setArticleLabel, catchupFeed, subscribeToFeed, unsubscribeFeed, shareToPublished, feed_icon) already matches PHP exactly. Client evidence: `ApiCommon.java:139` (every request is a JSON POST to `<ttrss_url>/api/`), `RootCategoriesModel.java:49/95` (`getFeeds(cat_id=-1, include_nested=true)` for the main list; `getCategories(enable_nested=true)` for the root list), `ArticleModel.java:196` (`getHeadlines(has_sandbox=true, show_content, show_excerpt, include_attachments, …)`), `HeadlinesFragment.java:1516` (GET `public.php?op=feed_icon&id=…`).

| # | Gap | Client impact |
|---|---|---|
| G1 | `POST /api/` not routed (only `POST /api/index.php`) | **every** client request 404s; cannot log in |
| G2 | `getFeeds` is a naive `WHERE cat_id = ?` with a different entry shape | main list (`cat_id=-1`) empty; nested cats missing; entries lack `unread`/`has_icon`/`feed_url`/`cat_id`/`last_updated`/`order_id`/… → badges 0, no icons, no ordering (the client sends no `getCounters` op, so `getFeeds.unread` is its only badge source) |
| G3 | `getCategories` ignores `enable_nested` | nested categories shown as roots; root unread misses descendants |
| G4 | `getHeadlines` drops `has_sandbox` | `iframe` stripped from article content (the client sandboxes HTML in a WebView and sends `has_sandbox=true`; PHP then admits `iframe` — `Sanitizer.php:277`) |

**Corpus-masking note:** the recorded `getFeeds` corpus step used the param name `category_id` (PHP reads `cat_id` → 0 → empty), so the diff was green on the degenerate empty case on both sides — the shape gap was invisible to the harness. M5a.4 added real-`cat_id` corpus steps; the degenerate step stays (it documents that case faithfully).

**Increments** (bottom-up; each ends with green unit/DB tests + oracle-diff corpus steps where noted):

- **M5a.1 ✅ — Route `POST /api/` (G1).** Add `/api/` and `/api` for `api::handler` in `http.rs` (the base-nested variant inherits via `nest`). PHP resolves `/api/` → `api/index.php` via Apache `DirectoryIndex` (and 301s `/api` → `/api/`); the port serves the handler directly at both. POST-only, mirroring the existing `/api/index.php` route. Tests (2, `tests/public_http.rs`): full `login` via `POST /tt-rss/api/` and via `POST /tt-rss/api` — JSON body exactly as the client sends it — asserts the envelope (`status 0`, non-empty `session_id`, `api_level 23`); both routes previously 404'd via the static fallback. NOTE: `API::login` requires the user's `ENABLE_API_ACCESS` pref — PHP's registry default is **false** (the test enables it), confirmed against `Prefs.php`.

- **M5a.2 ✅ — `getCategories` `enable_nested` (G3).** Read the param; when true: `WHERE parent_cat IS NULL` and add the descendants' unread (`counters::cat_children_unread`, already ported iteratively) onto each root's unread — exactly PHP's `API::getCategories` + `Feeds::_get_cat_children_unread`. The golden DB already carries a nested tree (News → World News/Tech News; Development → Rust/Blogs), so no seed change. Tests (4, `tests/api_getcategories.rs`): flat lists the nested cat and omits descendant unread; `enable_nested` lists roots only with descendants folded in (6 direct + 1 nested = 7); `unread_only` drops a zero-unread root; special cats without `include_empty` (LABELS present / SPECIAL never-empty with unread 0 / UNCATEGORIZED absent). Corpus step `api_get_categories_nested` recorded vs PHP — **54/54 harness match**. The corpus step exposed a real port bug: `API::_is_cat_empty` (ported `cat_empty`) ran a generic `cat_id = -N` feed count for LABELS/SPECIAL, so both were wrongly "empty" whenever `include_empty` was off (latent — the existing corpus step used `include_empty=true`); fixed to PHP's semantics (LABELS = no `ttrss_labels2` rows, UNCATEGORIZED = no NULL-cat feeds, **everything else never empty**), with a regression test.

- **M5a.3 ✅ — `getHeadlines` `has_sandbox` (G4).** Read the `has_sandbox` param, store it in the session (`$_SESSION['hasSandbox']` via `sess.with_mut`), and thread it into the `sanitizer::sanitize` call in `api_headline_row` (was hardcoded `false`; `allowed_elements(has_sandbox=true)` admits `iframe` — the sanitizer keeps and hardens the iframe: `embed-responsive` wrapper + `sandbox` attr). `get_article` re-reads the session flag (PHP `getArticle` has no param of its own — it inherits the flag the client set via `getHeadlines`). Tests (3, `tests/api_getheadlines_sandbox.rs`): an article with an `<iframe>` — stripped without the flag, kept + hardened with it, and `getArticle` honoring the session flag. Corpus step `api_get_headlines_has_sandbox` (Ars Technica, `show_content=true` + `has_sandbox=true`) recorded vs PHP.
  - **This was the first corpus step to exercise API content sanitization** (`getarticle` uses `sanitize=false`, `getheadlines` used only `show_excerpt`), so recording it surfaced three *pre-existing* sanitizer gaps independent of `has_sandbox`. All resolved to a green envelope except the M7-gated sandbox value:
    1. **`flavor_*` absent on the Rust side (~90 diffs):** `Article::_get_image` was ported to `article.rs::get_image` (and its 8 PHP tests, `tests/article_parity.rs`) but never *wired* into `api.rs::api_headline_row`. Ported the PHP `if (include_attachments) { flavor_image/flavor_stream/flavor_kind }` block. (The logic was already tested; only the API wiring was missing.)
    2. **`controls` vs `controls=""` (serialization):** the port's sanitizer re-serializes through html5ever while PHP uses libxml, so boolean-attribute spelling legitimately differs. Fixed by making the harness compare HTML string fields (`content`) as **canonical DOM trees** (`harness::html::canonical_html`, same parser both sides) rather than raw text — mirrors the existing top-level HTML-body path. Not a port bug; a comparison-bar choice (see `AGENTS.md`: frontend HTML matches structure + visible text, not whitespace/byte-spelling).
    3. **`%20`-vs-space in `href` (malformed feed input):** the Ars feed has hrefs with a stray leading space (`href=" https://…"`). PHP's `rewrite_relative` returns the space *literally* (verified in the oracle PHP); the `%20` is introduced by **libxml `saveHTML()` percent-encoding spaces in `href`/`src` attributes only** (`class`/`title`/`poster` keep literal spaces). html5ever keeps the space literal. Fixed with `sanitizer::libxml_space_encode` (space→`%20`) applied at the `href`/`src` attribute-finalization points, with a comment documenting the libxml quirk. No-op for well-formed URLs.
  - **Residual (expected, M7-gated):** 2 YouTube-embed articles still differ only in the iframe `sandbox` value — PHP (with the `youtube-embed` plugin, the whitelist's only known consumer) emits the loose `allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox`; the port (no plugin system yet) emits the tight `allow-scripts`. **Closed by M7.11** — the read-side `sanitize` now computes the `HOOK_IFRAME_WHITELISTED` admitted-host set and gives whitelisted iframes the loose value (verified by `af_youtube_embed::youtube_iframe_keeps_loose_sandbox`). 54/55 harness match; the 1 mismatch is exactly this. Two independent harness fixes also landed: `harness run` now clears stale `NNN_*.json` from the out dir (step-numbering shifts were poisoning diffs), and `oracle-a` was re-recorded with clean numbering.

- **M5a.4 ✅ — Faithful `getFeeds` (G2).** Replaced the naive `WHERE cat_id = ?` query with a port of `API::_api_get_feeds` (`api.rs::get_feeds`): the four blocks in PHP's order — (a) **labels** when `cat_id ∈ {CATEGORY_ALL (-4), CATEGORY_LABELS (-2)}` via `counters::get_labels`; (b) **special virtual feeds** (starred/published/fresh/all/recently-read/archived) when `cat_id ∈ {CATEGORY_ALL, CATEGORY_SPECIAL (-1)}` — `get_counters` + the virtual-feed title helper, entries `{id, title, unread, cat_id: -1}`; (c) **child categories** when `include_nested && cat_id` — `{id, title, unread, is_cat: true, order_id}`, unread = `get_counters(cat, is_cat=true) + cat_children_unread`, `order_id`/`title` order; (d) **real feeds** — no cat filter for `cat_id ∈ {CATEGORY_ALL_EXCEPT_VIRTUAL (-3), CATEGORY_ALL}`, else `(cat_id = ? OR (0 = ? AND cat_id IS NULL))`, `limit`/`offset`, `order_id`/`title` order, the `unread_only` gate, and the exact PHP shape `{feed_url, title, id, unread, has_icon, cat_id, last_updated, order_id, last_error, update_interval}`. `has_icon` = feed-icons cache-file existence (PHP `Feeds::_has_icon`); `last_updated` = unix epoch, with NULL (a never-updated feed) → `0`, matching PHP's `(int) strtotime($last_updated ?? '')`. `cats.rs` gains `CAT_ALL_EXCEPT_VIRTUAL = -3` / `CAT_ALL = -4`. The PHP's unconditional plugin-host virtual-feed loop stays a no-op pre-M7 (no plugin feeds exist — M7 will extend block (b)/(c)). Tests (8, `tests/api_getfeeds.rs`): special feeds (6, fixed order + unread), labels, all-except-virtual (full 10-field shape + per-feed unread + title order), a real cat, uncategorized (`cat_id=0` → empty), `unread_only` drops a 0-unread feed, `limit`/`offset` paging, and `include_nested` folding a child cat's unread. Corpus: 6 steps with real `cat_id` values (`api_get_feeds_special/labels/all/cat/nested_cat` + the degenerate `category_id` step kept, per the masking note) — **59/60 harness match** (the 1 mismatch is the M7-gated `has_sandbox` sandbox value).


**M5a decisions to revisit:**
1. **Naming/placement** — M5a (M5 scope completion) rather than a new rung: every gap is inside M5's "External API completeness" scope. Delivery order = next, before M7 (the plugin system), since M7's plugin virtual feeds will extend M5a.4's block (b)/(c) — landing M5a first means the shape is already faithful when plugin feeds appear.
2. **`/api` without trailing slash** — served directly (no 301): the Apache redirect is infrastructure, not tt-rss behavior; serving the handler there is superset-safe for clients that trim the slash.
3. **GET `/api/`** — not routed (the handler stays POST-only): PHP answers a 200 `INCORRECT_USAGE` envelope; accepted residual diff (no client GETs the API).
4. **Static-DB harness runs (M5a.2 follow-up)** — the record/replay diff needs both sides on a frozen DB: the Rust server now honors `TTRSS_DAEMON=0` (env-only switch; the PHP daemon is a separate process that simply isn't run) and `harness.sh` sets it; `make record` stops the updater container + resets the DB to the golden baseline before recording (previously the M6c.3 daemon and the live PHP updater each drifted the DB mid-run → counter/param diffs). `make record` also implies the baseline dir must be clean — stale files from an older step set made the diff report "only in A" (delete `oracle/data/runs/oracle-a` after corpus changes).

### M0 status: COMPLETE (2026-09-03)

- Oracle stack up: `oracle/` (docker compose: postgres15 + FPM app + updater + nginx),
  standard `/tt-rss/` deployment, pinned `sha-2f7d831` images, port 8280.
- Golden dataset seeded + frozen: 41 feeds (38 live + 3 broken), ~3.9k entries,
  3 users (admin w/ TOTP + app password, readonly, testuser), profiles, filters,
  labels, access keys; `oracle/data/golden-db.sql` + cache + manifest + `ids.json`.
- Corpus: `oracle/corpus/scenarios.template.toml`, **58 steps** (public endpoints,
  web login incl. OTP challenge, backend RPC, Pref_* handlers, full level-23 API,
  syndication, ETag/304, readonly user, logouts).
- Harness proven: two live oracle runs minutes apart diff **58/58 clean** after
  normalization; diff sensitivity covered by unit tests (status/data diffs detected,
  csrf/session/timestamp/set-cookie variance ignored). 10/10 unit tests green.
- PHP-FPM baseline recorded in `oracle/data/baseline.json` (hot endpoints
  ~46–117 ms mean incl. nginx + FPM cold worker + DB).

Key PHP behaviors discovered (the Rust port must match these):

- User passwords: `SSHA-512:sha512(salt+pass)` (salt in a **separate column**).
- App passwords: `SSHA-512:sha512(salt+pass):salt` — **ALGO:HASH:SALT**.
- TOTP: `otp_secret` column stores an ASCII string; it is base32-encoded on read,
  so the effective HMAC key is **the ASCII bytes of the column value**;
  6 digits / 30 s / SHA-1; `$otp->now()` has **no ±1 window**.
- Sessions: DB-backed (`ttrss_sessions(id, data, expire)`), name `ttrss_sid`,
  data = `base64(serialize({algo,nonce,payload}))`, xchacha20poly1305-ietf with
  `hex2bin(TTRSS_ENCRYPTION_KEY)`; `extend_session()` re-stamps the cookie on
  every request (hence per-request `set-cookie` with rolling expires).
- API: JSON body only (content-type enforced), `op` lowercased, `sid` in body,
  session cookies disabled; `API::login()` does `session_destroy()` +
  `session_start()` (new session **reuses the web session id** when a web cookie
  is present — session data is overwritten).
- Auth throttling: `AUTH_MIN_INTERVAL` (default 5 s) on failed attempts via
  `last_auth_attempt`.
- `Feeds::view` on a feed the user doesn't own → 200 with "Feed not found." error
  feed JSON (not 404); missing required `feed` param → PHP 8 TypeError → empty 500.
- `runtime-info` exposes `recent_log_events` (a live counter — normalized in diff).
- Updater daemon: fork-based; `docker compose stop` SIGKILLs it after the 10 s
  grace (exit 137) — expected, not a crash.

### M1 status: COMPLETE (2026-09-04)

Goal: `ttrss serve` boots, serves static + login, and can log in against the
golden DB with encrypted DB sessions; SPA shell renders. Progress is
demoable and oracle-diff-verified per surface.

Done:

- **`ttrss::core` (24 tests, no HTTP dep):**
  - `phpser` — byte-oriented PHP `serialize`/`unserialize` subset (raw-byte
    strings, ordered arrays, `s:`/`a:`/quoted-string handling).
  - `crypt` — xchacha20poly1305-ietf (32-byte key, 24-byte nonce, empty AAD),
    matches libsodium/RustCrypto byte-for-byte.
  - `session` — `key|serialize(value)` plaintext + base64 + encrypted PHP-array
    envelope (`{algo, nonce, payload}`); round-trips real oracle session rows.
  - `user` — password verify for SHA1/SHA1X/MODE2/SSHA-256/SSHA-512; app
    passwords (`ALGO:HASH:SALT`); TOTP (ASCII-byte key, 6/30/SHA-1, no ±1
    window).
  - `config` — `TTRSS_*` env with upstream defaults; `base_path()`; sqlx DSN.
  - `db` — sqlx `PgPool` applying `Db.php` session setup via `before_acquire`
    (`client_encoding`, `datestyle`, `TIME ZONE 0`, `cpu_tuple_cost`).
- **Harness HTML comparator (the "proper" way):** `crates/harness/src/html.rs`
  parses both documents with the canonical `html5ever`/`markup5ver` parser (via
  `scraper` + `ego-tree`) and compares **canonical DOM forms** — drops
  whitespace-only text nodes, normalizes text *and* attribute values (cache-bust
  / hex / timestamps), sorts attributes, keeps `<script>`/`<style>` as raw text
  (compared faithfully, not regex-mangled). Diff is now **port-agnostic** (compares
  request path, not full URL) so oracle (8280) vs Rust (8281) diff cleanly.
- **`ttrss` (axum, binary `ttrss serve`):** base-path nesting, `/healthz`,
  login-page renderer (`templates/login.html.tpl`: scripts/theme cache-bust from
  real mtimes, PHP-style urlencode), and a **custom static handler** — full
  header control, byte-for-byte bodies, path-traversal protection.
- **Static header parity:** Content-Type from the **canonical nginx `mime.types`**
  (107 ext; e.g. `application/javascript`, not `mime_guess`'s `text/javascript`),
  `Cache-Control: no-cache`, no `Accept-Ranges` (nginx omits it on the gzip path
  real clients hit). `crates/ttrss/src/mime.rs` holds the nginx table.
- **Login flow (verified 4/4 against the oracle through the comparator):**
  `POST /public.php?op=login` — `crates/ttrss/src/auth.rs` ports the
  `auth_internal` web path: SSHA-512 verify + TOTP (ASCII-byte key, no window);
  no-otp → 200 OTP challenge page (`templates/otp_challenge.html.tpl`),
  valid-otp → 302 + `Set-Cookie: ttrss_sid=<32hex>; path=/` + `Location` (validated
  `return` else self URL) + PHP `nocache` headers (`no-store, no-cache,
  must-revalidate`, `Pragma: no-cache`, fixed 1981 `Expires`). Non-OTP users get
  `last_auth_attempt` throttling (`AUTH_MIN_INTERVAL`). Verified: `healthcheck`,
  `login_page`, `web_login` (200 challenge), `web_login_otp` (302) all diff clean.
- **Public-surface dispatch audit** (all IHandler classes diffed PHP vs Rust
  arms): no gaps on the backend surface (`get_hotkeys_info`/`get_hotkeys_map`
  are static helpers — the map ships via `sanitycheck` init-params;
  `decrypt_feed_pass`/`remove_feed`/the `OPML` helpers take required params,
  which PHP itself refuses; the 7 `Pref_Prefs` plugin methods belong to the
  unported plugin subsystem). Two public.php endpoints the port lacked were
  added: **`op=getprofiles`** (the login page's Profile dropdown fetches it on
  login-field change/focus/blur — `auth::get_profiles`: `[{label, value}]` with
  the `Default profile` entry first, titles ascending, case-insensitive login;
  4 tests in `tests/public_getprofiles.rs`) and **`op=healthcheck`** (the
  monitoring probe — same `OK`/`text/plain` body as the `/healthz` route).
  `dbupdate`/`confirmDbUpdate` (the web DB-migrator) is a known skip: the
  schema is frozen at the reference commit and `checkforupdates` reports no
  updates, so the flow is unreachable in the port.
- **DB-backed encrypted session middleware** (`crates/ttrss/src/session.rs`):
  reads `ttrss_sid` cookie, loads + decrypts the session (via core `from_stored`),
  exposes a mutable `Sess` handle to handlers, persists back only if dirty
  (core `to_stored`). Anonymous requests never touch the DB. Session creation on
  login verified end-to-end: the stored row decrypts to the right keys
  (`uid=1`, `name=admin`, `access_level=10`, `auth_module=auth_internal`,
  `ref_schema_version=151`, `csrf_token`, `pwd_hash`, `bw_limit`/`safe_mode`).
- **Authenticated SPA shell renderer** (`templates/shell.html.tpl` +
  `render_shell`): the full `index.php` shell — container DOM (feedTree,
  headlinesInnerContainer, content-wrap, toolbar, cmdline, …), the
  `const __csrf_token` (from the session), the per-plugin `Plugins.*` JS blocks,
  theme/PWA metadata. Headers match the oracle: `text/html; charset=utf-8` (the
  shell is lowercase `utf-8`, the login page is uppercase `UTF-8`),
  `Cache-Control: private, no-cache`, `ETag: "<md5(body)>'>`, and `If-None-Match`
  → 304. The rolling cookie re-stamp (PHP `extend_session`) is in the session
  middleware: authenticated responses re-emit `Set-Cookie: ttrss_sid=…;
  expires=<now+86400>; Max-Age=86400; path=/` (skipped when the handler already
  set the cookie, e.g. the login 302).
- **Harness normalizer:** `UNIX_TS` now normalizes bare 10-digit unix epochs
  (the dojoConfig `cacheBust` and feed-publish epochs in API JSON) that the
  URL-only `CACHE_BUST` didn't catch. The `<script>`/`<style>` code blocks are
  whitespace-collapsed so indentation (tabs vs spaces) doesn't cause spurious
  mismatches while a real token change still shows up.
- **Verified 6/6 against the oracle through the comparator:** `healthcheck`,
  `login_page`, `static_svg`, `web_login` (200 challenge), `web_login_otp` (302),
  `shell` (200 SPA shell), `shell_conditional` (304). Whole workspace:
  **49 tests passing, clippy clean**.

M1 is done: `ttrss serve` boots, serves static byte-for-byte, renders the
login page, logs in (SSHA-512 + TOTP) against the golden DB, maintains encrypted
DB sessions, and renders the authenticated SPA shell. Next (M2) makes the SPA
actually load data read-only: the `backend.php` RPC surface (`getfeedtree`,
`Feeds.view`, …) so the feed tree and headlines render on real data.

### M1 refinement: frictionless admin + dedicated OTP user + full login/logout surface (2026-09-04)

- **Corpus/seed:** admin no longer has TOTP (username/password → 302 directly,
  the frictionless demo path); a new `otpuser/otppass` (secret
  `OTPSECRET1234567890`) carries the TOTP-enabled user, so the OTP challenge
  (200) → code (302) flow is still exercised, off the admin path. Corpus is now
  **61 steps**; golden DB re-seeded + re-snapshotted.
- **`op=logout` ported** (`Handler_Public::logout` + `UserHelper::logout`):
  CSRF-gated, destroys the session row, emits the rolling re-stamp then the
  deletion cookie (`ttrss_sid=deleted; expires=1970; Max-Age=0` — PHP 8.5
  clamps a past `expires` to epoch 1) and 302s to `self/index.php` with the PHP
  `nocache` limiter headers. Session middleware gained `destroy()` (deletes the
  row instead of re-saving).
- **Harness normalizer:** tt-rss "smart dates" (`13:28`, `1 Jan, 12:00`,
  `Thu, 1 Jan 2026`) collapse to `<ts>` — they're display renderings of stored
  datetimes that corpus logins re-stamp between runs (the users-admin
  `last_login` column). URL authority ports are stripped (literal +
  percent-encoded) so the oracle (8280) and Rust (8281) compare as the same app
  on different ports; all header values (notably `location`) are now
  value-normalized, not just `set-cookie`.
- **Verified against the oracle through the comparator (15/15 of the M1
  surface clean):** `healthcheck`, `login_page`, `static_svg`, `web_login`
  (302), `shell` (200), `shell_conditional` (304), `logout_admin` (302),
  `shell_after_logout` (200), `web_login_readonly` (302), `shell_readonly`
  (200), `logout_readonly` (302), `otp_login_challenge` (200),
  `otp_login_success` (302), `shell_otp` (200), `logout_otp` (302). Self-replay
  **61/61 clean**. Whole workspace: 51 tests passing, clippy clean.

Design notes (this increment):

- HTML compatibility = normalized/structural (parser-based DOM) + exact dynamic
  values + meaningful headers — not raw-byte matching. Static = byte-for-byte.
- `ttrss::core` keeps zero HTTP deps so the web framework (axum) stays swappable.

### M2 status: CORE COMPLETE — read-only UI on real data (2026-09-05)

M2 = *read-only UI on real data*. The `backend.php` read surface that the SPA
needs to render the main content area is now ported and diffed against the
oracle. **`Feeds::view` (feed + category), `Feeds::search`, `RPC::getAllCounters`,
`Pref_Feeds::getfeedtree`, `RPC::sanityCheck`, and `RPC::getRuntimeInfo` all diff
clean** on the golden dataset. Done so far:

- **`backend.php` dispatcher** (`crates/ttrss/src/rpc.rs`): `op`/`method`
  (`subop` fallback) routing, public shim (`rss`/`getProfiles`/`share` → 302
  `public.php`), light session validation (E_UNAUTHORIZED), CSRF gate
  (`validate_csrf`), `Handler`/`Errors::to_json` envelope, `E_UNKNOWN_METHOD`
  fallback. The session middleware's rolling cookie re-stamp + the PHP `nocache`
  trio (`no-store, no-cache, must-revalidate` / `Pragma` / fixed-1981 `Expires`)
  are emitted on responses whose session started.
- **`RPC::sanityCheck` + `getRuntimeInfo`** — the SPA's first calls (un-blocks
  the load sequence). Ports `_make_init_params` (36 keys: 17 prefs via the new
  `Prefs` resolver, `is_default_pw`, `label_base_index`, `theme` (a `themes/<x>`
  file/dir existence check — `night` is a `.css` file so it's *not* a theme →
  `""`), `plugins`, `hotkeys` (61-entry static map + derived prefixes), 4
  `icon_*` base64 blobs, labels) and `_make_runtime_info`. `_make_runtime_info`
  - `init-params` `labels` share `Labels::get_all`.
- **`Prefs` resolver** (`crates/ttrss/src/prefs.rs`): the `ttrss_prefs` DB
  registry is empty in this fork — defaults live in PHP `Prefs::_DEFAULTS`
  (reproduced) with the profile blacklist + `cast_to` (bool/int/str) +
  `ttrss_user_prefs2` overrides.
- **`plugins` field** = the `TTRSS_PLUGINS` env list mapped to class names by
  scanning each plugin's `init.php` (`plugins/` then `plugins.local/`), in
  order (the init list dominates; per-user `_ENABLED_PLUGINS` only re-registers
  a subset, so it's identical across users here).
- **Corpus:** `rpc_sanity_check` now sends `clientLocation={{base}}/` (+
  `hasSandbox`/`clientTzOffset`) so it exercises the success path (the oracle
  previously returned `E_URL_SCHEME_MISMATCH` with no client location). Re-recorded
  oracle-a/b.
- **Harness normalizer:** drops the live-daemon runtime fields
  (`daemon_is_running`/`daemon_stamp`/`daemon_stamp_ok` — present when an updater
  runs, absent on the daemon-less Rust test server) and `php_platform`/`php_version`
  (runtime environment facts, unused by the frontend) alongside `recent_log_events`.
- **`Pref_Feeds::getfeedtree`** (`crates/ttrss/src/feeds.rs`) — the feed list
  the SPA renders in the left panel (mode 2, a CSRF-ignored GET). Ports
  `_makefeedtree`: the special (`CAT:-1`) + labels (`CAT:-2`) categories with
  their real unread counts (`Feeds::_get_counters` — `FEED_FRESH` is the one
  `NOW()`-dependent value, drifts after the 24h fresh window), the nested real
  categories (empty ones hidden in mode 2), and `CAT:0` uncategorized. Node
  shapes match the oracle exactly: top-level cats have no `parent_id`, subcats
  do; special/label feeds carry `updated`/`auxcounter:0`/material `icon`,
  regular feeds carry `param` (`"(N articles / <ts>)"`, time normalized),
  `error`, `updates_disabled`. Feed favicons: `Feeds::_get_icon_url` checks the
  `feed-icons` DiskCache (`<feed_icon_dir>/<id>` file exists →
  `public.php?op=feed_icon&id=<id>`, else `""`); the 32 golden icons are a
  deployment artifact copied to `oracle/data/feed-icons/` (gitignored),
  pointed at via `TTRSS_RUST_FEED_ICONS_DIR`.
- **Dispatcher:** now routes `op=Pref_Feeds` (CSRF-ignored for `getfeedtree`/`index`)
  with the `Handler_Protected::before` (logged-in user) check generalized; the
  CSRF gate (`validate_csrf || csrf_ignored`) is applied per handler in
  `dispatch` rather than per handler.
- **Corpus:** `feed_tree` + `readonly_feed_tree` now use the SPA's exact request
  (CSRF-free GET, `mode=2`); the readonly user (uid 2, 0 feeds) yields just the
  Special category — a nice empty-tree edge case. `feed_tree_tech_news` stays a
  mode-0 GET (`category_id` is ignored by the PHP).
- **Setup:** the Rust test server must run with `TTRSS_PLUGINS` + `TTRSS_RUST_FEED_ICONS_DIR`
  matching the oracle and the doc root containing `plugins.local/nginx_xaccel`
  (deployment artifacts), so `plugins` + feed icons match.
- **Verified:** `rpc_sanity_check`, `rpc_runtime_info`, and all three feed-tree
  steps (admin mode 2, admin mode 0, readonly mode 2) diff **clean**; the mode-2
  tree is node-for-node identical to the oracle after port/time normalization.
  All M1 steps still clean; **20/61 match**. Self-replay **61/61 clean**. The
  browser now advances past the 30% overlay and populates the left feed panel.
  Remaining mismatches are the unimplemented M2+ surface (other RPC methods,
  headlines `Feeds::view`, article/labels/filters/users/prefs, external API,
  syndication, icons).
- **`Feeds::view` / `Feeds::search` (headlines)** (`crates/ttrss/src/headlines.rs`):
  the main content area. Ported `Feeds::view` (feed + category + virtual feeds),
  the full `_get_headlines` dynamic-SQL branch logic (starred/published/fresh/all/
  recently-read/archived/error/label feeds + search), the `yyiw` date-grouping
  `ORDER BY` (with `VFEED_GROUP_BY_FEED` default `false`), first-id check /
  `ForceUpdate`, and `_format_headlines_list` (32-key article objects, exact key
  order). `content_preview` is byte-identical (70/70 articles); `content`/`buttons`
  compare structurally (DOM). Article time rendering uses the new `time` module.
  `content_preview`/`feed_bg_color`/enclosures/labels all verified against the oracle.
- **`RPC::getAllCounters`** (`crates/ttrss/src/counters.rs`): ports `Counters::get_all`
  (feed + category + label counters, 64 entries). Uses the oracle's session settings
  (`cpu_tuple_cost = 0.5`, `TIME ZONE 0`) and **no explicit `ORDER BY`** so the natural
  plan order matches the PHP. Counter *values* and the *order relative to PHP* are
  byte-identical on the same DB (verified: PHP oracle and Rust produce the identical
  label-counter order on the same dataset).
- **`time` module** (`crates/ttrss/src/time.rs`): ports `TimeHelper::make_local_datetime`
  - `smart_date_time` + the user's `SHORT_DATE_FORMAT`/`LONG_DATE_FORMAT` (uid 1: `j M, G:i`),
  the `eta_min` and bare-time cases, and `naive_to_pg_text` (a `timestamp without time zone`
  → the exact PG/PDO text, fraction only when non-zero, trailing zeros stripped).
- **Sanitizer libxml2 fidelity** (`crates/ttrss/src/sanitizer.rs`): PHP uses libxml2
  (`DOMDocument::loadHTML`), which does *implicit `<p>` insertion* for leading bare text
  and drops leading whitespace/comments; html5ever (spec-compliant) does not. Ported
  `apply_libxml_implicit_p` to match — this cleared the category-view content diffs.
  The serializer now also matches libxml's `saveHTML()` **entity encoding**: non-ASCII
  text/attribute chars emit the named HTML entity (the 243-entry libxml table, ported
  as `sanitizer::HTML_NAMED_ENTITIES`) or a decimal numeric reference, and attribute
  quoting follows libxml (a value containing `"` but not `'` is single-quoted). Three
  previously deviating `sanitizer_parity` tests now assert the PHP oracle's real
  output (e.g. `<p>中文</p>` → `<p>&#20013;&#25991;</p>`; the `highlight_words_str`
  multibyte case is `t&euml;st` on libxml 2.13.9 despite the PHP test's `>= 21200`
  branch). Needed for the syndication feed's byte-fidelity.
- **Atom/JSON syndication** (`crates/ttrss/src/syndication.rs`): `public.php?op=rss`.
  Access-key auth (`ttrss_access_keys` lookup → 403; get-or-create of the feed self
  URL's key via `uniqid_short`), `Feeds::_get_headlines` with the syndication params
  (`include_children`, `ignore_vfeed_group`, `limit` default 60, virtual-feed default
  ordering, `ts` start filter, `_order_to_override_query`), `tag:<host>,<Y-m-d>:/<id>`
  tag URIs, and the Atom body rendered with minijinja from `templates/feed_atom.jinja`
  (a faithful transcription of `templates/generated_feed.txt`; the `feed_hub` block is
  never emitted on this path). The JSON variant is built with serde_json. Article
  content goes through `sanitizer::sanitize` + `DiskCache::rewrite_urls`, with a
  non-empty note prepended as a styled div; `noxml` switches the content type to
  `text/plain`. `046 syndication_feed_atom` / `047 syndication_cat_rss` /
  `048 syndication_bad_key` diff clean. The `q` search filter is ported
  (`Feeds::_search_to_sql` is spliced into the headline query with its dry-run
  precheck — see the M3 search note below). NOTE(port): the plugin-feed branch
  is not ported (no plugin feeds in the deployment); `Config::get_version()`
  is a placeholder const.
- **Harness normalizer (M2 additions):** smart-date / bare-time tokens collapse to one
  token (they differ run-to-run and server-to-server); URL authority ports are stripped
  (oracle vs Rust run on different ports by construction).
- **External JSON API + feed-tree date fix** (2026-09-07): `api/index.php`
  (API level 23) ported to `crates/ttrss/src/api.rs` — the `{seq, status, content}`
  envelope, the `API::before()` gate (login / `ENABLE_API_ACCESS`), and the read methods
  `login`, `isLoggedIn`, `getConfig`, `getUnread`, `getCounters`, `getFeeds`,
  `getCategories`, `getFeedTree`, `getLabels`, `getHeadlines`, `getArticle`, `getPref`
  (all reuse the M2 building blocks: `feeds::getfeedtree`, `counters::get_counters`,
  `headlines::get_headlines_query`, `article::get_labels`/`get_enclosures`, `prefs`).
  Two latent bugs surfaced and fixed against the oracle: the feed-tree `param` datetime
  now uses `time::make_local_datetime` (was a time-only `to_char 'HH24:MI'`; PHP includes
  the date for non-today feeds), and `labels::get_labels_json` / the API label resolution
  now match PHP `Article::_get_labels` — a **non-empty** `label_cache` is decoded and
  returned as-is (or `[]` if invalid/empty/`no-labels`) and never re-queries the label
  tables; only an **empty** `label_cache` hits the hard path (`ttrss_user_labels2`).
- **Verified (2026-09-07):** `oracle-a` was re-recorded from the freshly reset golden
  DB, which closed the three stale-fixture drifts noted in the 2026-09-05 pass (`009
  rpc_counters` natural-plan order, `013 feed_tree` unread, `010 rpc_sanity_check`
  persisted view prefs — all were dataset artifacts; Rust matched the live PHP oracle on
  the same DB). The external JSON API (033–045) was ported in the same pass. **44/61**
  steps now diff clean: the full auth flow (login/shell/logout, admin + readonly + OTP),
  `rpc_runtime_info`, all feed-tree steps, all four headlines steps, `rpc_counters`
  (values + PHP order), `rpc_sanity_check`, the whole external JSON API (033–045),
  syndication (046–048), and feed icons (049–050).
  Remaining mismatches after the M2 close-out (Bucket A, below) are unimplemented
  surface, not defects: only the M3 logout flow (051–052) and the network-dependent
  `checkforupdates` stub (011) remain.
- **M2 close-out (Bucket A, `backend.php` reads, 2026-09-16):** the remaining
  `backend.php` read handlers and the `/prefs.php` page are ported, taking the harness
  to **58/61** (605 tests). Added: `Article::view`/`search` (022/023),
  `Pref_Prefs::getProfiles` (031), `Pref_Feeds::inactiveFeeds` (016),
  `Pref_Labels::getlabeltree` (024), `Pref_Filters::getfiltertree` (025),
  `Pref_System::getscheduledtasks` (028), `Pref_Users::index`/`userdetails`
  (026/027), `Feeds::updatedebugger` (021, `text/html`), `Pref_Prefs::previewDigest`
  (032), the plugin handlers (029/030), and the `/prefs.php` SPA page (007).
  Remaining mismatches are disabled in the harness (`skip = true` in
  `scenarios.template.toml`; steps kept for re-enablement, numbering
  preserved):
  - `checkforupdates` (011) — network-dependent, not an equivalence target;
  - `previewDigest` (032) — flaky by construction: the "last 24h" window and
    as-of date drift relative to the recording time;
  - the admin logout flow (051–052) — `public.php?op=logout` CSRF/session
    teardown not yet ported (Rust returns the JSON `E_UNAUTHORIZED` gate
    instead of the 302 + shell);
  - the otpuser flow (058–061) — disabled for now.
  The active harness is **53/53 clean** against `oracle/data/runs/oracle-a`.
- **M3 (mutations I: article state), in progress (2026-09-07):** the
  Rust-server mutation surface is ported and diffed against the PHP oracle with
  the `db_state`-capturing M3 corpus (`oracle/corpus/scenarios-m3.toml`, now
  **21 steps**). Done and **21/21 clean**: RPC catchup (read/unread/toggle),
  mark star/unstar/toggle, publish/unpublish, plus the new `Article` handlers
  `setScore` / `setArticleTags` (transactional `ttrss_tags` + `tag_cache` rewrite,
  `FeedItem_Common::normalize_categories` ported to `util::normalize_categories`),
  `assigntolabel` / `removefromlabel` (`labels::add_article` / `remove_article`),
  `printArticleTags`, `completeTags`, `getmetadatabyid`, `redirect` (302/404 via a
  new `DispatchOutput::Raw`), and the catchup-scope handlers `RPC::catchupFeed` /
  `Feeds::catchupAll` (full `Feeds::_catchup` port in `feeds::catchup_scope`).
  Test parity: `ArticleHandlerTest.php` (17) → `tests/article_handler_parity.rs`
  (row 15 below). Notes: the `search` scope in `Feeds::_catchup` is ported
  (the `_search_to_sql` fragment is spliced into the mark-read UPDATE);
  article `note` (notes are plugin territory) is excluded from M3.
  **Search (M3+):** `Feeds::_search_to_sql` fully ported in
  `crates/ttrss/src/search.rs` (oracle-verified: the `str_getcsv` tokenizer,
  the three quote-grouping `preg_replace` passes, and the keyword loop —
  71 exact fragment cases incl. date-relative, in `tests/search_to_sql.rs`;
  `@date` parsing in `search_date.rs`). The headline view splices the
  fragment with its dry-run precheck (unexecutable → `false` + "Incorrect
  search syntax: <words>."), and `_catchup` accepts `search_query`/
  `search_lang` (7 DB tests in `tests/search_scope.rs`). The `af_psql_trgm`
  plugin-search fallback (`HOOK_SEARCH`) remains M7 territory; with the
  default (empty) search language, plain FTS words are unexecutable
  (Postgres rejects `to_tsquery('', …)`) — the actual tt-rss behavior,
  verified in the oracle. Remaining M3 work: port `notes` (the `note`
  handler) if it is in-scope, and re-enable the disabled main-corpus logout steps
  (051–052) once the session teardown is ported. See §3a M3 notes.
  **Feed CRUD (M4):** the `Pref_Feeds` write surface is ported in `feeds.rs`
  (dispatch in `rpc.rs`): `editSave`/`batchEditSave` (shared `editsaveops` —
  the single-feed owner-scoped save plus per-column batch UPDATEs; `cat_id = 0`
  → NULL, `cat_id` gated on `ENABLE_FEED_CATS`, `auth_pass` encrypted up front
  when a key is set and the batch path clears `auth_pass_encrypted`), `remove`
  (per-id `remove_feed` — the feed row + its access keys deleted in a
  transaction, starred entries preserved feedless, non-starred entries cascade
  away, favicon-cache file removed best-effort; negative id → `Labels::remove`),
  and `addCat`/`renameCat`/`removeCat` (the `Feeds::_add_cat`/`_remove_cat`
  trio). Plugin hooks are no-ops. Rust-only (no PHP `Pref_Feeds` config
  tests): DB tests in `tests/feed_crud.rs`.
  Second increment adds **ordering** (`catsortreset`/`feedsortreset` zero the
  `order_id` column; `savefeedorder` walks the dojo `payload` tree and persists
  each feed/category `order_id` + `cat_id`/`parent_cat` — the recursion is an
  explicit `Vec` work stack, not recursion), **access keys** (`clearKeys`/
  `regenFeedKey`/`getSharedURL` via `Feeds::_{clear,update,get}_access_key` +
  `uniqid_short`, share URL built with a PHP-faithful `http_build_query`), and
  `removeIcon` (delete the `feed-icons` cache file + reset the feed's
  `favicon_*` columns). 20 DB tests in `tests/feed_crud.rs`; `savefeedorder`
  verified against the real PHP logic (same resulting `order_id`/`cat_id`/
  `parent_cat` for reorder + move payloads). A third increment adds
  **`uploadIcon`** (multipart file upload → write the `feed-icons` cache file +
  set `favicon_is_custom`, gated on `Config::MAX_FAVICON_FILE_SIZE`, returning
  `{rc, icon_url}` with the `E_ICON_*` markers). This required generalizing the
  `backend.php` request path in `rpc.rs`: the handler now parses the request
  itself (`parse_backend`) instead of the `Form<RpcForm>` extractor — branching
  on `Content-Type` into `multipart/form-data` (axum `Multipart`, the file part
  threaded to the handler as `Option<&[u8]>`), `application/json`, or
  `application/x-www-form-urlencoded` / GET query — with multipart text fields
  landing in `extra` as `Value::String` (matching the old form path). 21 DB
  tests in `tests/feed_crud.rs` + 4 `parse_backend` unit tests (get / json /
  urlencoded / multipart); the 53-step main corpus still matches 53/53.
  A fourth increment ports the **OPML export** (`OPML::export` / `opml_export`,
  in `opml.rs`): the owner category + feed tree is walked iteratively
  (sub-cats before feeds; empty categories *retained* — the PHP emits every
  titled category and its "cleanup empty categories" pass is a no-op because
  it queries `//outline[@title]` while exported outlines use `text`) and
  serialized with **quick-xml** (`se::Serializer::with_root(...).indent(' ',
  2)` — the document is built as a typed `Doc`/`Outline` tree, not
  hand-assembled), yielding *functionally equivalent* OPML (2-space indent,
  `text`/`xmlUrl`/`htmlUrl` + the `ttrss*` attrs when `include_settings`),
  served as an
  `application/xml+opml` attachment via a new `DispatchOutput::Download`
  variant (`export` is login-gated + CSRF-ignored, like the PHP). 4 DB tests in
  `tests/opml_export.rs`. The `tt-rss-prefs`/`tt-rss-labels`/`tt-rss-filters`
  settings section is a TODO (needs `ttrss_user_prefs2` + labels + filters +
  `Config::SCHEMA_VERSION`). A fifth increment ports the **OPML import**
  (`OPML::opml_import` / `importOpml`, in `opml.rs`): the uploaded `opml_file`
  (the multipart `Option<&[u8]>`) is parsed with the quick-xml **pull** parser
  (flat element list + child indices; `text` + CDATA captured) and the
  `//opml/body/outline` tree is walked with an explicit work stack (in place of
  the PHP recursion). A node with `text`/`title` and no `xmlUrl` is a category
  (or one of the three settings sections, which are not created as feed cats);
  otherwise it's a feed. Categories dedupe by title + parent
  (`get_feed_category` + `Feeds::_add_cat`), feeds by `feed_url` + owner (title
  defaulting to `[Unknown]`, `ttrss*` attrs honored); top-level feeds land in an
  `Imported feeds` cat if present else uncategorized. The settings sections are
  applied via `Labels::create` (deduped by caption), `Prefs::set` (known pref
  keys only), and a `ttrss_filters2`/`_rules`/`_actions` insert from the filter's
  JSON CDATA payload (`filter-type="2"`, rules resolved by feed/cat title via
  `Feeds::_find_by_title`). `import` is login-gated (like all `OPML` ops) but
  NOT CSRF-ignored (it's a write). The RPC surface the UI calls is
  **`Pref_Feeds.importopml`** (the `opml_import_form` in `Pref_Feeds::index`
  posts `op=Pref_Feeds&method=importOpml` with the `opml_file` multipart
  upload — the port initially lacked the dispatch arm and the import 404'd as
  `E_UNKNOWN_METHOD`); the response body is the PHP `opml_notice` lines (3×
  `&nbsp;` per depth + `<br/>`: `Processing category:`, `Adding feed:` /
  `Duplicate feed:` (by title, or URL when untitled), `Adding label` /
  `Duplicate label:` (HTML-escaped), `Setting preference key … to …`,
  `Adding filter …` after the insert), and `print_error`'s `format_error`
  div (`Upload failed with error code 4`, UPLOAD_ERR_NO_FILE) when no file was
  uploaded. The walker also matches PHP's category-title handling: 250-char
  truncation, `text` falling back to `title` only when the truncated `text`
  is falsy, and a falsy (empty) `text`/`title` node treated as a feed, not a
  category. 12 tests in `tests/opml_import.rs`, incl. a handler-level
  multipart test for the `importopml` route. Only

  A sixth increment ports the read-side **`editfeed`** form-data handler
  (`Pref_Feeds::editfeed`, in `feeds.rs`): the data the single-feed edit dialog
  needs — the **full feed row** (all `ttrss_feeds` columns, mirroring PHP's `find_one(…)->as_array()`; the 6 nullable timestamp columns are cast `::text` so `NULL` → `null`), a computed `icon` (`Feeds::_get_icon`/`_get_icon_url` → `self_url` + `/public.php?op=feed_icon&id=<id>` when the feed has an icon, else `""`; special feed IDs map to Material icons), a decrypted `auth_pass` (`Feeds::decrypt_feed_pass`, base64 → PHP-unserialize → envelope-decrypt), the category `<select>` (`Controls::select_feeds_cats`, ported
  to an iterative pre-order DFS over a single `ORDER BY title` fetch; the `cats.select` string is **byte-exact** vs PHP — verified against the running oracle: the multi-line `<select name=…>` tag with 24-space continuations, nested titles indented by one U+2003 (em space) per level, a disabled `<option>` of 15×U+2015, single-quoted `value='%d'` category options, and the double-quoted `value="0"` `Uncategorized` tail), the update/purge interval options (with the `Default interval (…) (N days)` annotations, the non-standard-value fallback to `""`, and the
  `FORCE_ARTICLE_PURGE` override — a new `Config::force_article_purge` field), the
  user's `access_level`, and the text-search language list
  (`get_ts_languages` = `pg_ts_config` `cfgname`s, `ucfirst`ed). `plugin_data` is
  empty (no plugins ported). `editfeed` is login-gated but NOT CSRF-ignored
  (matching PHP, where `csrf_ignore` = `index`/`getfeedtree` only). The `feed` object is the **full row** (40 keys = 39 columns + `icon`), and `cats.select` is **byte-exact** vs the PHP oracle (verified by diffing the Rust output against a live `select_feeds_cats` run on the golden DB — 602 bytes identical); the overall JSON is *functionally equivalent* (same data; `serde_json` sorts keys / does not escape non-ASCII or `/`, which the frontend's `JSON.parse` transparently normalizes).
  5 tests in `tests/feed_edit.rs`.

  A seventh increment ports the read-side **`editfeeds`** (batch-edit) dialog
  (`Pref_Feeds::editfeeds`, in `feeds.rs`, dispatched as `DispatchOutput::Html`):
  the HTML the frontend feeds into a `dijit.Dialog` (via `innerHTML`). Per the
  AGENTS.md "Frontend HTML forms" finding, the browser normalizes whitespace,
  so the HTML lives in `templates/editfeeds.html.jinja` (rendered with
  minijinja, the established pattern used for the Atom feed) and the handler
  just supplies the context: a notice, three hidden fields (`ids`/`op`/
  `method=batchEditSave`), and a three-tab `TabContainer` (General /
  Authentication / Options). The General tab has the category
  `select_feeds_cats` (only when the `ENABLE_FEED_CATS` pref is set), the
  language `select_tag`, and the update/purge `select_hash` selects (the purge
  one only when `FORCE_ARTICLE_PURGE == 0`), each paired with a
  `_batch_toggle_checkbox` (a "Check to enable field" checkbox carrying
  `data-control-for` + `onchange=App.dialogOf(this).toggleField(this)`). The
  Authentication tab has the disabled `auth_login`/`auth_pass` inputs
  (single-quoted attributes, `autocomplete='new-password'`). The Options tab
  has the five boolean checkboxes (`include_in_digest`,
  `always_display_enclosures`, `hide_images`, `cache_images`,
  `mark_unread_on_update`) each with a caption + toggle. The footer has the
  Save (`type=submit`) + Cancel buttons. The rendered output is **structurally
  identical** to the PHP oracle (verified by whitespace-normalizing both and
  diffing — 6881 normalized chars identical); only the whitespace the browser
  collapses differs. `editfeeds` is login-gated but NOT CSRF-ignored (like
  `editfeed`). 5 structural tests in `tests/feed_edit.rs` (parse with
  kuchikiki; assert the fields, `name`/`data-control-for`/`dojoType`
  attributes, option values, and captions). Only the export's settings section
  remains for Feed CRUD.

  **Filter CRUD (M4):** the `Pref_Filters` write surface is ported in
  `filters.rs` (dispatch in `rpc.rs`), alongside the existing read-side
  `getfiltertree` / rule+action name helpers. `editSave` updates the owned
  filter row (title `trim`-med, *not* tag-stripped — the PHP intentionally
  allows `<` in titles) and re-saves rules/actions via `_save_rules_and_actions`
  (delete-then-reinsert; each `rule`/`action` param is an array of JSON strings,
  decoded, `id`-stripped, deduped; score-action `+N` → integer, tag/ignore-tag
  params run through `util::normalize_categories`, label actions store the
  `action_param_label`). `add` creates a filter (default checkboxes) + posted
  rules; `remove` deletes the owned filters (rules/actions cascade); `clone`
  clones each selected filter (`Clone of <title>` or an explicit
  `new_filter_title`, disabled, rules/actions copied by
  `_clone_rules_and_actions`); `join` merges the selected filters into the first
  (base) — moving rules/actions, deleting the rest, setting `match_any_rule` —
  then `_optimize` de-dupes (keep the first row of each unique rule/action,
  delete the rest). `savefilterorder` walks the dojo `payload` and persists each
  `FILTER:<id>` `order_id`; `filtersortreset` zeroes the owner's `order_id`.
  The shared `checkbox_to_sql_bool` helper was moved to `util::checkbox`
  (used by both `feeds.rs` and `filters.rs`). Write ops are login-gated +
  CSRF-checked; `index` / `getfiltertree` are the CSRF-ignored reads
  (`Pref_Filters::csrf_ignore`). Rust-only (no PHP `Pref_Filters` config tests):
  9 DB tests in `tests/filter_crud.rs`.

  **Filter read side (M4):** the edit-dialog prefill is ported in `filters.rs`:
  `edit` returns the owned filter row (or the empty-filter defaults for
  `id = 0`) with its rule list (computed `name`, `feed_id` array, `inverse`
  omitted when false), action list (`action_param_label` + `name`), the
  `filter_types` / `action_types` maps (filter type 5 unsupported), and the
  owner's labels; `printRuleName` / `printActionName` render a single
  rule/action's HTML name (or the PHP's `malformed` / `invalid … JSON`
  literals). `get_rule_name` resolves each `feed_id` element to a feed title, a
  `CAT:<id>` category title, or "All feeds". Rust-only: 5 DB tests in
  `tests/filter_read.rs`. The feed multi-select (`editrule` + `_feed_multi_select`)
  is also ported: an explicit work-stack DFS (no recursion) emits each category,
  then its whole subtree, then its feeds (U+2003 em-space indented by level),
  with `CAT:<id>` / feed-id / "All feeds" / "Uncategorized" selection, the flat
  feed list when `ENABLE_FEED_CATS` is off, and the PHP's
  `<select multiple=<TAB>rue"` opening quirk — byte-for-byte the PHP (4 DB tests in
  `tests/filter_editrule.rs`, oracle-diffed via `oracle/recorders/feed_multi_select.php`).
  The `index` dojo page is also ported: the HTML lives in a minijinja template
  (`templates/filters_index.html.jinja`) per the AGENTS.md large-markup rule; the
  handler reads/stores `prefs_filter_search` in the session and supplies the one
  dynamic value (the `clean()`ed, html-escaped search box). The `PluginHost` hook
  is a no-op and `__()` is the identity. Structure verified against the PHP (3 DB
  tests in `tests/filter_index.rs`; `oracle/recorders/filters_index.php`).

  **Filter test dialog (M5, was DEFERRED):** `testFilterDo` is ported — the
  "Test Filter" preview dry-runs the posted rules against the owner's
  articles (read-only). The shared `RSSUtils::eval_article_filters` engine
  now lives in `ttrss::core::filters` (regexes via the `regex` crate with the
  PHP `/$reg_exp/iu` semantics — `(?i)` + UTF-8; per-rule `inverse`,
  `match_any_rule` short-circuit, filter-level `inverse`, and the
  `matched_rules`/`matched_filters` out-params carrying the last-processed
  rule + the regexp captures). The handler builds the fake all-rules filter
  from the `rule[]` JSON params (invalid JSON / non-objects skipped), applies
  the per-rule feed/category scope to the query (`feed_id = N`, `CAT:N` →
  `cat_id = N`, `CAT:0` → `cat_id IS NULL`; OR-joined within a rule, joined
  `match_any_rule ? OR : AND`), and formats each match the way the PHP does:
  `highlight_words_str` (hence the DOM round-trip's outer `<span>` on
  highlighted titles/previews), the `&hellip;` + 200-char content preview
  (over the **tag-stripped** content — `strip_tags` is specific to this
  preview path, so a rule matching only inside a tag attribute previews from
  the start with no `&hellip;` and no highlight), the per-rule-type preview
  (`&mdash;` / link / author / tag icon), and the plain rule name
  (`get_rule_name`, incl. its trailing-space quirk and the always-present
  `(inverse)` suffix here, since `testFilterDo` does `$rule['inverse'] ??=
  false` before naming — the PHP `isset()` check is key-presence, not
  value). One PHP quirk is reproduced as `Err(())` → HTTP 500 with an empty
  body: a matched rule whose highlight word (the match, `strip_tags`-ed,
  200 chars) is empty — e.g. an `inverse` filter whose rule matched nothing,
  or a regex matching the empty string — makes
  `Sanitizer::highlight_words` loop forever and the request dies at
  `max_execution_time` (verified against the live oracle: ~39 s → 500
  empty). The request-parse boundary now normalizes PHP's `$_REQUEST`
  bracket fields (`rule[]` → `rule` array) so the SPA's editSave /
  testFilterDo forms reach the handlers in the shape PHP sees. Verified by
  30 core unit tests (1:1 from `tests_functional/RSSUtilsTest.php`
  `test_eval_article_filters_*`) + 14 DB tests in `tests/filter_test.rs`
  (title/content/tag rules, feed / category / uncategorized scopes,
  match-any vs match-all, inverse ×2 incl. the fatal path, the
  strip_tags preview, limit/offset, malformed-rule skip, no-rules, and the
  `rule[]`/`action[]` editSave regression through the real `rpc::handler`)
  - the oracle corpus `oracle/corpus/scenarios-filter-test.toml` (11 steps
  against the live PHP — 11/11 byte-identical, incl. the 500).

### Test-parity program (PHP `#[test]` → Rust `#[test]`, 2026-09-05)

> **STATUS: ✅ COMPLETE (2026-09-06; row 15 added 2026-09-07 with M3).** All **15/15 rows** ported and green —
> every PHP `#[test]` suite that maps to an already-ported Rust module is now
> replicated 1:1 (per-row counts in the table below). Ground truth at completion:
> `cargo nextest run` = **572 pass / 0 fail** (full workspace; **587** after the
> row-3 `server::prefs` REVISIT resolution below, 2026-09-06), `cargo clippy
> --workspace --all-targets` clean (only the pinned `sqlx-postgres 0.7.4` upstream
> note). PHP cases that don't port yet are either gated on modules not yet
> ported (M2 `Feeds::view`/`search`, M4 config CRUD, M5 admin/API, M6 feed
> engine, M7 plugins) or parked as `REVISIT`/N-A blocks in the Rust test code —
> see **What's next** below. Three real bugs were root-caused and fixed along the
> way (sqlx 0.7.4 statement-cache; `labels::remove` silent rollback; `#[sqlx::test]`
> cross-binary DB-drop race) — see the infra-findings notes further down.

The port is verified end-to-end by the **harness** (response/DB diff vs the PHP
oracle), but the *unit-test* surface is thin: the PHP suite is large
(`SanitizerTest` 162, `FunctionsTest` 73, `UrlHelperTest` 69, `TimeHelperTest` 41,
`PrefsTest` 29, `ArticleTest` 32, `SessionsIntegrationTest` 29, `CryptTest` 23,
`ConfigTest` 16, `LabelsTest` 15, `RPCTest` 10, …) while Rust has ~72 ad-hoc
`#[test]`s. Goal: for every Rust module, **replicate the corresponding PHP
test cases** so behavior is pinned by the same inputs/expectations the PHP
authors wrote, not just by the current golden corpus.

Rules: (1) mirror each PHP test case 1:1 (same inputs, same expected values);
(2) a PHP case that exercises a feature the Rust port omits is marked with a
`// TODO(port): …` and left commented, not silently dropped; (3) PHP-side
helpers (`Prefs::set`, fixtures) are translated to the Rust equivalent
(`TzCtx`, `PrefResolver::new(…)`, etc.); (4) commit **per module** so each
replication is an atomic, independently reviewable unit;
(5) **DB-dependent PHP cases are NOT dropped** — they are listed as a
`REVISIT` placeholder block in the Rust test module (naming each PHP test) and
flagged in the table below, to be implemented once the corresponding Rust
module has DB write access (a `set`/`reset`/… port or a testcontainer).

**REVISIT (DB-backed) ledger:** cases that need a live DB (writes/reads via
the Rust port) and are parked as `REVISIT` blocks in the Rust test code:

*None outstanding (2026-09-06): the 14 `server::prefs` cases this ledger
tracked were ported 1:1 in `tests/prefs_parity.rs` alongside the `Prefs::set` /
`reset` / `get_all` / profile-surface port (see row 3).*

| # | PHP suite | Rust module | status |
| --- | --- | --- | --- |
| 1 | `TimeHelperTest` (41) | `server::time` | ✅ 24 ported (applicable); N/A: `$long`/`no_smart_dt`, uid/profile, `convert_timestamp` |
| 2 | `SanitizerTest` (162) | `server::sanitizer` | ✅ **256** cases across all four PHP sanitizer suites — `tests/mocked/SanitizerUnitTest` (81), `tests_functional/SanitizerTest` (11), `tests/integration/SanitizerTest` (2), `tests_integration/SanitizerTest` (162) — ported 1:1 into `sanitizer_parity.rs` (+`sanitizer_parity/{mocked,functional,unit,integration}.rs`). Ground truth: PHP 8.5.9 / libxml 2.13.09 oracle. 5 cases carry a NOTE — 2 where the PHP test is itself buggy (the oracle fails it as written: `<source>` without `src` is not stripped; `<font color="red">` survives so the exact-string assert fails) and 3 where libxml 2.13 entity-encodes non-ASCII in `saveHTML` (Rust serializes raw UTF-8); each is ported 1:1 in inputs with the corrected / semantically-equivalent expectation. Re-implemented `validate`/`rewrite_relative`/`build_url`/`parse_url`/`has_disallowed_ip`/`canonicalize_ipv4_literal` to match PHP exactly (case-preserving, no dot-segment resolution, `//`→https, data/mailto/magnet exceptions, IP-literal canonicalization); added `iframe_whitelisted`, `wrap_iframes_in_div`, `highlight_words[_str]`, head→body fixup for `<noscript>`. |
| 3 | `PrefsTest` (29) | `server::prefs` | ✅ **29/29** (15 pure + 1 extra guard in `prefs.rs::tests`; 14 DB-backed in `tests/prefs_parity.rs`). `Prefs::set`/`reset`/`get_all`/`create_profile` (+`get_profile_id`/`profile_exists`) ported to `server::prefs`; `all_defs()` = the ordered `_DEFAULTS` table (drives `default_for` + `get_all`); faithful PHP `(int)` cast; PHP 8.1+ PDO storage forms (bool → `"1"`/`""`); `headlines::pref_set` (the `_DEFAULT_VIEW_*` upsert) now delegates to `prefs::set`; PHP built-in `strip_tags` moved to `server::util` |
| 4 | `FunctionsTest` (73) | `server` / `core` helpers | 69 ✅ + 4 parked (REVISIT). Ported 1:1 (2026-09-06): `clean` (10), `truncate_middle` (7), `mb_substr_replace` (6), `arr_qmarks` (6), `uniqid_short` (4), `get_random_bytes` (3), `t_sprintf` (2), `t_nsprintf` (1), `gzdecode` (1) — `server::util` (+`::tests`); `bool_to_sql_bool` (2), `checkbox_to_sql_bool` (2), `sql_bool_to_bool` (3, inputs extended to the PHP suite's exact values) — `core::config::tests`; `truncate_string` (11 — the custom-suffix case completed the row, `headlines.rs`), `get_theme_path` (3), `get_scripts_timestamp` (2) — `render.rs` (existing ports, tests added); `theme_exists` (1) — `rpc.rs`; `with_trailing_slash` (5) — `main.rs`. The two private `clean` copies (`sanitizer.rs`, `article.rs::cleaned_str`) now delegate to `util::clean_str`. Parked: `implements_interface` (4) — PHP `class_implements()` reflection; no Rust consumer in the port yet (revisit if a plugin/handler type-check surface is needed). `flate2` added as a workspace dep (lockfile version). |
| 5 | `UrlHelperTest` (69) | `server` / `core` url | ✅ `validate` (9, `sanitizer.rs`) + `url_to_youtube_vid` (6, `headlines.rs`); ~~REVISIT: trailing-slash on host-only URLs + scheme-case preservation~~ **resolved** by the row-2 faithful `validate` (case preserved, no trailing slash, IDN→punycode); `rewrite_relative` + `build_url` + `canonicalize_ipv4_literal` now ported (see row 2; `rewrite_relative_cases` added); N/A: `resolve_redirects` (4), `fetch` (16), `has_disallowed_ip` DNS cases |
| 6 | `CryptTest` (23) | `core::crypt` | ✅ 12 primitive cases; 7 N/A (config lookup, base64 codec, `algo` whitelist live in the caller — REVISIT block) |
| 7 | `ConfigTest` (16) | `server::main` + `core::config` | ✅ `matches_self_url` (18 provider cases, `main.rs`); `cast_to` covered in `prefs::tests`; N/A: `get_self_url` a–k + `get_self_dir` (self URL from env); REVISIT: IDN/Punycode host normalization |
| 8 | `UserHelperTest` (8) | `server::auth` | ✅ 8/8 ported: 7 in `tests/user_parity.rs` (`#[sqlx::test]` + fixtures + `common::make_db`); session-id regeneration in `session_parity.rs` (row 9) on `auth::build_login_session` |
| 9 | `SessionsIntegrationTest` (29) | `server::session` | ✅ 29/29 ported (`tests/session_parity.rs`) + row-8 regeneration case = 30 tests. `Sessions` class methods ported to `server::session::{exists,read,write,destroy,validate_session}` (raw string-level, matching `SessionHandlerInterface`); envelope helpers (`encrypt_envelope`/`decrypt_envelope`) + `Session::from_items` in `core::session`; `validate_session` (+`single_user_mode` config, `ACCESS_LEVEL_*` consts in `core::user`) now used by `rpc::dispatch`; middleware `load` auto-creates like PHP `session_start`; `sql_bool_to_bool` canonicalized in `core::config` (used by prefs + config) |
| 10 | `ArticleTest` (32) | `server::article` | ✅ 32/32 ported (`tests/article_parity.rs`). `Article` static surface ported to `server::article`: `get_tags` / `get_labels` (+ `Labels::update_cache` marker) / `catchup_by_id` / `purge_orphans` / `get_enclosures` / `labels_of` / `feeds_of` / `get_image` (XPath subset via iterative kuchikiki walk, YouTube embed / video poster / img / enclosure fallback / ALBUM detection) / `format_enclosures` (+ `can_inline` via prefs `STRIP_IMAGES`); `rewrite_relative` now `pub(crate)`; PluginHost hooks + DiskCache wrapping omitted (no plugins / no disk cache) with NOTEs |
| 11 | `FeedsCountersIntegrationTest` (40) | `server::counters` | ✅ 40/40 ported (`tests/counters_parity.rs`). `Feeds::_get_counters` ported to `counters::get_counters` (`FeedOrTag` = feed id **or** tag name, PHP `(int)`/`is_numeric` casts, tag branch, `FRESH_ARTICLE_MAX_AGE` pref lookup); `Counters::{get_all, get_conditional, get_labels}` exposed as `counters::{get_all, get_conditional, get_labels}` on the existing M3 counter machinery (private `get_all`/`get_labels` renamed `all_counters`/`labels_counters`); `feed_counters` gained the PHP `$n_feed === FEED_ARCHIVED` strict-int flag; PHP `setUp()`'s `Prefs::set(FRESH_ARTICLE_MAX_AGE, 999999, 1, null)` modeled as a `ttrss_user_prefs2` INSERT |
| 12 | `FeedsHeadlinesIntegrationTest` (20) | `server::headlines` | ✅ 20/20 ported (`tests/headlines_parity.rs`). `Feeds::_get_headlines` exposed as `headlines::get_headlines_query(&Db, &HeadlinesParams)` (pub `HeadlinesQuery` = the typed form of PHP's 9-tuple return); `since_id` plumbed into the private query builder (numeric-feed main query + first-id query, not the tag-browsing query — faithful to PHP); `HeadlinesParams::new(feed, owner_uid)` carries the PHP defaults (limit 30, `view_mode` all); prefs loaded with profile `None` (no session profile at this layer) |
| 13 | `LabelsTest`/`LabelsIntegrationTest` (15/35) | `server::labels` | ✅ 50/50 ported (15 pure + 35 DB, `tests/labels_parity.rs`). New `server::labels` = the PHP `Labels` class: `label_to_feed_id`/`feed_to_label_id` (canonical i64 home; `article` re-exports), case-insensitive `find_id`/`find_caption`, `get_all`/`get_as_hash`, transactional `create`/`remove` (`remove` binds the varchar `ttrss_access_keys.feed_id` as a **string** — PHP PDO binds ints as strings), `add_article`/`remove_article`, `clear_cache`, `update_cache` + `get_labels_json` (string-level `Article::_get_labels` with the cache-hit-as-is semantics; `article::get_labels` delegates to it) |
| 14 | `RPCTest` (10) | `server::rpc` | ✅ 10/10 ported (`tests/rpc_parity.rs`). PHP `RPC::mark`/`publ`/`delete` ported to `article::{mark, publ, delete}` (cleaned string params, Postgres-side `$1::boolean` cast like PDO string binding, `pub` request key for `publ`, `Null` output on empty `delete`), dispatched from `rpc.rs`; the `markSelected`/`publishSelected` arms were already present; tests seed a `Sess` with `uid = 1` (PHP `DbTestCase`'s `$_SESSION["uid"]`) |
| 15 | `ArticleHandlerTest` (17) | `server::article` (handlers) | ✅ 17/17 ported (`tests/article_handler_parity.rs`). PHP `Article` handler methods invoked directly (`invokeHandler('Article', …)`, `$_SESSION["uid"]=1`) ported to the Rust handler functions: `setScore` (3 — single/multi/zero; `ttrss_user_entries.score`), `setArticleTags` (4 — new/replaces/clears/trims; transactional `ttrss_tags` + `tag_cache`), `printArticleTags` (2 — existing/empty), `completeTags` (3 — matching/no-match/case-sensitive; `LIKE 'search%' LIMIT 10`), `assigntolabel`/`removefromlabel` (2 — `ttrss_user_labels2` + `label_cache`), `getmetadatabyid` (2 — `{link,title}` / `[]`), `redirect` (1 — 302 + `Location`). Also fixed a latent `metadata_by_id` SQL bug (a doubled `\\` line-continuation that made the query invalid; masked because the corpus sent the wrong `id` param so both sides returned `[]`) and corrected corpus steps 022/023 to send `id` — the oracle baseline was re-recorded so they now exercise the real path. `util::normalize_categories` gained its own unit test (trim/lower/sort, `t:` numeric prefix, quote-strip, 250-char cap) |
| 16 | `FeedParserTest` (20) + `FeedItemTest` (35) + `tests_functional/FeedItem{Atom,Common}Test` (92) | `core::feed` | ✅ M6a.1 + M6a.2 + M6a.3 (2026-09-16). `FeedParserTest` (20) → `tests/feed_parser.rs`. `FeedItemTest` (35) → `tests/feed_item.rs` (RSS/Atom `get_id`/`get_date`/`get_link`/`get_title`/`get_content` + `normalize_categories`). `tests_functional/FeedItemAtomTest` (42) → `tests/feed_item_atom.rs`; `tests_functional/FeedItemCommonTest` (50) → `tests/feed_item_common.rs` (shared `get_author`/`get_comments_url`/`get_comments_count`/`get_enclosures`/`normalize_categories`/`count_children`/`subtree_or_text`/`get_element`). XML via `roxmltree`. NOTE: source-element removal is reproduced by skipping `<source>` subtrees in tree walks (roxmltree is immutable); see the `test_constructor_removes_source_element` NOTE. |
| 17 | `RSSUtilsTest` `is_gzipped` (8) + `FunctionsTest` `gzdecode` (1) | `rss_utils` | ✅ M6a.4 (2026-09-16). `rss_utils::is_gzipped` (magic bytes `\x1f\x8b\x08`) + `rss_utils::gzip_decode` (`flate2::read::GzDecoder`), inline unit tests. Covers the "gzip without a `Content-Encoding` header" fallback the PHP's `RSSUtils::is_gzipped` exists for (that method has no PHP call site — the common case is curl's `CURLOPT_ENCODING` / `reqwest`'s `gzip`). |
| 18 | `RSSUtilsTest` pure helpers: `make_guid_from_title` (10) + `decode`/`encode_srcset` (14) + `function_enabled` (2) + `labels_contains_caption` (6) + filter-action helpers `has`/`find`/`calculate_article_score` (16) | `rss_utils` + `core::filters` | ✅ M6b.1 (2026-09-17). `rss_utils::{make_guid_from_title, decode_srcset, encode_srcset, function_enabled, labels_contains_caption}` (+ `SourceItem`/`Label`); the filter-action helpers in `core::filters` (cohesive with the M5 `eval_article_filters` + `FilterAction`, which already carried the `test_eval_article_filters_*` cases). NOTE: `function_enabled` always `true` (no Rust `disable_functions`); the srcset helpers duplicate the pre-existing private copies in `diskcache.rs`/`sanitizer.rs` (consolidation deferred). The DB-backed pipeline (`update_rss_feed`: hash/dedup/upsert/counters) is M6b.2. |
| 19 | `calculate_article_hash` (3) + `UpdateRssFeedTest` (7 + 8 filter) | `rss_utils` + `updates` | ✅ M6b.2a + M6b.2b + M6b.2c (2026-09-17). `rss_utils::calculate_article_hash` (the article dedup key — stable field-ordered SHA-1 over title/content(link)/tags/author/score_modifier/language/timestamp/num_comments/enclosures; `util::sha1_hex` lowercase-hex SHA-1; no plugins → empty prefix), 3 inline cases. `ttrss::updates` (`tests/update_rss_feed.rs`): the deterministic pipeline core `update_rss_feed`/`process_feed` (parse → guid → dedup → upsert `ttrss_entries` + `ttrss_user_entries` → stamp metadata) replays the recorded mock feed (7 cases 1:1 from `UpdateRssFeedTest.php`), plus 8 M6b.2c cases for the filter/label/tag application (`load_filters` → `filter`/`score`×2/`catchup`/`tag`/`ignore-tag`/blacklist/`label`). `update_rss_feed` takes the feed XML as input (the network fetch is M6c); `tsvector_combined` via `strip_tags(title) . \" \" . strip_tags(content)` (vs PHP `Html2Text`, FTS-equivalent). |

#### What's next (after the parity program)

The program itself is done; the remaining PHP `#[test]` suites are gated on
modules that are **not yet ported**, so they land *alongside* those modules, not
before them:

| Remaining PHP coverage | Blocked on (Rust) | Milestone |
| --- | --- | --- |
| ~~`Feeds::view` / `Feeds::search` + `RPC::getAllCounters`~~ **✅ done 2026-09-11** | `headlines::view`/`search` + `counters::get_all_counters` (wired in the `rpc.rs` `Feeds` branch); corpus 009/017/018/019/020/056 clean | **M2** ✅ |
| Feed-parser / RSS-utils (fetch/parse/hash/dedup/filters/enclosures) | `FeedParser` + `RSSUtils` ports (**M6a parser complete**: `FeedParser` + `FeedItem` fields + enclosures/`xml:base`/`c14n` + gzip; **M6b.1 pure helpers + M6b.2a/2b done**: GUID/srcset/`function_enabled`/labels + the filter-action helpers + `calculate_article_hash` + the `update_rss_feed` pipeline core; remaining = M6b.2c filter/label/tag application (`load_filters`) + M6c fetch/daemon) | **M6** |
| DiskCache / Scheduler / counter-cache maintenance | DiskCache port + `daemon` tokio workers | **M6** |
| PluginHost hooks (the `HOOK_*` no-ops left in rows 9–14) | Rust plugin API + first-party plugins | **M7** |
| External-API write ops + admin (`Pref_Users`, system, OTP/QR) | write/admin handlers (API meta + DB-only writes ✅; `Pref_Users` admin slice ✅; `shareToPublished` ✅; `getFeedIcon` ✅; `Pref_System` log/email ✅; batch-add-feeds + case-insensitive dispatch ✅; **password recovery `forgotpass` + API `logout` ✅**; OTP/QR shipped M4; only remaining: `updateFeed`/`subscribeToFeed` → M6) | **M5** ✅ (complete; `updateFeed`/`subscribeToFeed` → M6) |
| Config CRUD (feed CRUD + categories + icons + OPML, filters, access keys) ✅ | M4 mutation handlers | **M4** (complete 2026-09-16; feed subscribe/create → M6) |
**M2 close-out test parity (2026-09-16):** porting the `backend.php` read handlers
surfaced two more PHP suites for already-ported modules, now replicated:

- `FeedsTest.php` (3) + `FeedsIntegrationTest.php` (13) → the `Feeds::_get_title` /
  `_get_cat_title` feed-title resolution, ported 1:1 to `tests/feeds_parity.rs` (12
cases; the one `test_get_title_non_numeric_id` case is N/A — the Rust `feed_title`
  takes an `i64`, so the non-numeric case is handled by the id parser at the handler
  layer). Note: `headlines::feed_title_of` is a near-duplicate of
  `feeds::feed_title` (the else branch returns `""` vs the PHP-faithful `id` string);
carrying a NOTE to centralize on `feeds::feed_title` when next touching `headlines`.
- `DiskCacheTest.php` `rewrite_urls` subset → the *current* implementation rewrites a
  URL only when a cached copy exists (`<self>/public.php?op=cached&file=images/<sha1>`);
  the frozen PHP `rewrite_urls` tests predate that (URL-encoded name, no cache setup),
  so 8 corrected-expectation cases were ported into `diskcache.rs::tests`. The full
  DiskCache class lifecycle tests (instance/put/get/…) remain M6 (the class itself is
  not ported — only `send`/`rewrite_urls`/`get_full_path`/`get_mime_type`/
  `get_fake_extension` are). The `fake_extension` Rust table is an intentional
  subset (feed-icon image types only), so the PHP video/audio/pdf `mimeMap` cases
  do not apply. |
No DB-backed `REVISIT` cases remain (the 14 `server::prefs` cases were
resolved 2026-09-06, see row 3), and row 4 is now complete except the 4
`implements_interface` reflection cases (parked 2026-09-06 — no Rust consumer
for PHP `class_implements()` in the port yet). The M4 config-coverage row is *new* suites never
in the 14-row board
(feed subscribe/delete, categories, filters, OPML, access keys, icons/ordering),
not a re-visit of rows 1–14 (the `Labels` class CRUD is already fully covered by
row 13).

#### Test infrastructure for DB-backed suites (status: COMPLETE, 2026-09-05)

**Goal:** rows 8–14 (DB-backed parity suites) run against a **dedicated** Postgres
(separate from the oracle stack), versioned schema + seed (NOT the golden dump),
isolated per test, fast + repeatable.

**Design (decided — replaces the earlier hand-rolled template-clone; build on
libraries, don't cleanroom the lifecycle):**

- **Server** = a *reused* testcontainers `postgres:15-alpine` container
  (`crates/test-db`, `CONTAINER_NAME = "ttrss_test_pg"`). Reused containers persist
  across runs and are **not removed on handle drop** (verified in testcontainers
  0.27.3 source: `ContainerAsync::drop` returns early for
  `ReuseDirective::Always`/`CurrentSession`). A `#[ctor]` in each test binary
  starts/attaches it and sets `DATABASE_URL` (`crates/test-db::init_database_url`).
  `TTRSS_TEST_PG_URL` overrides with any dedicated instance.
- **Per-test DB** = sqlx `#[sqlx::test(fixtures(...))]`. Confirmed from sqlx 0.7.4
  source (`sqlx-core/src/testing/`, `sqlx-postgres/src/testing/`): creates a fresh
  `_sqlx_test_N` DB per test, applies fixtures (baked via `include_str!`, in order,
  each as one command string), injects the pool, drops the DB on success (failed
  tests keep it; stale DBs are reaped on the next run via the `_sqlx_test.databases`
  bookkeeping table + a 2s margin). Reads `DATABASE_URL` (a superuser) at runtime.
- **Fixtures** = `crates/test-db/fixtures/{schema,baseline,seed}.sql` (versioned =
  the frozen reference's base schema + admin user + PHP `seed.sql`), referenced by
  **explicit path** from each test file (`include_str!` is source-file-relative).
- **Session parity** = reuse `ttrss::core::db::SESSION_SETUP` via the `#[sqlx::test]`
  `PoolOptions<Postgres> + PgConnectOptions` signature + a `before_acquire` hook
  (wrap the result in `Db::from_pool`), so test connections match the server exactly
  (`TIME ZONE 0`, `datestyle = 'ISO, european'`, `cpu_tuple_cost`, `client_encoding`).

**Research notes (verified against vendored/cloned sources, not memory):**

- Repo names here differ from training data: sqlx = `transact-rs/sqlx`
  (`launchbadge/sqlx` 301s there); testcontainers = `testcontainers/testcontainers-rs`.
  Read the `repository` field from the vendored `Cargo.toml`; verify HTTP 200 before
  cloning. GitHub's git endpoint returns `401 "Repository not found"` for
  nonexistent/private repos (public = 200, no auth). Set `GIT_TERMINAL_PROMPT=0`.
- `std::env::set_var` is **`unsafe`** in this toolchain (rustc 1.98) — wrap in
  `unsafe {}` (see `init_database_url`). rust-src is now installed → grep the sysroot.
- testcontainers 0.27.3: `get_host_port_ipv4(5432) → Result<u16>`;
  `get_host() → Result<url::Host>` (enum `Domain/Ipv4/Ipv6`, no `Display` — match it);
  `with_mapped_port(u16, ContainerPort)`; `with_reuse(ReuseDirective)` (re-exported
  at the crate root); `with_container_name(&str)`. testcontainers must stay 0.27
  (testcontainers-modules 0.15 requires `^0.27`).
- `ctor` crate v1.0.13 for pre-test init; sqlx `migrate` feature is already enabled
  in the workspace (required for `#[sqlx::test]` DB management).
- `#[sqlx::test]` also needs sqlx's **`macros`** feature (the attribute is exported
  under `feature = "macros"`, DB management under `migrate`) — added to the workspace
  sqlx features.
- This `ctor` 1.0.13 follows the future-dated unsafe-attributes convention: the
  attribute must be written `#[ctor(unsafe)]` (a bare `#[ctor]` is a hard error).
- sqlx 0.7 decodes **strictly**: tt-rss id columns are `integer` (INT4) → decode as
  `i32`, not `i64` (the old smoke test's `i64` decodes were never run and would have
  failed at runtime).
- nextest 0.9.143: the profile key is `test-threads` (not `threads`).

**Shipped (committed):**

- ✅ `core::db`: `SESSION_SETUP` now `pub`; added `Db::connect_url` +
  `Db::from_pool(PgPool)`.
- ✅ `server` split into lib + bin; `auth::authenticate` fixed to match PHP (SSHA-512
  hash upgrade + `last_login` stamp); `AuthOutcome: Debug`.
- ✅ `crates/test-db` rewritten to the design above: `PgServer::acquire` (reuse
  container or `TTRSS_TEST_PG_URL`), `ensure_test_pg()`, sync
  `init_database_url()` (the `#[ctor]` entry point; no-op if `DATABASE_URL` is
  already set), `ssha512_password`, `seed_user` (returns `i32`). Old template-clone
  machinery (`TestDb`/`SeedLevel`/`ensure_template`/`reap_stale`) deleted; the
  dedicated container's stale `ttrss_tmpl_*`/`ttrss_t_*` databases were dropped.
- ✅ `server/tests/common/mod.rs`: `#[ctor(unsafe)]` init +
  `make_db(PoolOptions<Postgres>, PgConnectOptions) -> Db` (applies `SESSION_SETUP`
  via `before_acquire`).
- ✅ `server/tests/user_parity.rs` = row 8 on the new infra: **7 pass, 1 ignored**
  (session-id regeneration → row 9), ~1.1 s/test.
- ✅ `testing/tests/smoke.rs`: fixture set reproduces the PHP integration starting
  state (version 151, admin user, seed counts) + fresh/writable per-test DB.
- ✅ `.config/nextest.toml` (cargo-nextest adopted; `test-threads = 16`; slow =
  `#[ignore]`).
- ✅ Clippy cleanup pass while at it: fixed the pre-existing warnings in
  `headlines.rs`/`counters.rs`/`sanitizer.rs`/harness `run.rs` (unused assignments,
  needless borrows, redundant closures, `map_entry`, redundant guards, …) —
  including restoring a **missing `#[test]`** on `headlines::tests::
  truncate_string_chars` (a live port of FunctionsTest truncate cases that had
  silently never run). Workspace is now clippy-clean except one upstream
  future-incompat note from the pinned `sqlx-postgres 0.7.4`.
- Verified: `cargo nextest run` → **389 pass, 1 ignored**; per-test DBs are dropped
  on success (empty `_sqlx_test.databases` bookkeeping after a green run).

**Row 9–14 pattern** (each new suite = one file in `crates/ttrss/tests/`):
`mod common;` + per test `#[sqlx::test(fixtures("../../testing/fixtures/schema.sql",
"../../testing/fixtures/baseline.sql", "../../testing/fixtures/seed.sql"))]` with the
`PoolOptions<Postgres> + PgConnectOptions` signature and `common::make_db`. Extra
per-suite state = plain INSERTs after `make_db` (or `test_db::seed_user`).

**Row 9 done** (`session_parity.rs`, 30 tests green): the `Sessions` class port
itself went into `server::session` (raw `exists`/`read`/`write`/`destroy` +
`validate_session`), so rows 10–14 follow the same shape — port the PHP
class surface to the module, then assert the PHP suite 1:1 in the test file.
**Row 10 done** (`article_parity.rs`, 32 tests green) using that shape.
**Row 11 done** (`counters_parity.rs`, 40 tests green).
**Rows 12–14 done** (`headlines_parity.rs` 20, `labels_parity.rs` 50,
`rpc_parity.rs` 10 — all green).

Infra findings from rows 12–14 (affect all DB-backed tests):

- **sqlx 0.7.4 statement-cache bug:** each `PgConnection` keeps a
  `StatementCache` (default capacity 100) keyed by **SQL text only**. Re-using
  the same text with different parameter type OIDs (e.g. an `i32` literal vs
  `i64` for the second parameter) reuses the old prepared statement and the
  server dies with `08P01 insufficient data left in message` (a 4-byte
  INT4 value can't satisfy the 8-byte INT8 receiver). PHP PDO has no such
  cross-call reuse, so `Db::connect_url` and the test `make_db` set
  `statement_cache_capacity(0)`.
- **`labels::create`/`remove` harden:** both now return `Result` (they
  previously swallowed `INSERT`/`DELETE` errors via `.ok()`); the
  `ttrss_access_keys.feed_id` cleanup binds the label feed-id as a string
  (the column is `varchar(250)`; an `i64` bind is a `42883` operator error
  that aborts the whole transaction).
- **`#[sqlx::test]` cross-binary race:** each test binary's one-shot stale-DB
  cleanup drops **connection-less** test databases older than 2 s — including
  databases of sibling binaries running concurrently. A test that sleeps >1 s
  (the harness pool has a 1 s idle timeout) must hold a pool connection across
  the sleep, and `make_db` now sets `acquire_timeout` to 120 s so
  connection-establishment under full-suite load doesn't `PoolTimedOut`
  (nextest `test-threads` lowered 16 → 8 for the same reason).

#### M2 — `Feeds::view` headlines / Sanitizer approach

**DONE (committed):**

- `8efb755` harness: HTML-bearing JSON fields compared **structurally** (canonical
  DOM via existing `scraper`/`html5ever` in `crates/harness/src/html.rs`), not as raw
  strings. `normalize_json` → `html::is_html_fragment(s)` → `canonical_html(s)`.
- `e2a187b` **high-fidelity iterative sanitizer** (`crates/ttrss/src/sanitizer.rs`,
  `kuchikiki`+`html5ever` 0.26): ports `Sanitizer::sanitize` — rewrite `<a>`/`<img>`/
  `source`/`video` (`UrlHelper::rewrite_relative`+`has_disallowed_ip`, `rel`/`target`,
  `referrerpolicy`/`loading`, `srcset` via `RSSUtils`), sandbox iframes, strip
  disallowed elements (whole subtree) + attrs (`on*`,`data-*`,id/style/class/width/
  height/allow/srcdoc, `javascript:` hrefs), highlight words. **All traversals + the
  serializer are iterative (heap stacks)** — no call-stack recursion, deep-nesting
  safe. Deps: `kuchikiki=0.8`, `html5ever=0.26` (workspace + server).
- Harness `canonical_html` now treats `<p>` as **transparent** (libxml HTML4 auto-wraps
  bare text; html5ever HTML5 doesn't) — reconciles the two without chasing libxml.

**DONE (2026-09-11 — all ported; the plan below is kept for reference):** `Feeds::view` (017/018/019/056) + `Feeds::search` (020) +
`RPC::getAllCounters` (009). Wire a `"Feeds"` branch in `crates/ttrss/src/rpc.rs` (add
`Feeds` to `is_protected`; it's CSRF-gated, corpus sends valid token). `Feeds::view`:
feed-ownership check (label/feed/cat) → else `_generate_error_feed` "Feed not found.";
`_get_headlines` SQL (regular feed `feed_id=N`; category `cat_id IN (subcats)` +
`ttrss_feeds.title AS feed_title,favicon_avg_color,`); 32-field article object;
time fields via `TimeHelper::make_local_datetime` (harness normalizes to `<ts>`);
`content` = `sanitizer::sanitize(...)`; `feed_bg_color` via `_color_of` (colormap[15]);
enclosures (`Article::_format_enclosures`); `buttons` = 3 plugins (af_psql_trgm, note,
share `HOOK_ARTICLE_BUTTON`, each +`data-plugin-name`); `content_preview` =
`"&mdash; "+truncate_string(strip_tags(raw),250)`. `view_mode` (req `view_mode`), NOT
`m`, controls the view filter — corpus uses `view_mode=""` so no filter; 017 & 018
return the same articles. `Feeds::search` (020) = `{show_language:true,
show_syntax_help:true,default_language:"",all_languages:Pref_Feeds::get_ts_languages()}`
(query `pg_ts_config`, ucfirst). Re-read `tt-rss/classes/Feeds.php` `view`(430)/
`_get_headlines`(1354)/`search`(558), `TimeHelper.php`, `Article.php:308`.

##### M2 — `Feeds::view` headlines / Sanitizer approach (decided)

The `view()` response's per-article `content` field is the **sanitized article
body HTML** (`Sanitizer::sanitize` on the raw `ttrss_entries.content`, then
`DiskCache::rewrite_urls` (no-op here), then content-transforming plugin hooks
like nsfw's `<details>` wrap). `Sanitizer::sanitize` is *reused* across the SPA
`view()`, the external API, the public/syndication path, and the share plugin.
Lifecycle: raw → sanitize → plugins → JSON `content` → **SPA renders it
wholesale via `innerHTML`** (`Article.js:265`) → client hooks (Dojo parse, nsfw
blur). Plugins transform it deterministically and **entity-form-agnostically**
(nsfw string-wraps; af_youtube_embed matches iframe/enclosure URLs) — nothing
does a byte-sensitive match on the entity spelling/whitespace.

**Decision (B):** the *harness* compares HTML-bearing fields **semantically**
(parsed tag/attr/text structure, ignoring entity spelling + whitespace) — the
RSS feed origin is the canonical source of the content, not us, so we don't
chase libxml's byte form. BUT the **sanitizer itself must be high-fidelity** —
it's the security-critical production logic, so port it *fully* (not just the
golden-dataset subset): drop disallowed elements (whole subtree) + attributes
(`on*`,`data-*`,`id/style/class/width/height/allow/srcdoc`,`javascript:` hrefs),
rewrite `<a>` (`rel`/`target` + `UrlHelper::rewrite_relative`), decorate `<img>`
(`referrerpolicy`/`loading` + `srcset` validation via `RSSUtils::decode/encode_srcset`),
`data:`-URI allowlist, `has_disallowed_ip` (private/loopback) checks, iframe
sandboxing, `STRIP_IMAGES`, `highlight_words`.

- **Parser:** use a real HTML parser (`kuchikiki`/`html5ever` — the harness
  already uses `scraper`/`html5ever` in `crates/harness/src/html.rs::canonical_html`,
  which is exactly the semantic comparator to reuse for HTML fields). NOT the
  libxml2 FFI route (`libxml`/KWARC) — that's byte-exact but a C/clang build
  burden, and byte-parity isn't required given (B).
- **Verified transform (all 49 articles in 017/018/019):** leading bare text →
  `<p>`; disallowed attrs stripped; `<a href>` gains `rel`/`target`; `<img>` gains
  `referrerpolicy="no-referrer"`+`loading="lazy"` (attr order = raw order, new
  attrs appended); `srcset` = `trim(url)+' '+trim(size)` joined by `,`;
  non-ASCII → HTML4 named entities (`&rsquo;` etc., 252-entry table) + `&`→`&amp;`
  (raw content mixes literal chars + numeric `&#8217;` + named forms; the parser
  collapses all to the char model). Since (B), the *named-entity spelling* no
  longer needs to match — only the parsed structure.
- **Corpus steps (satisfied 2026-09-11):** `009 rpc_counters` (`RPC::getAllCounters`),
  `017/018/019/056` `Feeds::view` (headlines), `020` `Feeds::search` (returns a
  search-UI response, *different* shape — `show_language`/`all_languages`, not a
  headline list). Re-read `tt-rss/classes/Feeds.php::view`(430)/`_get_headlines`(1354)/
  `search`(558), `Counters.php`, `Sanitizer.php`, `UrlHelper.php` when resuming.

---

## 3a. Subsystem work notes (reference — by layer, not by milestone)

Detailed per-file / per-method work items, organized by **subsystem (layer)** rather
than milestone. Each phase notes the milestone(s) it feeds (`→ Mx`). This is the
detailed content reference consulted as each milestone is started; the delivery
*order* itself is §3.

### Phase 0 — Foundations & test harness (→ M0/M1)

- Cargo workspace, CI (fmt/clippy/test), GPL-3.0 headers, `rust-version` pin.
- Copy assets (js/, lib/, themes/, images/, locale/, templates/, sql/pgsql) into the repo as data.
- `ttrss::core::config`: all ~60 `TTRSS_*` settings with types + defaults (port `Config::_DEFAULTS`), self-URL detection rules, version detection (build-time embedded git info → `version_static` fallback).
- `ttrss::core::db`: sqlx pool (host/port/user/pass/db/sslmode), `client_encoding`/`timezone` session setup, **migration runner reusing the `.sql` files** (version in `ttrss_version`, per-plugin tables).
- Error type hierarchy mirroring `Errors::*` (E_UNAUTHORIZED, E_UNKNOWN_METHOD, E_SCHEMA_MISMATCH, E_INCORRECT_USAGE, E_NOT_FOUND, E_OPERATION_FAILED, E_PLUGIN_*…).
- **Test harness**: PostgreSQL testcontainer (or service) → run migrations → seed fixture DB; golden-file capture tooling against a reference PHP instance (record `backend.php`/`api/index.php` responses to JSON fixtures for diffing).
- **Exit criteria**: `ttrss::core::db` boots a pristine PG and a v151 DB identically to PHP; config parity test vs PHP `Config::get()` for a matrix of env setups.

### Phase 1 — Domain core (no HTTP) (→ M1/M2)

- Model structs for all tables (sqlx FromRow).
- `Prefs` subsystem: registry (`ttrss_prefs`), get/set with type casting + strip-tags rules, profiles, per-user overrides.
- `users`: password hashing/verification for **all five PHP formats** (SSHA-512/256, MODE2, SHA1X, SHA1) — validate against hashes generated by the PHP implementation; TOTP verify; app passwords (generate/verify); access levels; user CRUD; auth chain (plugin hook skeleton + `auth_internal` + `auth_remote`).
- i18n module: `.mo` loading, `__()`/`T_sprintf()`, Accept-Language selection matching `startup_gettext()` logic (full-locale-then-2-letter, q-factors).
- `TimeHelper` port (smart dates, tz conversion using `iana-time-zone`).
- Loose-type coercion module + property tests.
- **Exit criteria**: unit + golden tests pass for prefs, users (including login against PHP-seeded DB), i18n (sample strings in ≥5 locales), time formatting (time matrix vs PHP `date()`).

### Phase 2 — Feed pipeline (the engine) (→ M6; sanitizer/URL work needed earlier for M2 content rendering)

- `FeedParser` + `FeedItem_*` port over `xml` (RDF/RSS/Atom, namespaces, content encoding, gzip detection).
- `UrlHelper` port: reqwest client (proxy, connect/total timeouts, 429 throttle, conditional GETs, redirect policy) + URL validation + **SSRF guards** (private/reserved IP checks incl. resolved-IP validation, IDN) + srcset + YouTube id.
- `Sanitizer` port over html5ever (allowed elements, disallowed attrs, iframe whitelist, highlighting, URL rewrite to cache, image strip) — differential tests against PHP sanitizer output on a corpus of nasty HTML (reuse `tests_functional/SanitizerTest` corpus).
- `RSSUtils` update pipeline: fetch → parse → dedup by hash/GUID → `ttrss_entries`/`ttrss_user_entries` upserts → filter evaluation (rules → actions: score/labels/tags/hide) → enclosures + favicon caching (DiskCache port) → counter cache updates → error logging → failed-feed disabling.
- `Feeds` core: subscribe (incl. OPML/HTML feed-link discovery), categories (tree), catchup, purge, counters (`Counters` + cache tables), virtual feed definitions, access keys.
- `Article` core: tags, labels, score, notes, enclosures, flavor/OG images, catchup-by-id, orphan purge.
- **Exit criteria**: sanitizer corpus passes; counter math parity tests (feed/cat/label/virtual); ported PHP unit/functional tests for the deterministic pieces (parser, hash, GUID, dedup, filter); a lightweight batch smoke test (fixture feed batch → aggregate/cross-feed invariants). *(The original "golden-file parity of an update run" full-DB diff is **dropped** — write-heavy, low signal-to-noise; see `docs/unified-architecture.md` §1.)*

### Phase 3 — Web server foundation (→ M1)

- axum app: router mirroring entry points (`/`, `/prefs.php`, `/backend.php`, `/public.php`, `/api/`, `/images/`, `/themes/`, `/js/`, `/lib/`, `/cache/`, `/plugins.local/…` static), static file serving with cache-busting mtime queries.
- Sessions: DB-backed store port (encrypted via XChaCha20-Poly1305 when `ENCRYPTION_KEY` set — byte-compatible with PHP session rows), cookie handling (name, secure, httponly, remember-me lifetime), session validation (pwd_hash change, disabled account, plugin hook).
- CSRF tokens (per-session, `hash_equals` → `subtle` constant-time compare).
- Locale negotiation middleware; gettext in error strings.
- `public.php` port: `healthcheck`, `login` (auth chain, single-user mode, profile selection, remember-me, return-URL validation via `matches_self_url`), `logout`, `cached`, `feed_icon`, `dbupdate` (web migration UI), `forgotpass` (captcha arithmetic, email via `lettre`).
- Shell pages: `index.php` + `prefs.php` renderers (plugin CSS/JS injection, theme, CSRF, **ETag/304 conditional behavior** via `send_conditional_html` port).
- `backend.php` dispatcher: Handler trait (`before/after/csrf_ignore/catchall`), protected/administrative gates, JSON envelope, `Errors::to_json`.
- **Exit criteria**: a browser (with unmodified frontend JS) can log in, reach the main shell, and static assets all serve; login/logout/session flows match PHP (including encrypted session interop test: Rust reads session written by PHP and vice versa).

### Phase 4 — Frontend API parity (`backend.php` handlers) (→ M2 read ops, M3/M4/M5 write+admin ops)

Port each handler class, method-by-method, against golden files (read methods land in M2; the rest follow the milestone ladder):

1. `RPC` (legacy): togglepref, setpref, mark, delete, publ, counters, getFeedTree… (whatever App.js/Feeds.js/Headlines.js call).
2. `Feeds`: view, search, opensite, updatedebugger, subscribeToFeed, catchupAll.
3. `Article`: redirect, tags (print/set/complete), score, labels (assign/remove), enclosures, getmetadatabyid.
4. `Counters`: get_all/get_conditional.
5. `Pref_Prefs`: the big one (1647 L) — profiles ✅ (add/clone/remove/activate/save + getProfiles; pure DB + session, no plugin overlap), app passwords ✅ (generateAppPassword/deleteAppPasswords/appPasswordList — a `make_password`/`get_salt` helper + SSHA-512 hash in the `ALGO:HASH:SALT` form the API auth already verifies; list is a jinja template, oracle-structurally-matched), getPluginsList/getAvailablePlugins ✅ (listing only), previewDigest ✅, prefs save/reset ✅ (`saveconfig` + `resetconfig`), pref UI `index` page ✅ (the `AccordionContainer` shell + the full `index_prefs` list — all six sections, every control type, data sources oracle-matched; `index_plugins` form shell inlined with the list still lazy), pref UI `index_auth` pane ✅ (the `TabContainer`: personal data / password / app passwords / OTP-2FA with the QR + base32 secret), its write handlers ✅ (`changePersonalData`/`changepassword`/`otpenable`/`otpdisable` — the auth + OTP mutations) + `customizeCSS` ✅; remaining: the `index_plugins` list content (M7) and plugin install/uninstall/update (M7).
6. `Pref_Feeds`: feed tree, edit/save (single + batch), categories (add/rename/remove/sort), icons (upload/remove), OPML import, inactive/error feeds, access keys (clear/regen/shared URL).
7. `Pref_Filters`: tree ✅, edit/clone/add/remove/save ✅ (write CRUD), ordering ✅, join ✅, optimize ✅, edit-dialog prefill (`edit`) ✅, rule/action names (`printRuleName`/`printActionName`) ✅, feed multi-select (`editrule`/`_feed_multi_select`) ✅, `index` dojo page ✅ (jinja), `testFilterDo` ✅ (the "test filter" preview — the `RSSUtils::eval_article_filters` engine ported to `ttrss::core::filters`, previously DEFERRED to M6). **Complete.**
8. `Pref_Labels`: tree, edit, colors, save, add/remove.
9. `Pref_Users`: list, add, edit, details, remove, resetPass (admin).
10. `Pref_System`: log viewer, clear log, send test email, scheduled tasks list.
11. Plugin handler hooks (where relevant to UI).

- **Exit criteria**: every JS `xhr` call in `js/` answered with structurally identical JSON (golden diff < 0); Selenium smoke (existing `selenium_test.py`) passes against Rust server.
- **M3 status (2026-09-07):** item 1's mutation arms (`mark`/`publ`/`delete`/`catchupSelected`/`publishSelected`/`catchupFeed`), item 2's `catchupAll` (full `Feeds::_catchup` port), and item 3's `Article::redirect`/tags (print/set/complete)/score/labels (assign/remove)/`getmetadatabyid` are ported and diffed 21/21 by the `db_state`-capturing `scenarios-m3.toml` corpus (parity: row 15). Item 3's `enclosures` read path is row 10; `note` is M7 (plugin territory). Item 2's `view`/`search` remain M2; `subscribeToFeed` + `opensite` are ported (M4, see the status bullets below).
- **M4 status (2026-09-16):** the `Pref_Prefs` **profiles** slice is ported (`addprofile`/`cloneprofile`/`remprofiles`/`activateprofile`/`saveprofile`, with `getProfiles` already done) — verified by Rust-only DB tests in `tests/pref_profiles.rs` (Rust-only because the frozen PHP suite has no `Pref_Prefs` configuration tests). All five are pure DB + session work and have **no plugin overlap**: `cloneprofile`'s copy uses the static `Prefs::_PROFILE_BLACKLIST` (exposed as `prefs::PROFILE_BLACKLIST`), applied only when cloning from the default profile (`old_profile = 0`); from a named profile, all prefs copy verbatim. `remprofiles` protects the active profile and relies on the `ttrss_user_prefs2.profile → ttrss_settings_profiles.id` FK cascade to drop overrides; `activateprofile` sets the session `profile` key (read by `Session::profile` / the profile-aware `prefs` resolution). A shared `util::ctype_digit` (PHP `ctype_digit`: non-empty, all ASCII digits) backs `cloneprofile`'s input guard.
- **M4 status (2026-09-16, app passwords):** the `Pref_Prefs` **app-passwords** slice is ported — `generateAppPassword` (a random 16-char distinct-char password via `util::make_password`, hashed SSHA-512 with a fresh 250-hex-char salt from `ttrss::core::user::get_salt`, stored as `ALGO:HASH:SALT` in the `service='_api'` row the API auth already verifies), `deleteAppPasswords`, and the shared `appPasswordList` renderer (a jinja template, `templates/app_password_list.html.jinja`, oracle-structurally-matched against the PHP via `oracle/recorders/app_password_list.php` — whitespace-agnostic per the frontend-HTML bar). Verified by Rust-only DB tests in `tests/app_passwords.rs` (6 cases: generate/hash round-trip, distinct-char alphabet, delete incl. foreign-noop + empty-ids, title html-escaping, `NULL` `last_used` → "Never").
- **M4 status (2026-09-16, prefs save/reset):** the `Pref_Prefs` **prefs save + reset** slice is ported — `saveconfig` (the prefs form's `method=saveconfig` submit: persists every valid pref in the posted form via the existing `prefs::set`, applying the four per-pref quirks — `boolean_prefs` absent→"false" defaulting, `DIGEST_PREFERRED_TIME` change → reset `ttrss_users.last_digest_sent`, `USER_CSS_THEME`/`USER_LANGUAGE` change → the `PREFS_NEED_RELOAD` flag, and `BLACKLISTED_TAGS` → `util::normalize_categories`) and `resetconfig` (`prefs::reset`, keeping `_PREFS_MIGRATED`). Both return the exact string the SPA checks (`PREFS_NEED_RELOAD` vs "The configuration was saved."). Verified by Rust-only DB tests in `tests/pref_saveconfig.rs` (8 cases: sets bool/str prefs, boolean defaulting + the set-no-op when a boolean equals its default, invalid-pref ignored, blacklisted-tag normalization, theme reload on/off, digest-time reset, and resetconfig discarding overrides). Added `DIGEST_PREFERRED_TIME`/`USER_CSS_THEME`/`USER_LANGUAGE`/`BLACKLISTED_TAGS` constants to `server::prefs`.
- **M4 status (2026-09-16, pref UI `index` page):** the `Pref_Prefs` **`index`** tab is ported — a new `server::pref_ui` module + `templates/prefs_index.html.jinja` (jinja, per the large-markup rule). It renders the `AccordionContainer` (Personal data / Authentication [lazy `index_auth`], Preferences [inlined], Plugins [form shell inlined, list still lazy]) and the full `index_prefs` list: all six sections walked from `pref_item_map`, profile-blacklisted prefs hidden under a named profile, `<hr/>` separators omitted there, and every control type (`select_hash`/`select_tag` for translations/timezones/themes/intervals/ts-languages, the `SimpleTextarea` for `BLACKLISTED_TAGS`, `NumberSpinner`/`input` for the int/text age+format prefs, `ValidationTextBox` for the rest, plus the `USER_CSS_THEME`/`DIGEST_ENABLE`/`SSL_CERT_SERIAL` button clusters). Data sources are oracle-matched: the `USER_TIMEZONE` list is now a verbatim snapshot of PHP `DateTimeZone::listIdentifiers(ALL_WITH_BC)` (599 ids incl. `Factory`, exact order — `chrono_tz::TZ_VARIANTS` differed) in `server::pref_timezones`; translations + themes + intervals + ts-languages verified against the frozen PHP. New `controls::icon`/`input_tag`/`number_spinner_tag` helpers + `prefs::SSL_CERT_SERIAL`. Verified by Rust-only DB tests in `tests/pref_index.rs` (structure: panes/sections/labels/control-types/plugins-pane; profile: notice + blacklisted-hiding + no separators). Omissions (documented): the local-themes dir glob, the SSL cert-serial lookup (Register disabled), and the lazy `index_auth`/plugin-list content (M7). Dispatch: `"index" => DispatchOutput::Html(pref_ui::index(...))` (SPA `innerHTML` injection, like `editfeeds`).
- **M4 status (2026-09-16, pref UI `index_auth` pane):** the `Pref_Prefs` **`index_auth`** pane (the lazy Personal/Authentication tab) is ported — a Dojo `TabContainer` with four panes, built in `server::pref_ui` as Rust strings (like `plugins_pane`, given the branchy auth-module + OTP conditionals): **Personal data** (`changePersonalData`: `full_name` + `email` from the `ttrss_users` row), **Password** (`changepassword`: the `IAuthModule2` form — `auth_internal` implements it), **App passwords** (the `appPasswordList` holder + generate/remove buttons), and **Authenticator (OTP)** (enabled → disable form; disabled → QR + formatted base32 secret + `otpenable` form; non-internal → notice). New `server::otp` module ports `UserHelper::is_otp_enabled` / `get_otp_secret` (the `schema_version ≥ 143` path: lazily generates + persists a 10-byte hex secret, `sha1(salt)`-derived when enabled) and `Pref_Prefs::_get_otp_qrcode_img` / `format_otp_secret` — base32 via the maintained `base32` crate (`Alphabet::Rfc4648 { padding: false }` == PHP `Base32::encodeUpperUnpadded`, RFC 4648 vectors verified), the `otpauth://totp/…` URL via `urlencoding` (PHP `urlencode`), and the QR PNG data URI via `qrcode` + `png` (a functionally-equivalent scannable PNG, not byte-identical to chillerlan). New `controls::format_notice` / `format_warning` (mirror `controls_compat.php`). Verified by Rust-only DB tests in `tests/pref_index_auth.rs` (structure: TabContainer + 4 panes + every field/button/label; OTP enabled vs disabled branch; the generated 20-hex-char secret is persisted and its base32 form is in the pane) + 3 `otp.rs` unit tests. New deps: `qrcode`, `image`, `png`, `base32`, `urlencoding`. Dispatch: `"index_auth" => DispatchOutput::Html(pref_ui::index_auth(...))` (SPA `innerHTML` injection).
- **M4 status (2026-09-16, `index_auth` write handlers + `customizeCSS` + `FORBID_PASSWORD_CHANGES`):** the four form submits the `index_auth` panes post are now ported — `changePersonalData` (update `full_name` + `email`), `changepassword` (the `Config::FORBID_PASSWORD_CHANGES` guard → "Access forbidden."; the 5 validation checks — clean-altered / same-as-old / blank-old / blank-new / confirm-mismatch — then `Auth_Internal::change_password`, refreshing the session `pwd_hash` so `validate_session` stays valid), `otpenable` (`check_password` + `UserHelper::enable_otp`: the TOTP code must match the current secret), `otpdisable` (`check_password` + `disable_otp`, clearing the secret on schema ≥ 143). `auth.rs` gains `check_password` (verify with the stored algorithm + upgrade an out-of-date hash to SSHA-512) and `change_password` (fresh salt + SSHA-512 hash); `otp.rs` refactors the secret core into a shared `otp_state` and adds `enable_otp`/`disable_otp`; `controls::format_error` (the `alert-danger` variant) joins `format_notice`/`format_warning`; `Config` gains `forbid_password_changes` (`TTRSS_FORBID_PASSWORD_CHANGES`, T_BOOL, default false). Plus the `customizeCSS` read (the active-profile `USER_STYLESHEET` pref, `<br/>` → `\n`, as `{"value": …}` JSON) — new `prefs::USER_STYLESHEET` const. Omitted (NOTEd): the password-change / e-mail-change / OTP-change e-mail notifications — PHP sends them via the system MTA / `mail()`, which the Rust port does not replicate. Verified by Rust-only DB tests in `tests/auth_writes.rs` (8 cases: personal-data save, password-change success + 5 validation errors + wrong-old + forbidden-guard, OTP enable wrong-pw/wrong-code/success, OTP disable wrong-pw + clear-on-disable, customizeCSS `<br/>` conversion). Additive to the oracle corpus (none of the methods is in it); 738 server tests pass, clippy clean.
- **M4 status (2026-09-16, `Feeds::subscribeToFeed`):** the "Add a feed" dialog data is ported — `cat_select` (the user's feed categories via the existing oracle-matched `select_feeds_cats("cat")`) + `intervals.update` (the default-annotated `$update_intervals` from backend.php, a JSON object — PHP's non-sequential integer keys `0,-1,15,…` serialize as an object, and the frontend's `select_hash` re-sorts the integer-like keys numerically, so the serde `Map` key order is functionally irrelevant). The `$update_intervals` map is factored into a shared `feeds::update_intervals_with_default` (the `0`/"Default interval" label gets `(…DEFAULT_UPDATE_INTERVAL…)` appended via the existing `update_interval_label`), now also used by `editfeed` (refactored, behavior-preserving — `feed_edit` still passes). CSRF-gated (only `Feeds::index` is `csrf_ignore`). Verified by `tests/feed_edit.rs::test_subscribe_to_feed` (the `<select name="cat">` + Technology/Science & Nature/Uncategorized options; `intervals.update` is an object with the default-annotated "Default interval (30 minutes)" + the 8 standard labels). Additive to the oracle corpus; 775 server+core tests pass, clippy clean.
- **M4 status (2026-09-16, `UrlHelper::validate` + `Feeds::opensite`):** a new `server::urlhelper` module ports `UrlHelper::validate` and its collaborators (a lenient PHP-`parse_url` → `ParsedUrl`, `build_url`, a `filter_var(FILTER_VALIDATE_URL)` stand-in, `idn_to_ascii` via the `idna` crate's `domain_to_ascii` [UTS46 non-transitional == `IDNA_NONTRANSITIONAL_TO_ASCII`+`INTL_IDNA_VARIANT_UTS46`], and the SSRF guard `has_disallowed_ip` + `canonicalize_ipv4_literal` [dotted octal/hex/decimal IPv4 → dotted-quad] with the exact `FILTER_FLAG_NO_RES_RANGE`/`NO_PRIV_RANGE` ranges). Recorded from the real `ghcr.io/tt-rss/tt-rss` image (which has `intl`, unlike the `php:8.5-cli` recorder image) via `oracle/recorders/url_validate.php` into a **60-case** differential fixture (`tests/fixtures/url_validate.json`): protocol-relative fix, IDNA→punycode, IPv4/IPv6 + octal/hex literals, port bounds, the extended SSRF branch (standard-ports-only + loopback/link-local/reserved reject; private allowed on 80/443), path-encoding for the filter-only URL, `clean()`/strip_tags, and `localhost`. `Feeds::opensite` (the "Open site" menu item, a POST the frontend submits with `csrf_token`) is ported: 302 `Location` to `UrlHelper::validate(site_url, false)`, else 404 "Feed not found or has an empty site URL." — returned as `DispatchOutput::Raw` (like `Article::redirect`), CSRF-gated (only `Feeds::index` is `csrf_ignore`). New dep: `idna`. Verified by `tests/url_validate.rs` (all 60 cases) + `tests/feed_edit.rs::test_opensite` (302 to the fixture feed's site URL; 404 for missing / other-user / empty / invalid site URL). Additive to the oracle corpus. `Feeds::add` (the network fetch + RSS/Atom parse + DB insert) is **moved to M6** (the engine) — it needs the full `FeedParser`/`UrlHelper::fetch` subsystem. 777 server+core tests pass, clippy clean.
- **M4 status (2026-09-16, `RPC` config verbs — **M4 complete**):** the four remaining small `RPC` config verbs the SPA calls are ported (in `server::settings`, dispatched in `rpc.rs`): `setpref` (the generic pref setter — `prefs::set` now takes a `strip_tags` flag matching the PHP signature; off only for `USER_STYLESHEET`), `togglepref` (flips a bool pref via `!resolve(…)` and echoes the resolved typed value), `setWidescreen` (`WIDESCREEN_MODE`), and `log` (the client-JS error reporter → `tracing::error!` under a `client-js` target + `{"message":"HOST_ERROR_LOGGED"}`). New `prefs::Pref::to_json` + `WIDESCREEN_MODE` const. Verified by `tests/rpc_config_verbs.rs` (7 cases). With these the M4 config surface is **complete**; the last feed-adjacent item, `Feeds::add` (subscribe), is **moved to M6** (the engine) — its return codes 0–8 depend on the M6 `UrlHelper::fetch`/`FeedParser`. 784 server+core tests pass, clippy clean.

### Phase 5 — External API + syndication (→ M2 reads, M5 writes)

- `api/index.php` port: JSON envelope `{seq,status,content}`, method routing, `login`/`logout`/`isLoggedIn`/`getConfig` (incl. `daemon_is_running` lockfile check), all ~20 API methods, `Sec-Fetch-Mode` browser opt-in gating, per-user `ENABLE_API_ACCESS`.
- Syndication: `public.php?op=rss` Atom (Template port → minijinja of `generated_feed.txt`) + JSON feed; access-key auth; `noxml`; tag URI generation.
- Contract tests: port `tests/integration/ApiTest.php` cases; optionally test against published API docs (level 23).
- **Exit criteria**: ttrss-reader (or equivalent client) fully functional against Rust; syndicated Atom feed diff vs PHP (modulo timestamps) is clean.
- **M5 status (2026-09-16, external-API meta + DB-only write ops):** six `api/index.php` methods are ported (in `server::api`, dispatched in `before_and_dispatch`, now `pub` for the tests): `getVersion` (`{"version": …}` — a new shared `ttrss::core::config::VERSION` const, now also used by syndication), `getApiLevel` (`{"level": 23}`), `catchupFeed` (reuses the M3 `feeds::catchup_scope`, normalizing `mode` to `all` for non-`all`/`1day`/`1week`/`2week`), `unsubscribeFeed` (an owned-feed existence check + the M4 `feeds::remove_feed`, else `E_OPERATION_FAILED`), `updateArticle` (a batch `UPDATE ttrss_user_entries` over `marked`/`published`/`unread`/`note`/`score` — the value is a controlled SQL literal or param matching the PHP's string-interpolated `SET <field> = <set_to>`; the falsy-`set_to` cases — a non-0/1/2 mode on a bool field, or `score = 0` — return `INCORRECT_USAGE`), and `setArticleLabel` (reuses `labels::find_caption`/`add_article`/`remove_article` over `feed_to_label_id`). `feeds::catchup_scope`/`remove_feed` are now `pub`. Verified by Rust-only `tests/api_writes.rs` (15 cases: the meta methods + the `ENABLE_API_ACCESS` gate → `API_DISABLED`; `catchupFeed`; `updateArticle` marked/unread/toggle/note/score + the `INCORRECT_USAGE` arms; `unsubscribeFeed` ok/missing; `setArticleLabel` assign/remove/missing-label). Deferred (documented): `updateFeed`/`subscribeToFeed` (M6 — need the fetch/parse engine), `shareToPublished` (the ~340-line public-share renderer — its own increment), and `getFeedIcon` (a raw-bytes response, not the JSON envelope — do with feed-icon serving). Additive to the oracle corpus. 831 workspace tests pass, clippy clean.
- **M5 status (2026-09-16, `Pref_System` admin slice):** the `Pref_System` admin handler's remaining methods are ported in a new `server::system` module (dispatch in `rpc.rs`; `index` added to `csrf_ignored`). `index` renders the Dojo `AccordionContainer` shell (built as Rust strings, like `pref_ui`, given the branchy panes): **Event log** — `_log_viewer` (the `ttrss_error_log` paged at 15/page with a severity `<select>`: the PHP bitwise-errno sets for Errors(256)/Warnings(512)/Everything(1024) via a `log_filter_errnos` helper + a Clear-log button; a `"no errors"` row when empty), **Email configuration** (the `sendTestEmail` form: `mail_address` prefilled + the on-submit JS), and two lazy panes — **Scheduled tasks** (`getscheduledtasks`, ported earlier) and **Server Information** (`getphpinfo`). Two deliberate no-ops bridge the PHP-only surface: `sendTestEmail` returns `{rc:0, error: …}` (the system MTA / `mail()` is not wired up) and `getphpinfo` renders a small Rust/server diagnostics table (version, build target, OS/arch, key config, DB — omitting the password + encryption key) in place of `phpinfo()`. `clearLog` deletes all `ttrss_error_log` rows. Verified by Rust-only `tests/pref_system.rs` (5 cases: accordion structure, `_log_viewer` severity filter [Errors→1, Warnings/Everything→2], `clearLog`, the `sendTestEmail` no-op, the `getphpinfo` diagnostics). Additive to the oracle corpus. 853 workspace tests pass, clippy clean.
- **M5 status (2026-09-16, batch-add-feeds write surface + case-insensitive dispatch):** the `Pref_Feeds` **batch-add** write surface is ported in `feeds.rs`: `batchSubscribe` (the batch-subscribe dialog data — `enable_cats` as an int, `1`/`0`, per the PHP `(int)` cast, + `cat_select` via the existing oracle-matched `select_feeds_cats`) and `batchAddFeeds` (each line of the posted `feeds` list is `trim`-med and `UrlHelper::validate`-d, then — in a per-feed transaction — skipped if the user already has that `feed_url` else inserted as a `[Unknown]` feed with the posted `cat`/`login`/`pass`, `update_method = 0`, `auth_pass_encrypted = false`; read-only users are a no-op). It is a pure DB insert: the new feeds are picked up by the daemon on its next update, so it does **not** need the M6 `Feeds::add` fetch/parse engine. Alongside it, a **root-cause fix to the dispatcher** (`rpc.rs`): PHP `backend.php` does `$method = strtolower($method)`, so method dispatch is case-insensitive — the port now lowercases the method at the entry point and matches lowercased arms (previously it matched the raw mixed-case method, which silently 404'd the frontend's lowercase `inactivefeeds`/`renamecat` invocations — a latent bug the camelCase oracle corpus never exercised). All `dispatch`/`rpc_handler` method arms are now lowercase; the `op` (handler) names stay case-sensitive (PHP `class_exists($op)` is). Verified by `tests/feed_crud.rs` (6 new cases: `batchAddFeeds` insert/skip-existing/invalid-skipped/read-only no-op, `batchSubscribe` int `enable_cats` + `cat_select`, and an end-to-end **case-insensitive dispatch** test through `rpc::handler` proving `inactivefeeds`/`inactiveFeeds`/`getFeedTree`/`batchaddfeeds`/`batchsubscribe`/camelCase `markSelected` all route, not `E_UNKNOWN_METHOD`). Additive to the oracle corpus. 859 workspace tests pass, clippy clean.
- **M5 status (2026-09-17, password recovery + API `logout`):** the public **password-recovery** flow is ported in a new `server::forgotpass` module + `templates/forgotpass.html.tpl` (a minimal Dojo page with a `__CONTENT__` slot), faithful to `Handler_Public::forgotpass`. A 403 gate fires first (`disable_login_form` or a missing `auth_internal` plugin), then one of three branches: (a) the **request form** (first GET) — login + email + an arithmetic CAPTCHA (`v1 + v2`, operands stored in the session) + a CSRF token; (b) the **`method=do` POST** — validates the CSRF token (constant-time) + the CAPTCHA answer + the fields, looks the user up by `LOWER(login) + email`, then stores a fresh `resetpass_token = "time():sha1(…)"` (the "email" step is a **no-op** — the system MTA / `mail()` is not wired up, consistent with `Pref_System::sendTestEmail`; the reset link is logged, not sent); (c) the **reset link** (`?op=forgotpass&hash=…&login=…`) — validates the 15-hour `time():token` window, clears the token, and resets the password (the new password is shown in the "Completed." notice via the existing `users::reset_password`). The handler mints a public session id if none is present (so the CAPTCHA/CSRF persist across GET/POST) and sets the `ttrss_sid` cookie. Alongside it, the external API's **`logout`** method (`API::logout`) is ported: `sess.destroy()` + `{"status":"OK"}`, exempted from the not-logged-in gate and the per-user API gate in `api_auth` (`UserHelper::logout` always succeeds). Verified by Rust-only `tests/forgotpass.rs` (3 cases: the full form→POST→reset-link flow [token stored then cleared; password reset], the unknown-user "not found" arm, and the 403 gate) and `tests/api_writes.rs` (2 cases: `logout` clears the session's `uid`; `logout` callable when not logged in). Additive to the oracle corpus. 867 workspace tests pass, clippy clean.

### Phase 6 — CLI, daemon, mail, scheduler (→ M5 mail, M6 daemon/scheduler, M8 CLI parity)

- `clap` port of `update.php` (all subcommands incl. `opml-export/import`, `user-*`, `gen-search-idx` (Postgres FTS backfill via `html2text`), `gen-encryption-key`, `plugins-list`, `debug-feed`, `force-*`, `cleanup-tags`, `send-digests`, `update-schema`).
- Daemon: tokio worker pool replacing fork model (same lockfile names, spawn interval, feed batch limit, per-task stagger, child runtime cap via task timeout), single-process `--daemon` mode.
- Scheduler: `croner`-based, `ttrss_scheduled_tasks` persistence, all housekeeping tasks (orphan purge, cache expire, disable failed feeds, icon cleanup, login-limit log, error-log expire, lock-file expire, digests).
- Digest: `Digest::prepare_headlines_digest` (templates `digest_template*.txt`, `email_article_template.txt`) + `Mailer` (lettre; `LOG_SENT_MAIL` event logging).
- **Exit criteria**: ops parity — a Docker-style deployment (serve + daemon) runs unattended; digest emails match PHP templates; CLI golden outputs (user-list etc.).

### Phase 7 — Plugin system + first-party plugins (→ M7)

**Scope: all 18 first-party plugins** from the frozen reference (`tt-rss/plugins/`), phased bottom-up (M7.1–M7.21). Built on `docs/plugin-research.md` (2026-09-12); this section supersedes the old one-paragraph Phase 7.

| # | Plugin | One-line function (user view) |
|---|--------|-------------------------------|
| — | `auth_internal` | username/password login against the tt-rss DB (already the built-in login path) |
| 1 | `note` | per-article sticky notes (article-button + `PluginHandler`) |
| 2 | `nsfw` | tag-based content hiding (collapsed "NSFW" disclosure + image placeholder) |
| 3 | `no_iframes` | strip embedded iframes from content unless whitelisted |
| 4 | `shorten_expanded` | collapse overly long articles in expanded/CDM view |
| 5 | `af_zz_vidmute` | mute HTML5 videos in article content (JS) |
| 6 | `af_comics` | repair comic-strip feed entries at fetch time |
| 7 | `auto_assign_labels` | auto-assign labels from title/content/tags rules at fetch time |
| 8 | `af_youtube_embed` | YouTube feed entries render as embedded players; iframe whitelist |
| 9 | `cache_starred_images` | auto-cache media of starred articles to disk (scheduled task) |
| 10 | `share` | share an article by unique no-account URL (keys revocable in prefs) |
| 11 | `bookmarklets` | drag-to-bar bookmarklets: subscribe to feed / share this page |
| 12 | `af_psql_trgm` | mark similar articles read (threshold pref) + "Related articles" dialog |
| 13 | `toggle_sidebar` | main-toolbar button to toggle the sidebar |
| 14 | `hotkeys_swap_jk` | swap j/k navigation keys (vim orientation) |
| 15 | `hotkeys_noscroll` | n/p (↑/↓) move between articles without scrolling |
| 16 | `hotkeys_force_top` | open expanded articles scrolled to the top |
| 17 | `auth_remote` | login via proxy HTTP Basic auth or client SSL certificate |

**Out of scope:** `nginx_xaccel` (appears in the demo's `TTRSS_PLUGINS` env but is not in the frozen reference — the reference is the spec); community/third-party plugins (locked: unsupported); the git-based installer (deferred — listing only, and the listing landed in M4).

**Locked decisions:**

1. **Static loading** — plugins compile into the server; explicit registration list; no dynamic-loading machinery (research §3/§6).
2. **Contract pinned 1:1 with PHP** — `Plugin` + `PluginHost` traits, explicit `HostCtx` in place of PHP's globals (research §4.1–4.2).
3. **Lifecycle** — `init()` runs once at startup (register-only); per-request gating: `TTRSS_PLUGINS` (system) + `_ENABLED_PLUGINS` (user, profile-scoped) (research §4.3).
4. **State** — one long-lived instance per plugin: stateless across requests, `Send + Sync`, `RwLock` interior mutability in registries (research §4.4/4.5).
5. **Storage interop read+write** — `ttrss_plugin_storage.content` stays PHP `serialize()` bytes; the codec **already exists** (`core/phpser.rs`, both directions, built for the session blob) — M7.2 applies it, does not build it (supersedes research §7.2).
6. **i18n identity** — `__()` stays identity; plugin strings ship in English (research §4.9); API versioning mirrors PHP's `API_VERSION` (§7.6).
7. **`auth_remote` in scope** — 2026-09-19 decision ("all first-party plugins are worth porting"); **overrides research §7.3's deferral**; the `IAuthModule` login refactor lands with it (M7.20).
8. **The WASM route is preserved — explicitly not in jeopardy.** Static loading is the *current* loader (research §6: "start static"), not a rejection of dynamic loading. The contract/loader split is the point: the traits + `HostCtx` are loader-agnostic, and the only loader-specific code is the registration list (one `Vec<fn() -> Box<dyn Plugin>>`). A later switch to WASM (research §3 mechanism #1, the Zed/extism model) changes *that list* — instantiate a module, wrap it as `dyn Plugin` — and touches no plugin, no hook call site, no storage, no enablement. Two constraints keep the swap trivial: keep the trait's data surface PHP-shaped (scalars/strings/arrays — what a WASM ABI maps cleanly), and keep all loader knowledge in the registration module. The installer scope decision (research §7.1) reopens with the loader.

**What already exists:**

- `core/phpser.rs` — full PHP `serialize()` codec, both directions (session blob + feed `auth_pass` already use it).
- `src/plugins.rs` — the M4 listing (`getPluginsList`/`getAvailablePlugins`) with a static `PLUGIN_META` table; `system_enabled` from `TTRSS_PLUGINS`, `user_enabled` from `_ENABLED_PLUGINS` — M7.1 reuses its resolution so the listing and the loader can't drift.
- `Config::plugins` — the `TTRSS_PLUGINS` env (default `auth_internal`).
- `templates/shell.html.tpl` — **statically embeds the frontend JS of `af_psql_trgm`, `note`, and `share`** (ported verbatim from the PHP page); M7.21 makes plugin-asset injection data-driven.
- The ~40 `HOOK_*` call sites are all no-ops today — the correct "no plugins" baseline (research §4.11); call sites land **with the plugin that needs them**, never before.

**No *per-plugin* PHP test suites exist** (`tt-rss/plugins/` carries no test files; only `ConfigTest` touches plugins indirectly). NOTE (2026-09-19, M7.2): the *host* does have one — `tt-rss/tests_integration/PluginHostTest.php` — whose cases M7.1 already ported 1:1 (the M7.1 suite mirrors its hook/registry/feed/API/filter-action/command cases) and M7.2 ports the storage section; this corrects the original "no PHP plugin test suites" claim. Each increment otherwise verifies by **Rust-only DB tests** (write-op rule) plus **handler-level rendering/behavior tests** where a read-side surface is touched. (The oracle corpus is retired 2026-09-19 and not part of the bar; the M7.1 no-op baseline was corpus-verified before retirement.)

**Increments** (bottom-up; each = one verifiable step, one commit):

#### Phase 1 — Substrate (no user-visible change; no-op baseline preserved)

- **M7.1 — Plugin contract + registry + lifecycle.** `crates/ttrss/src/pluginhost/`: `trait Plugin` (`name`/`about`/`init` + the ~50 hook methods with PHP-matching signatures, default no-op; `get_js/css/prefs_js/prefs_css`, `flags`, `is_public_method`, `csrf_ignore`, `api_version`); concrete `PluginHost` (registries: plugins, hooks by type+priority, handlers, commands, filter actions, API methods, virtual feeds, scheduled tasks; runners in PHP's four modes — `run_hooks`, `run_hooks_until`, `run_hooks_callback`/`chain_hooks_callback` with short-circuit on `false`/`null`); `HostCtx<'a>` (`&PgPool`, session uid/profile/access_level, request params, active profile); static registration list; `init()` once at startup; per-request host view filtered by `TTRSS_PLUGINS` + `_ENABLED_PLUGINS`. **No call sites wired** → behavior identical to today. *Verify:* registry mechanics (priority, all four run modes incl. short-circuit), gating (system/user/profile-blacklist); no-op baseline held (full Rust suite green; corpus-invariance verified at landing, before the corpus was retired). **✅ 2026-09-19**
- **M7.2 — Plugin storage service.** `ttrss_plugin_storage` (name, owner_uid, content) on `core::phpser`: `set/get/set_array/get_array/get_all/clear_data` + `profile_get/profile_set`, always `HostCtx`-scoped. *Verify:* DB round-trips, (uid, name) keying, profile scoping, `clear_data`; **interop: byte-exact decode of PHP-written fixture rows + encode output PHP `unserialize()` accepts** (the "storage portable" exit criterion is established here — verified against the installed PHP 8.5.10, which surfaced two `phpser` deviations fixed in the same batch: `i:` integer-key form and the `zend_gcvt` float form). **✅ 2026-09-19**
- **M7.3 — `PluginHandler` backend dispatch.** `backend.php?op=PluginHandler&plugin=<n>&method=<m>[&…]`: resolve the active plugin, CSRF check (`validate_csrf` unless `csrf_ignore`), dispatch, the exact JSON error envelopes the SPA handles. *Verify:* dispatch / unknown plugin / unknown method / CSRF fail / `csrf_ignore` bypass / `is_public_method` gating. Ported as `pluginhost::dispatch` (`pluginhandler` for the protected path, `public_dispatch` for the no-account `public.php?op=pluginhandler` endpoint, which the same `--`-delimiter shortcut rewrites into); the `Plugin` trait gained `has_public_method` (the `method_exists` probe, checked before the CSRF gate — precedence pinned by `test_protected_unknown_method_beats_csrf`). The public path runs with `KIND_ALL`/owner 0 (PHP's `load($name, KIND_ALL, 0)`) and gates on `is_public_method`. The port's `$_REQUEST`-equivalent is a request map built from the named query/form fields + a `#[serde(flatten)]` catch-all (plugin methods see every param, as PHP's do); the protected path's map carries `op`/`method`/`csrf_token`/`plugin` exactly as the RPC layer does today. **✅ 2026-09-19** — 11-case suite (`tests/plugin_dispatch.rs`) on a configurable test-double plugin + the `is_protected`/`csrf_ignored` gate unit tests (incl. the case-sensitivity of PHP's `in_array`).

#### Phase 2 — Article display (closes the M3 `notes` residual)

- **M7.4 — `note`.** Article notes in the **`ttrss_user_entries.note` column** (keyed `ref_id` + `owner_uid` — *not* plugin storage; the port has no core note handling, this is the M3 residual it closes). `hook_article_button` (the button row in article lists — the only hook `init` registers); `PluginHandler` methods **`edit`** (get — `SELECT note … WHERE ref_id + owner_uid`, renders the textarea only when a row exists, the Save/Cancel footer always renders) and **`setNote`** (set — `UPDATE`, echoes `json_encode([id, note])`; "clearing" a note is just `setNote` with an empty string — there is no separate clear method). The click wiring is client-side (`onclick="Plugins.Note.edit(id)"` in `note.js`, already embedded in the shell) — **no `HOOK_HEADLINES_RENDERED` hook**. Landed as `src/plugins/note.rs` ✅ (2026-07-05).
  - **Done:** `Note` plugin (the `ttrss_user_entries.note` column, keyed `ref_id` + `owner_uid`); `edit` (the textarea only when the article has a `ttrss_user_entries` row — *row presence, not note value*: a row with a NULL note still renders the empty textarea, as PHP's `if ($row = fetch())` tests presence; the Save/Cancel footer always), `setNote` (`UPDATE` — no `INSERT`, so a note on an article with no row is a no-op, as in PHP; echoes `{id, note}` JSON), the button-row `hook_article_button` (the exact PHP literal — `Plugins.Note.edit(id)` onclick).
  - **`invoke_method` now returns `Result<MethodOutput>`** (`Json(Value)` — wrapped by the dispatcher — or `Raw(String)`, emitted verbatim as the response body): `note`'s `edit` is the first `Raw` user; the dispatcher's `Raw` branch is exercised by `plugin_dispatch`'s `raw` test.
  - **Tests:** `tests/note_plugin.rs` — 11 cases: save+load round-trip, `clean()` tag-stripping on save, footer-only `edit` for a rowless article, the rowless `setNote` no-op, owner scoping (a self-contained owner-2 fixture; owner 1's rowless `setNote` is a no-op on owner 2's row), per-article scoping, the plugin-level CSRF gate, active-set gating (a registered-but-disabled plugin is `E_UNKNOWN_PLUGIN` — M7.3's `get_plugin` is now active-aware), the button-row markup, `method_exists` case-insensitivity, and `about()` metadata.
  - **Fidelity note (surface):** PHP's `edit` echoes the note verbatim (`<?= $note ?>` — no escaping); ported as-is per the spec (the note is owner-scoped; the unescaped echo is a reference flaw).
- **M7.5 — `nsfw`.** Per-profile NSFW-tags prefs tab (`HOOK_PREFS_TAB`), `hook_render_article[_cdm|_api]` wrapping matching content in `<details class='nsfw'><summary>Not safe for work (click to toggle)</summary>`, `hook_article_image` placeholder, `get_css`. Closes the exit-criterion "NSFW filter". *Verify:* DB tests (tag matching, per-profile storage); handler-level rendering tests (wrapped article, placeholder image, prefs tab). ✅ (2026-07-05)
  - **Done:** `NsFw` (`src/plugins/nsfw.rs`): the stored tag list in plugin storage (owner-scoped via M7.2 — PHP's plain `host->get/set`, *not* `profile_*`: the prefs pane lives on the profile-scoped prefs page, but the storage calls themselves are plain), `save` (normalizes via `util::normalize_categories` — trim/lowercase/`t:`-prefix/sort/dedup — and stores `implode(", ")`), `rewrite_contents` (wraps `content` in `<details class='nsfw'>` for the three render hooks; the API hook's `headline ?? article` fallback returns the *article*, as in PHP), `hook_article_image` (`[self_url + "/plugins/nsfw/nsfw.png", "", "nsfw"]` placeholder), `hook_prefs_tab` (the `prefPrefs` pane → the new `HostCtx.output` buffer), `get_css`.
  - **`HostCtx` grew two fields:** `self_url` (PHP `Config::get_self_url()` for plugins; threaded in via `with_self_url` at the two dispatch choke points) and `output` (PHP's `ob_start()` buffer for printing hooks — `hook_prefs_tab` is the first user; M7.10's prefs page collects/clears it).
  - **Tests:** `tests/nsfw_plugin.rs` — 7 DB cases (save round-trip + normalization, render wrapping incl. the no-`tags` key form, the API `headline`/`article` fallback, the placeholder image, the prefs-tab pane, the CSRF gate, active-set gating) + 4 crate unit tests (the CSS literal, the method surface, the five `init` registrations through `hook_order`).
  - **Fidelity note (surface):** the prefs pane echoes the stored tags into the textarea *unescaped* in PHP (`<?= $tags ?>`); the port escapes (`escape_html`) — a textarea value needs quote escaping to be valid HTML, and the pane is the only rendering of that value. Also: the ported `hook_render_article_api` returns the rewritten *article* (not the row) exactly as the reference does — the M7.21 runner must treat that return value as the reference's does.
- **M7.6 — `no_iframes`.** `HOOK_SANITIZE` strips iframes unless whitelisted (interlocks with M7.11 — the whitelist hook runs first). *Verify:* sanitizer parity tests. ✅ (2026-07-11)
  - **Done:** `NoIframes` (`src/plugins/no_iframes.rs`): `init` registers `HOOK_SANITIZE`; the hook walks the sanitized fragment (string scan of the `&mut String` data surface — the port's `Sanitizer` is html5ever-based, so the walk is a tag scan, not a DOM XPath) and removes every `<iframe>` whose `src` host is not admitted by the `HOOK_IFRAME_WHITELISTED` chain (first-true wins, via `hook_order`; no host → not whitelisted, as PHP's `parse_url` empty-host path). The chain runs as a **direct loop**, not `run_hooks_until`: the generic runner's per-call HRTB closure cannot borrow the per-iframe host local (the M7.3 "captures must be owned" wall).
  - **The hook stage (single async entry):** the sync core is renamed `sanitizer::sanitize_core` (`pub(crate)` — the plugin-free CPU pipeline the parity/unit tests drive directly, **no runtime**). The public `sanitizer::sanitize` is now `async` and is the one read-side entry: it runs `sanitize_core` then the `HOOK_SANITIZE` stage (early-out on an empty `hook_order`, `mem::take` hand-off between hooks — PHP mutates the shared `DOMDocument`; composition is the same), so the hook stage **cannot be skipped** at a call site. It takes two extra args: `article_id: i32` and `plugins: Option<&mut HostCtx>` (a `None` site passes no ctx → core-only). All four read-side sites call it (once-built ctx per request): `api::get_headlines` (+ the `api_headline_row` param), `api::get_article`, `syndication::generate_feed` (owner-scoped ctx — feed output runs the **feed owner's** plugins, confirmed against `Handler_Public.php:52-55`), `headlines::format_headlines_list`. New dispatch helpers: `session_ctx` (now `pub(crate)`) and `owner_ctx` (no-session owners: default profile, access 0, the owner's `_ENABLED_PLUGINS`).
  - **`Sanitizer::iframe_whitelisted`** (the core's always-false stub with the `TODO(port)`) is retired in favor of `sanitizer::url_host` (the `parse_url` host helper, bracket-trimmed) feeding the plugin's chain.
  - **Tests:** `tests/no_iframes_plugin.rs` — 3 pipeline cases (with `has_sandbox = true`, where the core *keeps* iframes and removal is the plugin's job: active-system removes both the src'd and the src-less iframe; registered-but-inactive leaves them sandboxed; user-list activation removes). 4 unit tests (metadata, attribute extraction, the removal walk, the whitelist chain with a stub whitelisting plugin — the M7.11 interlock).
  - **Fidelity note (surface):** with `has_sandbox = false` the core strips iframes itself (`iframe` enters the allowed set only then) and the plugin is moot — the plugin's territory is the sandbox-capable path, matching the reference (the plugin exists to remove iframes for clients that can sandbox them).
- **M7.7 — `shorten_expanded`.** `get_js`/`get_css`-only (pure frontend; `init` registers no hooks) — collapse overly long articles in CDM/expanded. Ported as `src/plugins/shorten_expanded.rs`; `get_js`/`get_css` `include_str!` the reference `init.js`/`init.css` (the PHP methods are a `file_get_contents` of those files, so the reference stays the single source of truth — tab-indented bytes preserved). ✅ (2026-07-11)
  - **Done:** `ShortenExpanded` (name `shorten_expanded`, class `Shorten_Expanded`, about "Shorten overly long articles in CDM/expanded" / fox, api 2, empty `init`); registered in `all_plugins`. *Verify:* unit tests pin `get_js`/`get_css` byte-identical to the reference files + the `Plugins.Shorten_Expanded` / `.content-shrink-wrap` shape. The **shell-render test (JS present when enabled, absent when not)** is the M7.21 dynamic-asset-wiring bar, not M7.7's — see the note below.
- **M7.8 — `af_zz_vidmute`.** `get_js`-only (mute audio in HTML5 videos in article content; `init` registers no hooks — the `init.js` client registers `HOOK_ARTICLE_RENDERED`/`_CDM` to set `muted = true`). Ported as `src/plugins/af_zz_vidmute.rs`; `get_js` `include_str!`s the reference `init.js` (no `init.css` — `get_css` is the empty default). ✅ (2026-07-11)
  - **Done:** `AfZzVidMute` (name `af_zz_vidmute`, class `Af_Zz_VidMute`, about "Mute audio in HTML5 videos" / fox, api 2); registered in `all_plugins`. *Verify:* unit tests pin `get_js` byte-identical to the reference file, `get_css` empty, and the `HOOK_ARTICLE_RENDERED*` / `muted = true` shape. Same scoping as M7.7 — the shell-render gate is M7.21's.
  - **Asset-wiring scoping (M7.21 owns it):** the reference `index.php` concatenates each *loaded* plugin's `get_css`/`get_js` into the shell (`foreach get_plugins(): get_css()/get_js()`). The port's `shell.html.tpl` instead bakes the golden dataset's three plugin JS blocks (`af_psql_trgm`, `note`, `share`) statically, and `render_shell` only substitutes dynamic scalars. Making the shell emit the **active** plugins' assets (so "JS present when enabled, absent when not" holds for `shorten_expanded`/`af_zz_vidmute`) is M7.21's "replace the three statically embedded shell blocks" step. M7.7/M7.8 land the plugins + asset parity; M7.21 lands the wiring and the shell-render gate.

#### Phase 3 — Update pipeline (hooks into the `updates` engine — highest-risk cluster)

- **M7.9 — `af_comics`.** ✅ (1) the `HOOK_ARTICLE_FILTER` pipeline wiring (fire the chain on each new article in `upsert_article` + persist `plugin_data` on INSERT/UPDATE; the pipeline threads the loaded registry), and (2) the `af_comics` port (the 5-hook shell + the 12 `ComicFilter`s, extraction via `scraper`). *Verify:* the hook-firing test (a plugin rewriting `content` is reflected in the stored base entry + `plugin_data`) and the filter match-guard/dispatch tests; the per-filter fetch+extraction bodies are ported faithfully (reviewed, not harness-tested — no network).
- **M7.10 — `auto_assign_labels`.** ✅ `HOOK_ARTICLE_FILTER`: for each owner label, if its caption matches (whole-word, case-insensitive) the article's tags/content/title, the label is added to the article's label list (the PHP's `labels_contains_caption` dedup). The pipeline seeds `article["labels"]` with the article's existing labels (RSSUtils:836) and stores the post-hook list (existing + plugin) in the "labels [other]" re-apply (RSSUtils:975/1203). The reference has **no** prefs page — the "rules" *are* the user's labels (the plan's "prefs page for the rules" is a mischaracterization; the PHP is the spec). *Verify:* 3 unit tests (`labels_contains_caption`, the `\b…\b/i` whole-word match, metadata) + 1 pipeline test (matching caption assigned, non-matching not; label storage).

#### Phase 4 — Rendering & sanitization (closes the 54/55 + 59/60 residuals)

- **M7.11 — `af_youtube_embed`.** ✅ (1) `HOOK_RENDER_ENCLOSURE` → the embedded YouTube player (`hook_render_enclosure` renders `https://www.youtube-nocookie.com/embed/<vid>` with the `youtube-player` class, the loose `sandbox`, `allow`, and `frameborder='0'`; `""` otherwise) using the reference's `url_to_youtube_vid` (the 4 URL patterns), and (2) `HOOK_IFRAME_WHITELISTED` → the PHP's YouTube-host whitelist (`in_array($host, YOUTUBE_HOSTS)`). **Closes the M5a.3 residual.** *Verify:* the residual e2e (a YouTube iframe through the read-side `sanitize` keeps the **loose PHP sandbox value** `allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox`; a non-YouTube one gets the restrictive `allow-scripts` — `af_youtube_embed::tests`), the `hook_render_enclosure` markup test, and the PHP `iframe_whitelisted` ports (empty-src / no-hook → false, `sanitizer_parity/integration.rs`).
  - **The sandbox wiring (the load-bearing part):** the read-side `sanitize` now computes the `HOOK_IFRAME_WHITELISTED` *admitted-host set* before the core runs — for each iframe `src`, fire the chain with the host (`parse_url($src, PHP_URL_HOST)`, per the reference) and record the admitted hosts. The set (plain `String`s) is passed to the sync core's `process_iframes`, which gives an admitted iframe the loose PHP sandbox value and every other iframe the restrictive one. The srcs are collected by a sync helper (`collect_iframe_srcs`) so no `kuchikiki` `NodeRef` (an `Rc` tree, not `Send`) is held across the `await` that fires the hook — holding one broke `sanitize`'s `Send` and the axum `Handler` bound. Two variants, not three: `sanitize_core` (sync, takes the admitted set as an 8th `Option<&BTreeSet<String>>` arg) is the one core the parity/unit helpers drive directly with `None` (no runtime), and `sanitize` (async) computes the set, runs the core, then the `HOOK_SANITIZE` stage — `process_iframes` treats `None` and `Some(&empty)` identically (restrictive for all).

#### Phase 5 — Media & sharing

- **M7.12 — `cache_starred_images` ✅ (Phase 1–2).** **Scheduler-driven** (correcting the earlier draft, which guessed a `HOOK_ARTICLES_MARK_TOGGLED` queue-on-star — the reference uses no mark-toggled hook): the `@hourly` task scans for starred articles not yet cached (`plugin_data NOT LIKE '%starred_cache_images%'`), downloads their media into a `starred-images` `DiskCache` (no-auto-expiry marker, a `.status` attempt file capped at `max_cache_attempts=5`), and marks them done (`plugin_data = "starred_cache_images,<uid>:…"`); the `@daily` task unlinks cache files whose article is gone. **Phase 3** (the M6c.4 scheduler variant that actually invokes the tasks — system plugins always, user plugins only for active users) is **deferred** to the scheduler housekeeping. *Verify (done):* 3 DB tests (task run caches a starred article's media + marks it; the attempt cap stops at 5 and does **not** mark done; the expiry task unlinks files for a gone article).
- **M7.13 — `share`.** Per-article secret key in plugin storage (`newkey`/`unshare`/`clearArticleKeys`), the **no-account public route** serving one article by key, `PluginHandler` dialog/list/revoke (`is_public_method` + `api_version`), prefs tab section (frontend JS already in the shell). *Verify:* DB tests (key lifecycle, profile scoping); public-route tests (valid/expired/revoked).
- **M7.14 — `bookmarklets` ✅.** Prefs-tab UI exposing the two bookmarklets (subscribe-to-feed, share-this-page → `sharepopup`): the `prefFeeds` AccordionPane with the two `javascript:` URIs (the subscribe one confirms + redirects; the share one opens a popup with the selected text + page URL) + the "More info…" button. The `sharepopup` public method (no account required — the login form is rendered when unauthenticated) creates a published article via `article::create_published_article` + closes the window. The `subscribe` public method (the full `Feeds::_subscribe` engine: all result codes 0-5, the "Edit subscription options" link, the multiple-feed-URLs select form). `HostCtx` now carries `&Config` (the PHP `Config` singleton equivalent), un-deferring `subscribe` + enabling the future `auth_internal`/`auth_remote` plugins. *Verify (done):* 5 DB tests (prefs-tab rendering, the `sharepopup` form, the `sharepopup` share action, the login form when unauthenticated).
- **M7.15 — `af_psql_trgm` ✅.** Trigram-similarity filter: `save` (stores similarity/min_title_length/enable_globally), `showrelated` (the Related-articles dialog: `SIMILARITY(title, ?)` over 2 weeks, top 10 by similarity), `hook_article_filter` (the core filter: `pg_trgm` availability → `enable_globally`/`enabled_feeds` gating → similarity threshold → min-title-length → `MAX(SIMILARITY(title, ?))` over 1 day → `force_catchup = true`), `hook_prefs_tab` (the AccordionPane UI + `pg_trgm` availability check), `hook_prefs_edit_feed` (per-feed checkbox, checked if in `enabled_feeds`), `hook_prefs_save_feed` (reads `trgm_similarity_enabled` from form data, updates `enabled_feeds` in storage), `hook_article_button` (the `bookmark_outline` icon → `showRelated` dialog). `CREATE EXTENSION IF NOT EXISTS pg_trgm` added to the test fixtures. `feeds.rs` now calls `run_hooks(HOOK_PREFS_EDIT_FEED)` in `editfeed` (→ `plugin_data`) and `run_hooks(HOOK_PREFS_SAVE_FEED)` in `editsaveops` (passes the form data as `&Value`). The `Plugin` trait's `hook_prefs_save_feed` signature now includes `&Value params` (the form data, equivalent of PHP's `$_POST`) — required for WASM plugins (no host globals). All 10 PHP methods fully ported. *Verify (done):* 8 DB tests (save, article_button, prefs_tab, filter×3, showrelated×2).

#### Phase 6 — UI chrome & keyboard (all small)

- **M7.16 — `toggle_sidebar`.** `HOOK_MAIN_TOOLBAR_BUTTON`. ✅ the `chevron_left` button (the `Plugins.Toggle_Sidebar.toggle` onclick) + `get_js` (the `Feeds.toggle` + icon flip). 2 DB tests.
- **M7.17 — `hotkeys_swap_jk`** · **M7.18 — `hotkeys_noscroll`** (`HOOK_HOTKEY_MAP`) · **M7.19 — `hotkeys_force_top`** (`get_js`). One increment each. *Verify:* hotkey-map unit tests; shell-render test.

#### Phase 7 — Authentication

- **M7.20 — `auth_remote`.** The login path becomes pluggable: an `IAuthModule` trait behind the `HOOK_AUTH_USER` chain (`UserHelper` parity); `auth_remote` implements proxy HTTP Basic auth + client-SSL-certificate login; `auth_internal` re-implemented on the same trait (uniformity — behavior must stay byte-identical; the login suites stay green). *Verify:* login-path tests (internal unchanged; remote: basic-auth mapping, cert subject → user, failures).

#### Phase 8 — Wrap-up

- **M7.21 — Data-driven plugin assets + residual closure.** The served page concatenates **active** plugins' `get_js`/`get_css`/`get_prefs_js`/`get_prefs_css` (replacing the three statically embedded shell blocks; with the oracle's `TTRSS_PLUGINS` set the shell-render tests pass). Full Rust suite re-run; confirm closed residuals: M3 `notes`, M5a.3 sandbox value, parity-board `HOOK_*` rows 9–14, `HOOK_SEARCH` fallback; refresh the plan tables.

**Exit criteria (unchanged from the ladder):** the stock plugin set reproduces the PHP UI features it drives (NSFW filter, notes, OTP auth, trigram search/related); plugin storage data portable (PHP↔Rust, established M7.2, spot-checked M7.21); full Rust suite green with the stock plugin set registered (handler/DB tests cover the read-side surface the retired corpus used to).

**Decisions to revisit (M7):**

1. **`auth_internal` as a plugin vs staying built-in** — M7.20 re-implements it on `IAuthModule` for uniformity; the cheaper alternative keeps a permanent special case in the login path.
2. **`pg_trgm` availability** — enable globally on oracle + test DBs (simplest) or gate the M7.15 step; decide at M7.15 start.
3. **Phase 6 granularity** — the four chrome/keyboard plugins could collapse to one "UI chrome" increment; kept separate for the one-commit rhythm.
4. **Static shell JS (M7.21)** — the data-driven switch is the faithful behavior (PHP concatenates active plugins' assets) but touches the most-tested page; the alternative is leaving the three blocks static (a permanent deviation).

### Phase 8 — Hardening, parity verification, packaging (→ M9)

- Security pass: CSRF matrix, SSRF fuzzing, sanitizer fuzzing (cargo-fuzz), session crypto interop, timing-safe comparisons, cookie flags, IDN edge cases, `Sec-Fetch-Mode` checks.
- ~~Differential harness at scale~~ (**retired 2026-09-19**): the record/replay corpus is no longer part of the plan — its read-only scope and wall-clock fixture decay made the maintenance cost outweigh the signal; the Rust test suite (PHP test-parity) is the verification bar.
- Performance: counter-cache effectiveness, headline query parity (EXPLAIN), large-DB (10M articles) test; tune sqlx pool.
- Docker image(s) mirroring `supahgreg/tt-rss` + web-nginx pair; systemd units; docs (config reference, migration guide, plugin API guide).
- **Exit criteria**: drop-in replacement for an existing deployment (point both at same DB, switch nginx upstream).

### Deferred (low priority)

A parking lot for small, known, not-now items (not milestone blockers):

- **Dynamic timezone list.** The `USER_TIMEZONE` select currently uses a hardcoded snapshot of PHP `DateTimeZone::listIdentifiers(ALL_WITH_BC)` (599 ids, `server::pref_timezones`). If the tzdata ever changes, or we want to avoid the snapshot, regenerate/compute the list dynamically to match PHP exactly — including the `Factory` id and the exact PHP ordering (which `chrono_tz::TZ_VARIANTS` does not reproduce: it omits `Factory` and orders some zones differently).
- **`html_to_text` for `tsvector_combined` (shareToPublished).** The full-text column is populated with an approximated `html_to_text` (a kuchikiki traversal dropping `script`/`style`/`head` + collapsing whitespace), not PHP's full `Html2Text::convert`. Low priority to revisit: the column only feeds `to_tsvector` full-text search, so exact text parity isn't required. Revisit if full-text-search parity vs PHP matters.
- **`getFeedIcon` response headers simplified.** The port emits only `Content-Type` + `Content-Length`, skipping `DiskCache::send`'s conditional-request caching headers (304 / If-Modified-Since, ETag, Last-Modified, Content-Disposition, `CACHE_MAX_DAYS` expiry). Low priority to revisit: the corpus doesn't exercise `getFeedIcon` and these are caching optimizations, not behavior. Revisit if conditional-request / caching semantics for favicons are wanted.
- **`getFeedIcon` MIME detection + no content-type gate.** The content-type is detected from magic bytes (PNG/JPEG/GIF/WebP, defaulting to `image/png`) rather than `finfo`, and the port does not replicate PHP's SVG-blocklist / non-image 400 gate. Low priority to revisit: the feed-icons cache only holds favicons. Revisit if a hostile / non-image file could land in the cache.
- **`send_headlines_digests` scheduler task is a no-op (SMTP / `Mailer` not ported).** The M6c.4 cron scheduler runs the task and records it in `ttrss_scheduled_tasks`, but sends nothing: only the digest *body* is ported (`ttrss::digest::prepare_headlines_digest`); the actual send is the `Mailer` (`lettre`, replacing PHP `mail()`) that ships with the M6 digest milestone (the M6 scope "Digest" line, `LOG_SENT_MAIL` event logging). Revisit once the `Mailer` lands — until then, scheduled headline-digest emails won't be sent.

---

## 4. Risks & Mitigations

| Risk | Severity | Mitigation |
| --- | --- | --- |
| **Community PHP plugin ecosystem dies** (plugins can't run under Rust) | High (product) | Rust plugin API + port popular plugins; document break; *optionally* keep a PHP sidecar later for exotic plugins (out of scope v1) |
| JSON shape drift in `backend.php` (PHP loose types, null vs "", array vs scalar) | High (frontend breaks silently) | Golden-file diff harness from day P3; loose-type module with property tests; Selenium smoke each phase |
| Feed-parser long tail (weird encodings, broken XML, nonstandard namespaces) | Medium | Port fixture corpus from `tests_functional`; keep PHP parser as oracle in differential tests |
| Sanitizer divergence → XSS or broken layouts | High (security) | Fuzz + corpus diff vs PHP; conservative defaults; iframe whitelist parity |
| Search behavior drift (tsvector, languages, trgm) | Medium | Keep identical SQL; backfill tool port; compare ranked results on fixtures |
| Migration compatibility (shared DB with PHP) | Medium | Reuse exact `.sql` files; same version tables; interop tests both directions |
| PHP crypt hash formats (MODE2/SHA1X legacy) | Medium | Find/generate PHP reference hashes in P1; keep verification paths even if legacy users are rare |
| Dojo frontend quirks (e.g., specific error JSON fields, ETag semantics) | Medium | Treat frontend JS as spec; replay real traffic in CI |
| Scope creep in plugin installer / plugin updates (git tooling) | Medium | Defer to P7-late/P8; read-only listing first |

---

## 5. Test Strategy (cross-cutting)

1. **Unit** (Rust): each ported module, property tests for type coercion, crypto interop vectors.
2. **Fixture/golden**: PHP-generated DB fixtures (users with all hash formats, feeds, articles, prefs, plugin storage) + recorded request/response pairs (backend.php, api, rss, shells) → assert Rust responses are structurally identical (timestamp-normalized).
3. **Differential**: run PHP instance and Rust instance against the same Postgres; replay traffic + feed-update runs; diff.
4. **Integration**: Postgres testcontainer; `ApiTest` port; Selenium smoke from existing suite.
5. **Fuzz**: sanitizer, feed parser, URL validator.
6. **Ops**: lockfile/daemon behavior tests; migration up/down (v0→151, partial states).
7. **Browser/UX** (golden states): Playwright + headless Chromium (pinned Docker image) driving the real SPA — the 3-test Selenium smoke ported, plus ~8–12 "golden UI states" (feed tree + counters, headlines, article pane, labels, prefs, error) recorded **once against the PHP oracle** and asserted against Rust. The UX-level floor; see `docs/php-tests-and-browser-automation.md`.

---

## 6. Decisions (locked — user responses)

1. **Frontend**: kept untouched; Rust serves it byte-for-byte.
2. **Plugins**: native Rust plugin API + ported first-party plugins (option a); community PHP plugins unsupported; **git-based plugin installer deferred** (M7 ships read-only listing only).
3. **Database**: PostgreSQL-only, clean cutover (no PHP/Rust cohabitation); Rust must pass tests on a fully populated PHP-originated dataset (golden DB from the oracle).
4. **External API**: level-23 compatibility maintained.
5. **CLI/daemon**: keep existing surface unless clearly better; changes must be proposed *before* implementation → proposal in RESEARCH-REPORT §2.5 (single `ttrss` binary with `serve`/`update`/`daemon`, no forking, kept lockfile names/exit codes). **Needs user approval before M8.**
6. **Priority**: MVP-first, demoable end-to-end checkpoints; confidence marker = read-only UI on real data (M2).
7. **Oracle**: we dockerize a live PHP instance (pinned `sha-` images from the repo's `.docker/` setup); details in RESEARCH-REPORT §2.2.
8. **Naming**: the Rust port is **`ttrss`** (no dashes) — one app crate `ttrss` with a `core` module; internal helper crates `test-db` and `harness` (no prefix; workspace-scoped); the frozen PHP reference stays `tt-rss` (dashes). GPLv3 retained.
9. **Performance**: bar is "faster than PHP-FPM"; baseline recorded in M0, perf gate in M9 (RESEARCH-REPORT §2.6).
