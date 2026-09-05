# tt-rss PHP → Rust Backend: Migration Plan (project: **tt-rust**)

Delivery is tracked as a milestone ladder (**M0–M9**, §3). §3a keeps the original
per-subsystem phase notes (P0–P8) as the detailed work-item reference. Locked
decisions: §6.

---

## 1. Codebase Analysis

### 1.1 Inventory (what exists in `tt-rss/`)

| Area | Size | Notes |
|---|---|---|
| Core PHP (`classes/`, `include/`, entry points) | ~43.5k LOC, 130 files | `classes/` alone is 58 files / 21.2k LOC |
| Bundled plugins (`plugins/`) | ~2.8k LOC, 32 files, 15 plugins | auth_internal, auth_remote, af_* filters, nsfw, note, share, bookmarklets, cache_starred_images, hotkeys_*, etc. |
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
|---|---|---|
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
|---|---|---|
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
tt-rss-rs/
├── Cargo.toml                  # workspace, GPL-3.0 license
├── crates/
│   ├── ttrss-core/             # domain: config, prefs, users, feeds, articles,
│   │                           #   counters, labels, filters, search, opml, digest,
│   │                           #   sanitizer, url helpers, feed parsing, templates
│   ├── ttrss-db/               # sqlx pool, model structs, migration runner (reuses
│   │                           #   the exact .sql files from tt-rss/sql/pgsql)
│   ├── ttrss-web/              # axum server: sessions, CSRF, i18n, dispatcher,
│   │                           #   backend.php/public.php/api handlers, shells,
│   │                           #   static assets, syndication
│   ├── ttrss-cli/              # update.php equivalent (clap) + daemon (tokio tasks)
│   ├── ttrss-plugins/          # plugin trait registry + first-party plugin ports
│   └── (optional) ttrss-i18n/  # gettext .mo loading, Accept-Language, __()/T_sprintf
├── assets/                     # copied from tt-rss: js/, lib/, themes/, images/,
│                               #   locale/, templates/, manifest.webmanifest, sql/
└── tests/                      # golden-file + differential test harness
```

Binary: one **`tt-rust`** binary with subcommands `serve` (web server), `update` (CLI ops), `daemon` (long-running updater); `update --daemon` preserved for exact PHP parity. Legacy `--opt:value` flag surface kept (see RESEARCH-REPORT §2.5 for the full keep/propose list — **to be confirmed before implementation**).

### 2.2 Crate choices (vetted, mainstream — no custom infrastructure)

| Concern | Crate(s) | Notes |
|---|---|---|
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
3. **Loose-type fidelity layer.** PHP's dynamic typing (`"f"` vs `false`, string ints, checkbox `"on"`, `_param_to_bool`, `sql_bool_to_bool`) is a real compatibility hazard → a small `ttrss-types` module implementing the exact coercion rules + property tests against recorded PHP behavior.
4. **Plugin system re-implemented natively in Rust** (trait-based hooks mirroring `PluginHost`), first-party plugins ported; **community PHP plugins cannot run** (see open question #2 — this is the biggest product risk).
5. **One process, two binaries' worth of capability**: web server (tokio) and update workers (tokio tasks) share `ttrss-core`; lockfiles kept for ops compatibility.

---

## 3. Delivery Plan — milestone ladder (M0–M9)

Restructured per `research-report.md` (strangler-fig / vertical-slice / golden-master methodology). **Rule: every milestone ends with a live, demoable end-to-end system and a green oracle-diff harness for everything it claims.** Port order = usage-census order (hot ops first), respecting the read → write → background ladder. §3 (this section) is the **delivery order** (which milestone to demo next); §3a is the **detailed work-item reference** (per-subsystem, consulted as each milestone is started).

| M | Milestone | Demo / exit criteria |
|---|---|---|
| **M0** | Oracle + harness | Dockerized PHP oracle (pinned `sha-` image) up with populated **golden DB** (`pg_dump` fixture); request-corpus recorder; structural JSON/HTML/Atom diff tool with non-determinism normalizers; harness proven against a stub Rust server; PHP perf baseline recorded |
| **M1** | Walking skeleton | `tt-rust serve` serves all static assets + login page; real login (`auth_internal`, SSHA-512) against golden DB; SPA shell renders in a browser; DB sessions (encrypted) read/write |
| **M2** | **Read-only UI on real data** (confidence marker) | Feed tree + counters + headlines + article content + search + labels correct on golden dataset; `backend.php` read ops + external API read ops (level 23) pass diff harness; Atom/JSON syndication, feed icons, ETag shells |
| **M3** | Mutations I: article state | mark/publish/star/unread, catchup (feed/cat/all), notes, scores, labels, tags; harness checks responses **and** resulting DB state vs PHP |
| **M4** | Mutations II: configuration | prefs/profiles, feed CRUD + categories + icons + ordering + OPML, filter CRUD + execution, label CRUD, access keys |
| **M5** | External API completeness + admin | Full level-23 API; user admin (`Pref_Users`), system (log, test email), password recovery, OTP/QR |
| **M6** | The engine: updates | `tt-rust update` + `daemon` (tokio workers): fetch/parse/hash/dedup/filters/enclosures/favicons, counter-cache maintenance, failed-feed disabling, cron housekeeping, digest emails; updater run on golden dataset diffed vs PHP updater |
| **M7** | Plugin system + first-party plugins | Rust plugin API; port auth_internal/remote, nsfw, note, af_* (incl. psql_trgm), share, bookmarklets, cache_starred_images, hotkeys; plugin storage + per-user enablement portable; plugin installer deferred (listing only) |
| **M8** | CLI/daemon parity + migrations | All `update.php` subcommands, lockfile semantics, `--update-schema` reusing exact `.sql` files, exit codes |
| **M9** | Hardening & performance | Fuzz (sanitizer/parser/URL), security matrix, perf vs PHP baseline (target: faster), Docker images, docs, cutover runbook |

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

Goal: `tt-rust serve` boots, serves static + login, and can log in against the
golden DB with encrypted DB sessions; SPA shell renders. Progress is
demoable and oracle-diff-verified per surface.

Done:
- **`ttrss-core` (24 tests, no HTTP dep):**
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
- **`ttrss-server` (axum, binary `tt-rust serve`):** base-path nesting, `/healthz`,
  login-page renderer (`templates/login.html.tpl`: scripts/theme cache-bust from
  real mtimes, PHP-style urlencode), and a **custom static handler** — full
  header control, byte-for-byte bodies, path-traversal protection.
- **Static header parity:** Content-Type from the **canonical nginx `mime.types`**
  (107 ext; e.g. `application/javascript`, not `mime_guess`'s `text/javascript`),
  `Cache-Control: no-cache`, no `Accept-Ranges` (nginx omits it on the gzip path
  real clients hit). `crates/server/src/mime.rs` holds the nginx table.
- **Login flow (verified 4/4 against the oracle through the comparator):**
  `POST /public.php?op=login` — `crates/server/src/auth.rs` ports the
  `auth_internal` web path: SSHA-512 verify + TOTP (ASCII-byte key, no window);
  no-otp → 200 OTP challenge page (`templates/otp_challenge.html.tpl`),
  valid-otp → 302 + `Set-Cookie: ttrss_sid=<32hex>; path=/` + `Location` (validated
  `return` else self URL) + PHP `nocache` headers (`no-store, no-cache,
  must-revalidate`, `Pragma: no-cache`, fixed 1981 `Expires`). Non-OTP users get
  `last_auth_attempt` throttling (`AUTH_MIN_INTERVAL`). Verified: `healthcheck`,
  `login_page`, `web_login` (200 challenge), `web_login_otp` (302) all diff clean.
- **DB-backed encrypted session middleware** (`crates/server/src/session.rs`):
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

M1 is done: `tt-rust serve` boots, serves static byte-for-byte, renders the
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
- `ttrss-core` keeps zero HTTP deps so the web framework (axum) stays swappable.

### M2 status: CORE COMPLETE — read-only UI on real data (2026-09-05)

M2 = *read-only UI on real data*. The `backend.php` read surface that the SPA
needs to render the main content area is now ported and diffed against the
oracle. **`Feeds::view` (feed + category), `Feeds::search`, `RPC::getAllCounters`,
`Pref_Feeds::getfeedtree`, `RPC::sanityCheck`, and `RPC::getRuntimeInfo` all diff
clean** on the golden dataset. Done so far:

- **`backend.php` dispatcher** (`crates/server/src/rpc.rs`): `op`/`method`
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
  + `init-params` `labels` share `Labels::get_all`.
- **`Prefs` resolver** (`crates/server/src/prefs.rs`): the `ttrss_prefs` DB
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
- **`Pref_Feeds::getfeedtree`** (`crates/server/src/feeds.rs`) — the feed list
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
- **`Feeds::view` / `Feeds::search` (headlines)** (`crates/server/src/headlines.rs`):
  the main content area. Ported `Feeds::view` (feed + category + virtual feeds),
  the full `_get_headlines` dynamic-SQL branch logic (starred/published/fresh/all/
  recently-read/archived/error/label feeds + search), the `yyiw` date-grouping
  `ORDER BY` (with `VFEED_GROUP_BY_FEED` default `false`), first-id check /
  `ForceUpdate`, and `_format_headlines_list` (32-key article objects, exact key
  order). `content_preview` is byte-identical (70/70 articles); `content`/`buttons`
  compare structurally (DOM). Article time rendering uses the new `time` module.
  `content_preview`/`feed_bg_color`/enclosures/labels all verified against the oracle.
- **`RPC::getAllCounters`** (`crates/server/src/counters.rs`): ports `Counters::get_all`
  (feed + category + label counters, 64 entries). Uses the oracle's session settings
  (`cpu_tuple_cost = 0.5`, `TIME ZONE 0`) and **no explicit `ORDER BY`** so the natural
  plan order matches the PHP. Counter *values* and the *order relative to PHP* are
  byte-identical on the same DB (verified: PHP oracle and Rust produce the identical
  label-counter order on the same dataset).
- **`time` module** (`crates/server/src/time.rs`): ports `TimeHelper::make_local_datetime`
  + `smart_date_time` + the user's `SHORT_DATE_FORMAT`/`LONG_DATE_FORMAT` (uid 1: `j M, G:i`),
  the `eta_min` and bare-time cases, and `naive_to_pg_text` (a `timestamp without time zone`
  → the exact PG/PDO text, fraction only when non-zero, trailing zeros stripped).
- **Sanitizer libxml2 fidelity** (`crates/server/src/sanitizer.rs`): PHP uses libxml2
  (`DOMDocument::loadHTML`), which does *implicit `<p>` insertion* for leading bare text
  and drops leading whitespace/comments; html5ever (spec-compliant) does not. Ported
  `apply_libxml_implicit_p` to match — this cleared the category-view content diffs.
- **Harness normalizer (M2 additions):** smart-date / bare-time tokens collapse to one
  token (they differ run-to-run and server-to-server); URL authority ports are stripped
  (oracle vs Rust run on different ports by construction).
- **Verified (2026-09-05):** on the restored golden dataset, `rpc_sanity_check`,
  `rpc_runtime_info`, all feed-tree steps, **all four headlines steps** (feed unread,
  feed all, category, search), `rpc_counters` (values + PHP-order), and the full auth
  flow (login/shell/logout, admin + readonly + OTP) diff **clean** — **25/61**.
  Note: the pre-recorded fixture set (`oracle-a`) and the `golden-db.sql` dump are
  slightly out of sync; `rpc_counters` counter *values* are identical but the *order*
  (natural plan order, stats-dependent) differs from the stale fixture — this is a
  dataset artifact, not a code difference (Rust matches the live PHP oracle on the
  same DB exactly). Re-seeding the oracle + re-recording `oracle-a` from the same
  frozen DB would close this gap.
  Remaining mismatches are the unimplemented M2+ surface: `hotkeyHelp`,
  `checkForUpdates`, the `Pref_*` handlers, the external JSON API (level 23),
  Atom/JSON syndication, and feed icons.
- **Next (M3):** mutations I — article state: mark/publish/star/unread, catchup
  (feed/cat/all), notes, scores, labels, tags. The harness will check both the
  response **and** the resulting DB state vs PHP.

### Test-parity program (PHP `#[test]` → Rust `#[test]`, 2026-09-05)

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

| module | parked PHP cases (in `crates/*/src/*.rs` `REVISIT` block) |
|---|---|
| `server::prefs` | `PrefsTest::set_*` (insert/update/bool/string/tags/unchanged/invalid), `get_all_*`, `set_*_profile*`, `reset_*`, `multiple_users_isolated` (14 cases) |

| # | PHP suite | Rust module | status |
|---|---|---|---|
| 1 | `TimeHelperTest` (41) | `server::time` | ✅ 24 ported (applicable); N/A: `$long`/`no_smart_dt`, uid/profile, `convert_timestamp` |
| 2 | `SanitizerTest` (162) | `server::sanitizer` | ✅ **256** cases across all four PHP sanitizer suites — `tests/mocked/SanitizerUnitTest` (81), `tests_functional/SanitizerTest` (11), `tests/integration/SanitizerTest` (2), `tests_integration/SanitizerTest` (162) — ported 1:1 into `sanitizer_parity.rs` (+`sanitizer_parity/{mocked,functional,unit,integration}.rs`). Ground truth: PHP 8.5.9 / libxml 2.13.09 oracle. 5 cases carry a NOTE — 2 where the PHP test is itself buggy (the oracle fails it as written: `<source>` without `src` is not stripped; `<font color="red">` survives so the exact-string assert fails) and 3 where libxml 2.13 entity-encodes non-ASCII in `saveHTML` (Rust serializes raw UTF-8); each is ported 1:1 in inputs with the corrected / semantically-equivalent expectation. Re-implemented `validate`/`rewrite_relative`/`build_url`/`parse_url`/`has_disallowed_ip`/`canonicalize_ipv4_literal` to match PHP exactly (case-preserving, no dot-segment resolution, `//`→https, data/mailto/magnet exceptions, IP-literal canonicalization); added `iframe_whitelisted`, `wrap_iframes_in_div`, `highlight_words[_str]`, head→body fixup for `<noscript>`. |
| 3 | `PrefsTest` (29) | `server::prefs` | ✅ 16 pure cases; **14 DB cases parked** (REVISIT, see ledger) |
| 4 | `FunctionsTest` (73) | `server` / `core` helpers | ✅ `truncate_string` (10, `headlines.rs`) + `with_trailing_slash` (5, `main.rs`) + `sql_bool_to_bool` (via `prefs`); **~50 parked** (REVISIT: `clean` 8, `truncate_middle` 7, `mb_substr_replace` 6, `arr_qmarks` 6, `bool_to_sql_bool` 2, `checkbox_to_sql_bool` 2, `implements_interface` 4, `get_theme_path` 2, `theme_exists` 1, `uniqid_short` 4, `get_random_bytes` 3, `t_sprintf` 2, `t_nsprintf` 1, `gzdecode` 1, `get_scripts_timestamp` 2) |
| 5 | `UrlHelperTest` (69) | `server` / `core` url | ✅ `validate` (9, `sanitizer.rs`) + `url_to_youtube_vid` (6, `headlines.rs`); ~~REVISIT: trailing-slash on host-only URLs + scheme-case preservation~~ **resolved** by the row-2 faithful `validate` (case preserved, no trailing slash, IDN→punycode); `rewrite_relative` + `build_url` + `canonicalize_ipv4_literal` now ported (see row 2; `rewrite_relative_cases` added); N/A: `resolve_redirects` (4), `fetch` (16), `has_disallowed_ip` DNS cases |
| 6 | `CryptTest` (23) | `core::crypt` | ✅ 12 primitive cases; 7 N/A (config lookup, base64 codec, `algo` whitelist live in the caller — REVISIT block) |
| 7 | `ConfigTest` (16) | `server::main` + `core::config` | ✅ `matches_self_url` (18 provider cases, `main.rs`); `cast_to` covered in `prefs::tests`; N/A: `get_self_url` a–k + `get_self_dir` (self URL from env); REVISIT: IDN/Punycode host normalization |
| 8 | `UserHelperTest` (8) | `server::auth` | ✅ 7/8 ported (`tests/user_parity.rs` on `#[sqlx::test]` + fixtures + `common::make_db`); 1 case → row 9 (session-id regeneration) |
| 9 | `SessionsIntegrationTest` (29) | `server::session` | — |
| 10 | `ArticleTest` (32) | `server::article` | — |
| 11 | `FeedsCountersIntegrationTest` (40) | `server::counters` | — |
| 12 | `FeedsHeadlinesIntegrationTest` (20) | `server::headlines` | — |
| 13 | `LabelsTest`/`LabelsIntegrationTest` (15/35) | labels | — |
| 14 | `RPCTest` (10) | `server::rpc` | — |

(Feed-parser/RSS-utils/DiskCache/Scheduler/PluginHost/Api suites map to modules
not yet ported and will be added alongside those modules in M4+.)

#### Test infrastructure for DB-backed suites (status: COMPLETE, 2026-09-05)

**Goal:** rows 8–14 (DB-backed parity suites) run against a **dedicated** Postgres
(separate from the oracle stack), versioned schema + seed (NOT the golden dump),
isolated per test, fast + repeatable.

**Design (decided — replaces the earlier hand-rolled template-clone; build on
libraries, don't cleanroom the lifecycle):**
- **Server** = a *reused* testcontainers `postgres:15-alpine` container
  (`crates/testing`, `CONTAINER_NAME = "ttrss_test_pg"`). Reused containers persist
  across runs and are **not removed on handle drop** (verified in testcontainers
  0.27.3 source: `ContainerAsync::drop` returns early for
  `ReuseDirective::Always`/`CurrentSession`). A `#[ctor]` in each test binary
  starts/attaches it and sets `DATABASE_URL` (`crates/testing::init_database_url`).
  `TTRSS_TEST_PG_URL` overrides with any dedicated instance.
- **Per-test DB** = sqlx `#[sqlx::test(fixtures(...))]`. Confirmed from sqlx 0.7.4
  source (`sqlx-core/src/testing/`, `sqlx-postgres/src/testing/`): creates a fresh
  `_sqlx_test_N` DB per test, applies fixtures (baked via `include_str!`, in order,
  each as one command string), injects the pool, drops the DB on success (failed
  tests keep it; stale DBs are reaped on the next run via the `_sqlx_test.databases`
  bookkeeping table + a 2s margin). Reads `DATABASE_URL` (a superuser) at runtime.
- **Fixtures** = `crates/testing/fixtures/{schema,baseline,seed}.sql` (versioned =
  the frozen reference's base schema + admin user + PHP `seed.sql`), referenced by
  **explicit path** from each test file (`include_str!` is source-file-relative).
- **Session parity** = reuse `ttrss_core::db::SESSION_SETUP` via the `#[sqlx::test]`
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
- ✅ `crates/testing` rewritten to the design above: `PgServer::acquire` (reuse
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

**Row 9–14 pattern** (each new suite = one file in `crates/server/tests/`):
`mod common;` + per test `#[sqlx::test(fixtures("../../testing/fixtures/schema.sql",
"../../testing/fixtures/baseline.sql", "../../testing/fixtures/seed.sql"))]` with the
`PoolOptions<Postgres> + PgConnectOptions` signature and `common::make_db`. Extra
per-suite state = plain INSERTs after `make_db` (or `ttrss_testing::seed_user`).

#### M2 — `Feeds::view` headlines / Sanitizer approach

**DONE (committed):**
- `8efb755` harness: HTML-bearing JSON fields compared **structurally** (canonical
  DOM via existing `scraper`/`html5ever` in `crates/harness/src/html.rs`), not as raw
  strings. `normalize_json` → `html::is_html_fragment(s)` → `canonical_html(s)`.
- `e2a187b` **high-fidelity iterative sanitizer** (`crates/server/src/sanitizer.rs`,
  `kuchikiki`+`html5ever` 0.26): ports `Sanitizer::sanitize` — rewrite `<a>`/`<img>`/
  `source`/`video` (`UrlHelper::rewrite_relative`+`has_disallowed_ip`, `rel`/`target`,
  `referrerpolicy`/`loading`, `srcset` via `RSSUtils`), sandbox iframes, strip
  disallowed elements (whole subtree) + attrs (`on*`,`data-*`,id/style/class/width/
  height/allow/srcdoc, `javascript:` hrefs), highlight words. **All traversals + the
  serializer are iterative (heap stacks)** — no call-stack recursion, deep-nesting
  safe. Deps: `kuchikiki=0.8`, `html5ever=0.26` (workspace + server).
- Harness `canonical_html` now treats `<p>` as **transparent** (libxml HTML4 auto-wraps
  bare text; html5ever HTML5 doesn't) — reconciles the two without chasing libxml.

**NEXT (not started):** `Feeds::view` (017/018/019/056) + `Feeds::search` (020) +
`RPC::getAllCounters` (009). Wire a `"Feeds"` branch in `crates/server/src/rpc.rs` (add
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
- **Corpus steps still to satisfy:** `009 rpc_counters` (`RPC::getAllCounters`),
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
- `ttrss-core::config`: all ~60 `TTRSS_*` settings with types + defaults (port `Config::_DEFAULTS`), self-URL detection rules, version detection (build-time embedded git info → `version_static` fallback).
- `ttrss-db`: sqlx pool (host/port/user/pass/db/sslmode), `client_encoding`/`timezone` session setup, **migration runner reusing the `.sql` files** (version in `ttrss_version`, per-plugin tables).
- Error type hierarchy mirroring `Errors::*` (E_UNAUTHORIZED, E_UNKNOWN_METHOD, E_SCHEMA_MISMATCH, E_INCORRECT_USAGE, E_NOT_FOUND, E_OPERATION_FAILED, E_PLUGIN_*…).
- **Test harness**: PostgreSQL testcontainer (or service) → run migrations → seed fixture DB; golden-file capture tooling against a reference PHP instance (record `backend.php`/`api/index.php` responses to JSON fixtures for diffing).
- **Exit criteria**: `ttrss-db` boots a pristine PG and a v151 DB identically to PHP; config parity test vs PHP `Config::get()` for a matrix of env setups.

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
- **Exit criteria**: golden-file parity of an update run (feed XML fixtures → resulting DB rows) vs PHP; sanitizer corpus passes; counter math parity tests (feed/cat/label/virtual).

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
5. `Pref_Prefs`: the big one (1647 L) — pref UI payload, profiles (add/clone/remove/activate/save), plugin list/install/uninstall/updates (install/update deferred, listing only), app passwords, OTP QR, digest preview, CSS customization, resetconfig.
6. `Pref_Feeds`: feed tree, edit/save (single + batch), categories (add/rename/remove/sort), icons (upload/remove), OPML import, inactive/error feeds, access keys (clear/regen/shared URL).
7. `Pref_Filters`: tree, edit/clone/add/remove/save, rule/action names, test filter, ordering, join, optimize.
8. `Pref_Labels`: tree, edit, colors, save, add/remove.
9. `Pref_Users`: list, add, edit, details, remove, resetPass (admin).
10. `Pref_System`: log viewer, clear log, send test email, scheduled tasks list.
11. Plugin handler hooks (where relevant to UI).
- **Exit criteria**: every JS `xhr` call in `js/` answered with structurally identical JSON (golden diff < 0); Selenium smoke (existing `selenium_test.py`) passes against Rust server.

### Phase 5 — External API + syndication (→ M2 reads, M5 writes)

- `api/index.php` port: JSON envelope `{seq,status,content}`, method routing, `login`/`logout`/`isLoggedIn`/`getConfig` (incl. `daemon_is_running` lockfile check), all ~20 API methods, `Sec-Fetch-Mode` browser opt-in gating, per-user `ENABLE_API_ACCESS`.
- Syndication: `public.php?op=rss` Atom (Template port → minijinja of `generated_feed.txt`) + JSON feed; access-key auth; `noxml`; tag URI generation.
- Contract tests: port `tests/integration/ApiTest.php` cases; optionally test against published API docs (level 23).
- **Exit criteria**: ttrss-reader (or equivalent client) fully functional against Rust; syndicated Atom feed diff vs PHP (modulo timestamps) is clean.

### Phase 6 — CLI, daemon, mail, scheduler (→ M5 mail, M6 daemon/scheduler, M8 CLI parity)
- `clap` port of `update.php` (all subcommands incl. `opml-export/import`, `user-*`, `gen-search-idx` (Postgres FTS backfill via `html2text`), `gen-encryption-key`, `plugins-list`, `debug-feed`, `force-*`, `cleanup-tags`, `send-digests`, `update-schema`).
- Daemon: tokio worker pool replacing fork model (same lockfile names, spawn interval, feed batch limit, per-task stagger, child runtime cap via task timeout), single-process `--daemon` mode.
- Scheduler: `croner`-based, `ttrss_scheduled_tasks` persistence, all housekeeping tasks (orphan purge, cache expire, disable failed feeds, icon cleanup, login-limit log, error-log expire, lock-file expire, digests).
- Digest: `Digest::prepare_headlines_digest` (templates `digest_template*.txt`, `email_article_template.txt`) + `Mailer` (lettre; `LOG_SENT_MAIL` event logging).
- **Exit criteria**: ops parity — a Docker-style deployment (serve + daemon) runs unattended; digest emails match PHP templates; CLI golden outputs (user-list etc.).

### Phase 7 — Plugin system + first-party plugins (→ M7)

- Rust plugin API: trait `TtrssPlugin` (about, init, storage load/save, ~40 hooks, CSS/JS/login JS injection, public methods, API methods, virtual feeds, CLI commands, scheduled tasks, prefs tabs), `PluginHost` registry port (per-user enablement via `_ENABLED_PLUGINS`, plugin storage table, plugin migrations).
- Port first-party plugins in priority order: `auth_internal` (P1, already needed), `auth_remote`, `nsfw`, `note`, `af_comics`/`af_youtube_embed`/`af_zz_vidmute` (article filters — also validate the filter engine), `share`, `bookmarklets`, `cache_starred_images`, `hotkeys_*`, `af_psql_trgm` (SQL-only, trivial), `toggle_sidebar`.
- Plugin installer/update checker (git-based) — **scope decision needed** (see Q7); may ship read-only listing first.
- **Exit criteria**: stock plugin set reproduces PHP UI features (NSFW filter, notes, OTP auth, trigram search); plugin storage data portable (rows readable both ways).

### Phase 8 — Hardening, parity verification, packaging (→ M9)

- Security pass: CSRF matrix, SSRF fuzzing, sanitizer fuzzing (cargo-fuzz), session crypto interop, timing-safe comparisons, cookie flags, IDN edge cases, `Sec-Fetch-Mode` checks.
- Differential harness at scale: run PHP + Rust against the same DB + same feed corpus + same request replay (recorded), diff all JSON/Atom output; chase to zero meaningful diffs.
- Performance: counter-cache effectiveness, headline query parity (EXPLAIN), large-DB (10M articles) test; tune sqlx pool.
- Docker image(s) mirroring `supahgreg/tt-rss` + web-nginx pair; systemd units; docs (config reference, migration guide, plugin API guide).
- **Exit criteria**: drop-in replacement for an existing deployment (point both at same DB, switch nginx upstream).

---

## 4. Risks & Mitigations

| Risk | Severity | Mitigation |
|---|---|---|
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

---

## 6. Decisions (locked — user responses)

1. **Frontend**: kept untouched; Rust serves it byte-for-byte.
2. **Plugins**: native Rust plugin API + ported first-party plugins (option a); community PHP plugins unsupported; **git-based plugin installer deferred** (M7 ships read-only listing only).
3. **Database**: PostgreSQL-only, clean cutover (no PHP/Rust cohabitation); Rust must pass tests on a fully populated PHP-originated dataset (golden DB from the oracle).
4. **External API**: level-23 compatibility maintained.
5. **CLI/daemon**: keep existing surface unless clearly better; changes must be proposed *before* implementation → proposal in RESEARCH-REPORT §2.5 (single `tt-rust` binary with `serve`/`update`/`daemon`, no forking, kept lockfile names/exit codes). **Needs user approval before M8.**
6. **Priority**: MVP-first, demoable end-to-end checkpoints; confidence marker = read-only UI on real data (M2).
7. **Oracle**: we dockerize a live PHP instance (pinned `sha-` images from the repo's `.docker/` setup); details in RESEARCH-REPORT §2.2.
8. **Naming**: project **tt-rust**; crates `tt-rust-*`; GPLv3 retained.
9. **Performance**: bar is "faster than PHP-FPM"; baseline recorded in M0, perf gate in M9 (RESEARCH-REPORT §2.6).
