# Google Reader / FreshRSS API (freshapi plugin)

Native implementation of the FreshRSS / Google Reader API for the port, so
GR-protocol clients (Reeder, NetNewsWire, lire, Fiery Feeds, ReadKit, …) can
talk to ttrss-rust. Landed as a **Rust plugin** — the port of freshapi, the
[tt-rss PHP plugin](https://github.com/eric-pierce/freshapi) it replaces —
so GR clients keep working after moving from the PHP image. The reference is
freshapi, cloned to `freshapi/` (local-only, gitignored); it is the spec for
this feature, the way `tt-rss/` is the spec for the rest of the port.

## Why

- Current upstream tt-rss (the `c02520f` pin) has no Google Reader API.
  GR-protocol clients reach PHP tt-rss only through third-party plugins
  (freshapi is the maintained one).
- PHP plugins can't run in the port, so a ported tt-rss-rust install is
  worse than the PHP one it replaces for GR-client users. A native
  implementation closes that gap.
- freshapi's design is clean: it delegates to tt-rss's own official API
  (`callTinyTinyRssApi`) and to direct SQL, which maps onto our existing
  `api::*` functions and queries.

## Trait feasibility (why a plugin works)

In PHP, a plugin's files are served directly by the web server
(`plugins.local/freshapi/api/greader.php` is a real file), so "a plugin can
expose URLs" is part of the PHP plugin capability. Against the port's
`Plugin` trait (`crates/ttrss/src/pluginhost/plugin.rs`):

| freshapi requirement | plugin-trait supply | verdict |
|---|---|---|
| Direct SQL (categories, feeds, users, sessions) | `HostCtx::pool` | ok |
| Config (CACHE_DIR, SELF_URL) | `HostCtx::cfg` / `self_url` / `cache_dir` | ok |
| Call tt-rss ops in-process (login, getLabels, getCounters, getArticle, updateArticle) | same crate: call `api::*` / `labels::*` directly (freshapi's `callTinyTinyRssApi` = in-process API call; the port needs no indirection) | ok |
| Per-user enable gate | `_ENABLED_PLUGINS` resolved in `HostCtx::for_user` | ok |
| Arbitrary responses (JSON, text, OPML, custom status) | `MethodOutput::Response { status, content_type, body }` | ok |
| Raw request headers (Authorization, User-Agent) | `HostCtx::server_env` (raw header names/values, set by the HTTP layer) | ok |
| Session create / resume-by-id | `core::session::load/save` (pub; the official API's `sid` path uses the same primitives) | ok |
| **URL space + PATH_INFO** | **none — the trait has no route registration** | **missing** |
| CORS headers + `OPTIONS` → 204 | the plugin's protocol contract; needs `headers` on `MethodOutput::Response` (additive, default empty) + the HTTP method passed through to the route handler | missing (small) |

Two gaps, both small, both restored as **general host capabilities** (phase
0), so any future plugin can use them:

- `PluginHost::add_route` — a plugin declares a path prefix; the axum router
  consults `PluginHost` at request time, hands the plugin the raw request
  (method, path, query, headers), and emits the plugin's response as-is.
  The router wrapper is a dumb pipe; protocol specifics (CORS, OPTIONS 204)
  live in the plugin.
- `headers` field on `MethodOutput::Response` — additive, default empty;
  only existing user is `share::get`.

`PluginHost::add_api_method` (the closest existing seam) is insufficient:
the GR protocol is URL-path based (`/reader/api/0/...`), not `op=` based.

## Wire protocol (from `freshapi/api/freshapi.php`)

Single entry point; the client-configured base URL is the "script" and the
rest rides on `PATH_INFO`:

- `POST <base>/accounts/ClientLogin` with `Email` + `Passwd` →
  `SID=<user>/<session_id>` / `LSID=` / `Auth=<user>/<session_id>` text
  lines. This is the only request that takes a password.
- Everything else: `GET|POST <base>/reader/api/0/...` with
  `Authorization: GoogleLogin_auth/<user>/<session_id>`.

Dispatch (`FreshGReaderAPI::parse`, freshapi.php:1460):

| path | handler | response |
|---|---|---|
| `accounts/ClientLogin` | `clientLogin` | text (`SID=…`) |
| `reader/api/0/stream/contents/feed/<id>` | `streamContents` | JSON stream |
| `reader/api/0/stream/contents/user/-/state/com.google/reading-list` | `streamContents` | JSON stream |
| `reader/api/0/stream/contents/user/-/state/com.google/starred` | `streamContents` | JSON stream |
| `reader/api/0/stream/contents/user/-/label/<label>` | `streamContents` | JSON stream |
| `reader/api/0/stream/contents` (bare, EasyRSS/FeedMe) | `streamContents` reading-list | JSON stream |
| `reader/api/0/stream/items/ids` (`s=…`) | `streamContentsItemsIds` | JSON |
| `reader/api/0/stream/items/contents` (`i=…`, FeedMe) | `streamContentsItems` | JSON |
| `reader/api/0/tag/list?output=json` | `tagList` | JSON `{tags:[{id,type}]}` |
| `reader/api/0/unread-count?output=json` | `unreadCount` | JSON `{max, unreadcounts[]}` |
| `reader/api/0/subscription/list?output=json` | `subscriptionList` | JSON |
| `reader/api/0/subscription/export` | `subscriptionExport` | OPML |
| `reader/api/0/subscription/import` (POST raw OPML) | `subscriptionImport` | text |
| `reader/api/0/subscription/edit` (`s=`, `t=`, `ac=subscribe|unsubscribe|edit`, `a=`, `r=`) | `subscriptionEdit` | JSON |
| `reader/api/0/subscription/quickadd` (`quickadd=`) | `quickadd` | JSON |
| `reader/api/0/edit-tag` (`i=…`, `a=`, `r=`) | `editTag` | `OK` |
| `reader/api/0/rename-tag` (`s=`, `dest=`) | `renameTag` | JSON |
| `reader/api/0/disable-tag` (`s=…`) | `disableTag` | JSON |
| `reader/api/0/mark-all-as-read` (`s=`, `ts=`) | `markAllAsRead` | JSON |
| `reader/api/0/token` | `token` | 57-char SHA-256 text (unverified by clients; sent back as `T=`) |
| `reader/api/0/user-info` | `userInfo` | JSON `{userId,userName,userProfileId,userEmail:""}` |

Unknown/short paths → 400. `OPTIONS` → 204. CORS headers on every response:
`Access-Control-Allow-Origin: *`, `Allow-Headers: Authorization`,
`Allow-Methods: GET, POST` — all emitted by the plugin. A bare request to
the base URL (no PATH_INFO, no query) → `OK` (the health check).

### Auth model

- `ClientLogin`: standard tt-rss credentials — app passwords first, main
  password only when OTP is off; disabled accounts and accounts without
  `ENABLE_API_ACCESS` are rejected (401). This is exactly our
  `api::login` (`crates/ttrss/src/api.rs:250`) — reuse it, take the
  `session_id` from its content.
- Subsequent requests: the token is `user/session_id`; the session must
  exist, the username must match the session's, and the session must pass
  the per-request validity checks (`freshapiSessionAllowed`: session
  valid + `ENABLE_API_ACCESS` still set). Password change / API disable /
  account disable therefore signs clients out naturally.
- Per-user plugin gate: `_ENABLED_PLUGINS` (checked per request once the
  session's uid is known), as in freshapi; `ClientLogin` must be reachable
  with the plugin not yet enabled (the login itself is the entry point).
- Read-only users (`access_level = 1000`): writes (`subscription/edit`,
  `import`, `rename-tag`, `disable-tag`) → 403 `Forbidden!`
  (`requireWriteAccess`).

### Item / id encoding

- Article id on the wire: `tag:google.com,2005:reader/item/<hex>`, hex =
  decimal `ttrss_entries.id` in **lowercase hex without padding**
  (`dec2hex`).
- Inbound decode: `basename()` (strip the prefix), then
  `hex2dec` when the id is non-numeric, starts with `0`, **or the client
  is NetNewsWire** (it sends plain decimals that freshapi still hex2decs —
  reproduce verbatim, UA prefix check `substr(ua, 0, 11) == "NetNewsWire"`).
- Item JSON (freshapi.php:1128): `id`, `crawlTimeMsec` (sec + `"000"`),
  `timestampUsec` (sec + `"000000"`), `published` (sec), `title`
  (entity-decoded; `&<>` — plus `?\/;` for titles — swapped for their
  fullwidth forms), `canonical`/`alternate` href, `categories`
  (reading-list/read state, `starred` when marked, `user/-/label/…` for
  article labels), `origin` (`streamId: feed/<id>`, title, `htmlUrl` from
  link), `summary.content` (truncated at 500,000 bytes UTF-8), `author`,
  `enclosure[]` (`href`,`type`,`length` when duration ≠ 0).
- `unread-count`: `newestItemTimestampUsec` = the counter `ts` zero-padded
  to 16 chars; `max` = the `global-unread` counter.

## Design

### Mount point

Serve the API at **both**:

- `<self_url>/freshapi/…` — the native path (clean, no PHP vestiges)
- `<self_url>/plugins.local/freshapi/api/greader.php/…` — **freshapi
  compatibility path**. A user moving from PHP tt-rss + freshapi keeps
  their exact client base URL; only the image changes. This is the
  "100% drop-in" story.

The `.php` suffix is cosmetic (axum routes are literal strings). Both paths
hit the same plugin route.

### Module layout

`crates/ttrss/src/plugins/freshapi.rs` — with the other ported plugins.
Mirrors freshapi's structure, one function per endpoint:

- `handler` — method/OPTIONS/CORS, PATH_INFO split, dispatch (the `parse`
  table above), 400 fallback.
- `client_login` → wraps `api::login` (shared credential policy); on
  success emit the `SID/LSID/Auth` text.
- `authorized_session` — parse `GoogleLogin_auth/<user>/<sid>`, load the
  stored session, validate user match + `ENABLE_API_ACCESS`, return
  `uid` + sess.
- `stream_contents` / `stream_items_ids` / `stream_items` — scope
  resolution (feed id / reading-list / starred / label) then reuse the
  same entry queries our `api::get_headlines`/`get_article` use; format
  via `convert_to_freshapi` (freshapi.php:1128).
- `edit_tag` → `api::update_article` (read/star) + label ops from
  `labels.rs` (create-if-missing + attach, as in freshapi.php:1206).
- `unread_count` → `api::get_counters` + categories/labels titles.
- `tag_list`, `subscription_list`, `subscription_edit`
  (subscribe/unsubscribe/rename via `feeds::subscribe`/`remove_feed`),
  `rename_tag` / `disable_tag` (`labels.rs`), `mark_all_as_read`
  (`catchup` semantics), `token`, `user_info`.
- OPML: `opml::export` (rpc.rs:440 area) for `subscription/export`,
  `opml::import` for `subscription/import` (freshapi writes the raw body
  to a temp file; our `opml::import` takes the bytes directly).

Registration: constructor in `pluginhost::all_plugins()`, `about().system`
off (per-user plugin in the reference — `plugins.local`), so the
`_ENABLED_PLUGINS` gate applies per user, as in freshapi.

No new dependencies. IDs i32 end to end (the wire hex is the one place
decimal-string conversion happens).

### Test plan

- Integration tests per endpoint against `test-db` + the mock feed server
  (pattern: `tests/on_demand_updates.rs`): login (app password, OTP, bad
  creds, API disabled), per-user enable gate, auth failure, stream
  contents (reading-list, feed, starred, label), edit-tag
  (read/unread/star/unstar/label), unread-count shape, tag list,
  subscription list + subscribe/unsubscribe, OPML round-trip, token,
  read-only 403s.
- Host-capability tests for phase 0: route registration (match, raw
  request shape, response headers emitted), unregistered prefix → normal
  404.
- Client-compat checklist (smoke, manual): Reeder Classic, NetNewsWire,
  Fiery Feeds — login, sync, mark-read, star, subscribe.

## Phases

0. **Plugin host: route registration** (fleshed out below). Landing this
   first unblocks the plugin. Protocol specifics (CORS, OPTIONS 204) live
   in the plugin; the host and router stay generic.
1. **v1 core** — the freshapi plugin: `ClientLogin`, auth, `stream/contents`
   (all four scopes), `stream/items/ids`+`contents`, `edit-tag`,
   `unread-count`, `tag/list`, `user-info`, `token`. (Enough for a GR
   client to log in and read.)
2. **v2 management** — `subscription/list|edit|quickadd|export|import`,
   `rename-tag`, `disable-tag`, `mark-all-as-read`. **Done (2026-10-26):**
   all eight verbs, the `requireWriteAccess` 403 gate (import / edit /
   quickadd / rename / disable only — `mark-all-as-read` is not write-gated
   in the reference), reusing `api::subscribe_to_feed` / `unsubscribe_feed`,
   `opml::export` / `import`, and the phase-1 `stream_scope`. Deviations
   (documented in code): the OPML export uses the opml module's
   `application/xml+opml` type (the reference emits PHP's default
   text/html); `opml::import` runs on the bytes directly (no temp file);
   the read-only check reads the session's access level (DB-validated per
   request) instead of a fresh user query.

### Phase 0 sketch

Four additions, all additive (every existing plugin stays unchanged):

**1. `plugin.rs` — the new trait surface.**

```rust
/// One HTTP route a plugin claims (phase 0).
pub struct RouteSpec {
    /// Path prefix, e.g. `/freshapi`. Matched on whole segments: it
    /// matches itself and anything starting with `prefix + "/"`
    /// (`/freshapiX` does not match `/freshapi`).
    pub prefix: &'static str,
    /// Accepted methods; anything else gets a 405 from the router
    /// *before* the plugin is called. The freshapi plugin declares
    /// `GET, POST, OPTIONS` — OPTIONS is the plugin's to answer (204),
    /// not the router's.
    pub methods: &'static [Method], // `http::Method` (axum already depends on `http`)
}

/// The raw request handed to `handle_route`.
pub struct RouteRequest<'a> {
    /// The HTTP method (router has already filtered to `methods`).
    pub method: Method,
    /// The path after the matched prefix — the PATH_INFO, no leading
    /// slash (`reader/api/0/stream/contents/feed/3`); empty when the
    /// request hit the prefix itself.
    pub path_info: &'a str,
    /// The raw query string (`n=20&ot=…`); empty if none.
    pub query: &'a str,
    /// The raw body (e.g. the OPML for `subscription/import`).
    pub body: &'a [u8],
    // No headers field: the raw headers are already in
    // `HostCtx::server_env` (Authorization, User-Agent, …).
}

pub trait Plugin: Send + Sync {
    // … existing items …

    /// Routes this plugin claims. The lookup source behind
    /// `PluginHost::add_route` (the `init`-time registration, mirroring
    /// `add_hook`/`add_api_method`). Default: none.
    fn routes(&self) -> &'static [RouteSpec] { &[] }

    /// Handle a request that matched one of this plugin's routes.
    /// The router folds the returned `MethodOutput` into the final
    /// response (status, Content-Type, body, extra headers).
    /// Default: unreachable (only called for registered prefixes).
    async fn handle_route(
        &self,
        ctx: &mut HostCtx,
        req: &RouteRequest<'_>,
    ) -> MethodOutput {
        let _ = (ctx, req);
        MethodOutput::Raw("404".to_string())
    }
}
```

Naming: `handle_route` (no `hook_` prefix) because it is a *dispatch*,
not a hook — one plugin, resolved by prefix, called once, no chain and
no priority. Same class as `invoke_method`/`authenticate`; the `hook_*`
methods are chain handlers iterated by the hook runner.

**2. `MethodOutput::Response` — header passthrough.**

```rust
Response {
    status: u16,
    content_type: &'static str,
    body: String,
    /// Extra response headers (phase 0), emitted after the standard
    /// ones (duplicates win). The freshapi plugin's CORS block lives
    /// here. `String` values: the interface is per-request, so header
    /// values may be computed at request time (an echoed `Origin`, a
    /// built `Location`, …). Empty by default — the only existing
    /// constructor (`share::get`) is unaffected.
    headers: Vec<(String, String)>,
},
```

**3. `host.rs` — registration + lookup.**

```rust
impl PluginHost {
    /// Claim a route (called from the plugin's `init`, as with
    /// `add_hook`). Records `(prefix, plugin index, methods)`.
    /// Duplicate prefix → panic: registration is compile-time, from
    /// the `init` calls at `PluginHost::startup`, so a collision is
    /// a developer error and fails the boot (deliberately stricter
    /// than `add_api_method`, whose `insert` silently overwrites).
    pub fn add_route(&mut self, prefix: &'static str, methods: &'static [Method]);

    /// Longest-prefix, segment-boundary match against the request path
    /// (base already stripped). `None` → not a plugin route.
    pub fn lookup_route(&self, path: &str) -> Option<RouteMatch>;
}

pub struct RouteMatch<'a> {
    pub plugin: &'a dyn Plugin,
    pub prefix: &'static str,
    pub path_info: &'a str, // path after the prefix
}
```

The freshapi plugin registers both mount points in `init`:
`/freshapi` and `/plugins.local/freshapi/api/greader.php` (the `.php`
suffix is just a literal in the prefix string).

**4. `http.rs` — the router consult.**

The static routes are unchanged; the current `.fallback(static_handler)`
becomes a plugin-aware fallback:

```rust
async fn plugin_aware_fallback(State(state), req) -> Response {
    if let Some(m) = state.plugins.lookup_route(&req.uri().path()) {
        let mut ctx = session_ctx(&state, /* the cookie session, usually
            empty on this path — freshapi authenticates via the
            Authorization header and loads its own session */).await;
        let out = m.plugin
            .handle_route(&mut ctx, &RouteRequest { /* from req + m */ })
            .await;
        fold MethodOutput into Response:
            status, Content-Type: content_type, body,
            then extend with `headers` (Json/Raw → their usual types)
    } else {
        static_handler(state, req).await
    }
}
```

Method filtering (405) happens in the lookup/handler before `handle_route`
is called. The base-path build (the second `Router` with `base_slash`)
strips the base before `lookup_route`, so plugin prefixes are
base-relative, as in PHP.

**Per-user gate.** There is no hook-runner filter on this path, so the
plugin self-gates: after resolving the session's uid it checks its own
active flag (e.g. `ctx.active_plugins().any(|p| p.name() == "freshapi")`)
and 404s when disabled — `ClientLogin` exempt (it is the entry point).

**Phase 0 tests** (pattern: `tests/dispatch_completion.rs`): a stub
plugin claiming `/stub` with `GET, POST, OPTIONS` —

- `GET /stub/a/b?x=1` → stub sees `path_info "a/b"`, `query "x=1"`,
  empty body; its `Response { 200, "text/plain", "hi",
  headers: [("access-control-allow-origin","*")] }` comes back verbatim
  (assert status, body, header, Content-Type).
- `OPTIONS /stub` → the stub's own 204 + CORS headers are delivered.
- `DELETE /stub` → 405 from the router, stub not called.
- `GET /stubby` → falls through to the static handler (segment boundary).
- `POST /stub` with a body → the body reaches the stub (OPML import).
- `share::get` unchanged (empty `headers`, existing tests green).

### Phase 1 sketch

The freshapi plugin (`crates/ttrss/src/plugins/freshapi.rs`), on top of the
phase-0 host capability.

**Visibility changes (the first diff).** The helpers the plugin calls in
process are module-private in `api.rs` today; widen to `pub(crate)`:
`login`, `get_counters`, `get_labels`, `get_headlines`, `get_article`
(`update_article` is already `pub`).

**Skeleton.**

```rust
pub struct FreshApi;

impl Plugin for FreshApi {
    fn name(&self) -> &'static str { "freshapi" }
    fn about(&self) -> About {
        // description verbatim from freshapi/init.php — the PHP
        // Preferences list shows exactly this:
        // "A FreshRSS / Google Reader API Plugin for Tiny-Tiny RSS",
        // author "Eric Pierce", `system` false (per-user, enabled via
        // the owner's `_ENABLED_PLUGINS`)
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_route("/freshapi", &[Method::GET, Method::POST, Method::OPTIONS]);
        host.add_route(
            "/plugins.local/freshapi/api/greader.php", // freshapi compat
            &[Method::GET, Method::POST, Method::OPTIONS],
        );
    }
    async fn handle_route(&self, ctx: &mut HostCtx, req: &RouteRequest<'_>) -> MethodOutput {
        // dispatcher below; every Response carries the CORS headers
    }
}
```

Registered in `pluginhost::all_plugins()` (that alone puts it in the
per-user enable list; `available_plugins.json` is the *upstream
installable* catalog and is untouched).

**Dispatcher (`handle_route`).**

1. `OPTIONS` → 204 + CORS.
2. Empty `path_info` and empty query → 200 `OK` (the health check) + CORS.
3. `accounts/ClientLogin` → `client_login`.
4. `reader/api/0/<rest>` → `authorized_session` (failure → 401) →
   dispatch `<rest>` per the `parse` table (unknown → 400).
5. Anything else → 400 + CORS.

**Auth.**

```rust
struct Authed { uid: i32, sess: Sess, user: String, access_level: i64 }

// POST form body (application/x-www-form-urlencoded): Email, Passwd
async fn client_login(...) -> MethodOutput {
    // Map { "login", "password" } → api::login — the shared policy
    // (app passwords first, OTP, disabled accounts, ENABLE_API_ACCESS).
    // OK  → 200 text "SID=<user>/<session_id>\nLSID=\nAuth=<user>/<session_id>\n"
    //       (session_id read from login's content, api.rs:250)
    // ERR → 401
}

async fn authorized_session(...) -> Result<Authed, MethodOutput> {
    // Authorization: GoogleLogin_auth/<user>/<sid>
    // session::load(pool, key, sid) → sess.replace(sid, data) — the
    // exact adoption path the official API uses for `sid` (api.rs:87).
    // Checks: session exists; username matches the session's user;
    //   account not disabled; ENABLE_API_ACCESS; per-user gate:
    //   "freshapi" in the owner's _ENABLED_PLUGINS.
}
```

Read-only users (`access_level = 1000`): the write endpoints
(`edit-tag`, `subscription/*` writes, `rename-tag`, `disable-tag`,
`mark-all-as-read`) → 403 `Forbidden!`.

**Id codec.**

```rust
const ID_PREFIX: &str = "tag:google.com,2005:reader/item/";
fn id_to_wire(id: i32) -> String { format!("{ID_PREFIX}{id:x}") }

fn wire_to_id(wire: &str, is_nnn: bool) -> Option<i32> {
    let base = wire.rsplit('/').next().unwrap_or(wire); // basename()
    let digits = !base.is_empty() && base.bytes().all(|b| b.is_ascii_digit());
    if digits && !base.starts_with('0') && !is_nnn {
        base.parse().ok()
    } else {
        i32::from_str_radix(base, 16).ok() // hex2dec path
    }
}
// is_nnn = User-Agent starts with "NetNewsWire" (first 11 chars)
```

**`stream_contents` — scope + format.**

```rust
enum Scope { Feed(i32), ReadingList, Starred, Label(String) }
fn parse_scope(path: &str) -> Option<Scope> // the parse table; bare → ReadingList
```

- Fetch: the `get_headlines`-shape query (entries + feed + labels +
  enclosures, scoped to the owner), filtered per scope: `ReadingList`
  → `unread = 1`; `Starred` → `marked = 1`; `Label` →
  `labels::find_id` by caption then entries carrying it; `Feed` → the
  feed's entries. Per-scope filter details are verified against
  freshapi's `streamScope` at implementation (parity is at the data
  level; the SQL is ours).
- Knobs (match freshapi's actual behavior — deferred decision): `n`
  clamped 1..=10000 (default 20), `ot`/`nt` as `published` bounds,
  `r` = `d` (newest) / `o` (oldest).
- Format: `convert_to_freshapi(row) -> Value` per the "Item / id
  encoding" table. `stream_items_ids`/`stream_items` reuse the same
  scope parse for `s=` and the same formatter over `i=` ids.

**`edit_tag`** (`i=`, `a=`, `r=` — comma-separated lists):

- `starred` in `a`/`r` → `api::update_article` (`field=0`, `mode=1/0`)
- `read` in `r` → `field=2, mode=0`; in `a` (reading-list) → `mode=1`
- label in `a` → `labels::create`-if-missing + `labels::add_article`
- label in `r` → `labels::remove_article`
- → 200 text `OK`

**The small read endpoints.**

- `unread_count` → `api::get_counters` + label counts →
  `{ max: <global-unread>, unreadcounts: […] }` (reading-list,
  starred, each label, each feed — shape per freshapi).
- `tag_list` → `labels::get_all` →
  `{ tags: [ { id: "user/-/label/<caption>", type: "user" } ] }`.
- `user_info` → `{ userId: uid, userName: <user>, userProfileId: uid,
  userEmail: "" }`.
- `token` → 57-char digest derived from the session (clients echo it
  back as `T=`; freshapi never verifies it).

All handlers return `MethodOutput::Response` with the content type per
response (text/plain for `SID=`/`OK`/token, application/json, text/xml
for OPML) and the CORS `headers`.

### Phase 2 sketch

The management endpoints, same plugin. All the write ones sit behind the
phase-1 read-only gate (`access_level = 1000` → 403).

**`subscription/list`** (`?output=json`, read):

- Data: per-owner feeds + categories — a new small query in the shape of
  `opml::build_opml`'s (title, `rss_url`, `site_url`, category names).
- Wire: `{ feeds: [ { id: "feed/<id>", title,
  subscription: { id, title, url: <rss_url>, htmlUrl: <site_url> },
  categories: ["user/-/label/<cat>"] } ] }` (shape per freshapi).

**`subscription/edit`** (write; `ac=subscribe|unsubscribe|edit`, `s=`,
`t=`, `a=`, `r=`):

- `subscribe`: `s` = feed URL → `feeds::subscribe` (the low-level
  signature: `cfg, pool, plugins, self_url, uid, access_level, url,
  cat_id, auth…, update_interval, immediate_update=false` — all from
  `ctx`/`Authed`; category from `a` via `cats` lookup when given).
- `unsubscribe`: `s` = `feed/<id>` → `feeds::remove_feed(state, id,
  uid, self_url)`.
- `edit`: `s` = `feed/<id>`, `t` = new title → title update (a direct
  small query; no existing single-title-edit helper).
- Response: JSON status (match freshapi's exact envelope at
  implementation).

**`subscription/quickadd`** (write; `quickadd=<url>`):
`feeds::subscribe` → `{ status: "success", id: "feed/<new_id>" }`.

**`subscription/export`** (read): `opml::export(state, &sess, extra
{ include_settings: "0" })` → its `(status, body, _filename,
content_type)` folded into the `MethodOutput::Response` (text/xml).

**`subscription/import`** (write; POST, raw OPML body):
`opml::import(state, &sess, Some(req.body))` → its result text as the
response (freshapi writes the body to a temp file first; ours takes the
bytes directly — the documented deviation).

**`rename-tag`** (write; `s=user/-/label/<old>`, `dest=user/-/label/<new>`):
`labels::find_id` (old caption) → `labels::edit` (new caption).
JSON result.

**`disable-tag`** (write; `s=user/-/label/<name>`):
`labels::find_id` → `labels::remove` (drops the label and detaches
articles). JSON result.

**`mark-all-as-read`** (write; `s=<scope id>`, `ts=<unix ts>`):
parse the scope like `stream_contents` (feed id / reading-list /
label) → `feeds::catchup_scope` scoped to that target, with the `ts`
bound (exact mode string — the catchup scope accepts named modes and a
timestamp form — verified at implementation). JSON result.

No new host capability, no new dependencies beyond phase 1. The phase-1
`parse_scope` and read-only gate are reused unchanged.

## Decisions to revisit

- **`stream/contents` full-fidelity knobs** (`it`/`xt` filters,
  `ot`/`nt` time ranges, `c` continuation) — **settled (2026-10-26):
  match-freshapi — superseded for `ot` on 2026-10-27: follow the
  greader protocol.** The reference computes `min_date` from `ot` and
  never uses it (`streamContents` passes only `limit`/`skip`/`view_mode`/
  `feed_id`/`order_by` to `getHeadlines` — the API boundary never exposed
  `start_ts`; a regression from freshapi's pre-`getHeadlines` rewrite,
  which filtered on it). The protocol (mihaip spec: "the timestamp of the
  oldest item that you're interested in"; Arowser: "only items crawled
  after this time"; the 2009 Google spec's delta-sync pattern) requires
  honoring it, and clients rely on it: News Flash sends `ot=<last sync>`
  on every page, so ignoring it re-downloads the entire unread stream on
  every sync. **Deviation: the port honors `ot`** — `start_ts` threaded
  through the `getHeadlines` boundary into the already-ported
  `date_entered >=` filter (crawl time, matching the reference's own
  `streamItemsIds` `ot` convention). `nt` stays read-and-dropped like the
  reference (no `end_ts` in the headline query; no known client sends
  it).
- **NetNewsWire UA quirk** (hex-decoding decimal ids) — **settled
  (2026-10-26): keep.** Landed in phase 1 (`stream/items/contents`,
  `edit-tag`): the `substr(ua, 0, 11) == "NetNewsWire"` prefix check +
  `hex2dec` of decimal ids, verbatim.

Settled: the freshapi-compat path **stays** (the odd URL is the
drop-in story — see Mount point).
