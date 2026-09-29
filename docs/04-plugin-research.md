# TT-RSS plugin system — research

Research for the M7 plugin port. Goal: decide how plugins work in the Rust
port (a compiled, long-lived server) versus the frozen PHP reference
(interpreted, per-request), and enumerate the architectural decisions that must
be settled for **runtime parity** — i.e. the app behaves the same with the same
plugins active.

**Scope note.** Per `docs/01-plan.md`, community PHP plugins are *not* run under
Rust; we port a set of first-party plugins to Rust and expose a Rust plugin
API. That scoping materially relaxes the "compiled vs interpreted" problem
(§6): we control the plugins, so no general-purpose sandbox or foreign
interpreter is required. The open question is whether plugins are **compiled
into the host** or **loaded as artifacts at runtime** — which only matters if
we keep the git-based installer that drops in plugins without rebuilding.

---

## 1. The core question

PHP is interpreted: a plugin is a directory of `.php` files, loaded at request
boot with `require_once` + `new $class()` + `->init($host)`. Loading, enabling,
and re-loading per user is trivial — the interpreter does it for free.

Rust is compiled: there is no way to take an arbitrary source file and "run"
it. A "plugin" must be either (a) code already linked into the binary, or (b) a
compiled artifact (`.so`/`.dylib`/`.dll`, or a `.wasm` module) that the host
loads and calls into at runtime through a stable boundary. Everything else —
the hook system, storage, per-user enablement, frontend assets — is portable
regardless of the loading mechanism, because it's just *data + a contract*.

So the architecture splits into two independent decisions:

1. **The contract** — the plugin/host API surface (what a plugin *can do*, and
   what the host *offers*). This is loading-model-agnostic and is the right
   thing to pin down first.
2. **The loading mechanism** — how the plugin's compiled code reaches the host
   (static link vs `.so` vs WASM vs separate process). This can be decided
   later, and the answer depends on whether runtime install is required.

---

## 2. The parity target: what the PHP system actually does

Everything below is from the frozen reference (`classes/PluginHost.php`,
`classes/Plugin.php`, `classes/PluginHandler.php`, `plugins/`). The Rust port
must reproduce this *observable* behavior, not the PHP mechanics.

### 2.1 The `Plugin` base class (the plugin-side contract)

A plugin is a `Plugin` subclass providing:

- `init($host)` — called once at load with the `PluginHost`; the plugin
  registers its hooks, handlers, commands, special feeds, filter actions,
  scheduled tasks here.
- `about()` — metadata: `[version, description, author, is_system?]`.
  `about()[3] === true` marks a **system** plugin (`PluginHost::is_system`).
  The plugin **name** is the lowercased class name (e.g. class `share` →
  name `share`), *not* taken from `about()`.
- Optional capability hooks `get_js()`, `get_css()`, `get_prefs_js()`,
  `get_prefs_css()` — return JS/CSS the host injects into pages.
- `flags()` (e.g. `needs_curl`), `is_public_method($m)`, `csrf_ignore($m)`,
  `api_version()`.
- **~50 hook method stubs** — `hook_article_button`, `hook_article_filter`,
  `hook_feed_parsed`, `hook_fetch_feed`, `hook_search`, `hook_sanitize`,
  `hook_render_article*`, `hook_hotkey_*`, `hook_auth_user`,
  `hook_house_keeping`, `hook_filter_triggered`, `hook_*_toolbar_button`,
  `hook_get_full_text`, `hook_articles_mark_toggled`, etc. A host calls these
  only if the plugin registered that hook type.

### 2.2 The `PluginHost` singleton (the host-side contract)

Maintains registries and provides the services plugins call into:

- **Plugin registry** — `register_plugin`, `get_plugin(name)`,
  `get_plugin_names`.
- **Hook registry** — `add_hook(type, plugin, priority)`, then `run_hooks`
  (fire-and-forget, priority-ordered), `run_hooks_until` (stop at first
  truthy), `run_hooks_callback` / `chain_hooks_callback` (data transforms;
  short-circuit on `false`/`null`). This is the core extension mechanism —
  the host sprinkles ~40 call sites (see §4.11).
- **Handler registry** — `add_handler(handler, method, plugin)` (system
  plugins only) overrides request routing: the `api/`, `public.php`, and
  `backend.php` entrypoints defer to plugin handlers.
- **Command registry** — `add_command` → plugin CLI subcommands for the update
  daemon.
- **Plugin storage** — `ttrss_plugin_storage(name, owner_uid, content)`;
  `set/get/set_array/get_array/get_all/clear_data` + `profile_get/
  profile_set` (per-preference-profile). `content` is PHP `serialize()`-d.
- **Special / virtual feeds** — `add_feed(cat=-1, title, icon, plugin)` /
  `get_feeds(cat)` / `get_feed_handler(pfeed_id)`; negative feed IDs starting
  at `PLUGIN_FEED_BASE_INDEX`; the `IVirtualFeed` interface.
- **API methods** — `add_api_method` (system plugins) extends the external
  API.
- **Filter actions** — `add_filter_action(plugin, name, desc)` → user-
  selectable actions in the filter UI.
- **Scheduled tasks** — `add_scheduled_task` (via `Scheduler`), run during
  housekeeping (system plugins always; user plugins only for active users).
- **`load` / `load_all`** — the loader: reads plugin dirs (`plugins/` +
  `plugins.local/`), `require_once`s `init.php`, instantiates, calls `init`.
  `KIND_SYSTEM` (from `Config::PLUGINS` env, all users), `KIND_USER` (per-user
  `_ENABLED_PLUGINS` pref), `KIND_ALL`.

### 2.3 The `PluginHandler` (backend dispatch into plugins)

`backend.php?op=pluginhandler&plugin=<name>&method=<m>[&...]` → resolve the
plugin, call `$plugin->method()`, with a CSRF check (`validate_csrf` or
`$plugin->csrf_ignore($method)`). This is how the SPA invokes plugin methods.
Return paths: JSON error for unknown plugin/method or bad CSRF; otherwise the
plugin method's output.

### 2.4 Auth is a plugin

`auth_internal` / `auth_remote` are **system plugins** implementing the
auth module (`HOOK_AUTH_USER` → `authenticate(login, password, service)`). The
core's login path calls `chain_hooks_callback(HOOK_AUTH_USER, …)` (via
`UserHelper`). Reaching parity for `auth_remote` means the Rust login path
must be pluggable behind an auth-module trait, not hardwired to the DB.

### 2.5 Frontend assets + i18n

Plugins ship JS/CSS (via `get_js`/`get_css`/…) and gettext `.po` catalogs under
`locale/` (bound with `_bindtextdomain`). The host concatenates active
plugins' assets into the served page and loads their translations.

---

## 3. How Rust projects do dynamic plugins (the survey)

The community has converged on four real mechanisms, plus one escape hatch.
Grounded in current, non-toy projects:

| # | Mechanism | How it works | Representative projects | Trade-offs |
|---|-----------|--------------|-------------------------|-----------|
| 1 | **WASM sandbox** | Plugin compiled to `wasm32-wasip1` `.wasm`; host runs it in `wasmtime`/`wasmer` behind a host-function API. | **Zed editor** (extensions), **extism** (the "embed plugins as WASM" framework), Substrate/Polkadot (on-chain runtime), Fermyon Spin. | **Pro:** stable ABI (WASM is an ABI), portable single artifact, sandboxed (safe even for untrusted code), no host/plugin compile coupling. **Con:** plugins must target WASM (threads/syscalls/direct libc are limited); heavier runtime dependency; the host API is the boundary. |
| 2 | **Native shared lib (`dlopen`)** | Plugin built as a `cdylib` (`.so`/`.dylib`/`.dll`) exporting a C-ABI entry point; host loads with `libloading` and calls into it. | **Bevy** (`bevy_dynamic_plugin`, `bevy_mod_pdk`), **Godot GDExtension** (native addons), `cargo-dynlib` (builds such plugins). | **Pro:** closest to the PHP model, near-zero overhead, full Rust on both sides. **Con:** ABI stability is the hard part (need a stable `extern "C"` boundary; Rust's types/`dyn` don't cross safely across builds); unloading is risky (global state, the CRT); same-trust-domain (no sandbox). |
| 3 | **Embedded interpreter / scripting** | Host embeds a scripting runtime; plugins are *scripts* (Lua via `mlua`, a Rust-native DSL via `rhai`, Python via `pyo3`, or PHP itself via an embed). | Game mod systems, `rhai`/`mlua`/`rhai`-style hosts, Nvim. | **Pro:** closest to PHP's "interpreted, drop in a file" model; genuinely dynamic. **Con:** plugin logic is *not* Rust; you maintain the interpreter and a foreign language; feature/typing ceiling. |
| 4 | **Compile-time registration** | Plugins are crates compiled **into** the binary; each self-registers via a linker-section registry (`inventory`, `linkme`) or an explicit registry list. | Many Rust CLIs/engines use this for "extensible-without-dlopen". | **Pro:** simplest, no dynamic-loading machinery, full type safety, no ABI concerns. **Con:** no runtime install/swap — the set of plugins is fixed at build time (first-party or build-time only). |
| 5 | **Separate process (IPC/RPC)** | Each plugin is its own process; the host talks to it over stdio/Unix-socket/gRPC. | Figma plugins, Electron renderer isolation, LSPs. | **Pro:** strong isolation + crash containment. **Con:** heavyweight; the "plugin" is really a separate program with a protocol; overkill for trusted first-party code. |

**Take-aways for ttrss:**

- Because we are **first-party only**, #3 and #5 are overkill (no untrusted
  code to sandbox; no separate-language need). We keep Rust end to end.
- The realistic choices are **#4 (static)** and, if runtime install is
  required, **#1 (WASM)** or **#2 (native `.so`)**.
- #1 vs #2 hinges on: do we need to **install a prebuilt artifact at runtime
  without rebuilding the host**, and do we need **sandboxing**. If the answer
  to both is "no" (first-party, trusted, ship with the binary), #4 wins on
  simplicity. If we keep the git-installer that pulls arbitrary repos and
  builds/loads them live, we need #1 or #2 — and #1 is the safer bet for a
  stable cross-version artifact ABI.

---

## 4. The architectural concerns to settle (beyond loading)

These are the decisions that make the port *behave* like PHP, independent of
how the plugin code is loaded. Each is marked **[A]** (agreed / already
implied by the current port), **[C]** (contract — pin in the `Plugin`/
`PluginHost` traits), or **[D]** (depends on the loading mechanism / a later
scope decision).

1. **[C] The plugin/host trait boundary.** A `Plugin` trait (≈ the PHP base
   class: `init`, `about`, `get_js/css`, the hook methods, `is_public_method`,
   `csrf_ignore`) and a `PluginHost` trait (≈ the PHP singleton: `add_hook`,
   `set/get` storage, `add_handler`, `add_command`, `add_feed`,
   `add_filter_action`, `add_api_method`, `add_scheduled_task`). This is the
   single most important thing to pin: every ported plugin and every host call
   site compiles against it. Keep it 1:1 with PHP so a ported plugin reads
   almost the same.

2. **[C] Explicit request context (no globals).** PHP plugins reach
   `$_SESSION`, `$_REQUEST`, the DB, and `$_SESSION["profile"]` through
   globals and singletons. Rust has no such globals. Every hook callback and
   every `PluginHost` method that is request-scoped must receive an explicit
   context — e.g. a `HostCtx<'_>` carrying the `&PgPool`, the session
   (`uid`, `profile`, `access_level`), the request params, and the active
   profile. This is the biggest *shape* change from PHP and the main reason a
   naive 1:1 port won't compile.

3. **[C] Lifecycle & load timing.** PHP: plugins load fresh every request
   (`load_all` at boot; `KIND_USER` plugins re-resolved per user). Rust server
   is long-lived: load **system plugins once at startup**; for **user
   plugins**, the *code* can be loaded once but *activation* is gated per
   request by the user's `_ENABLED_PLUGINS` pref. So the registry holds the
   known plugin objects; per-request, the host filters to the set enabled for
   the current `uid`. `init()` runs once (startup), not per request — plugin
   `init` must therefore only *register*, not capture per-request state.

4. **[C] State & instance lifetime.** PHP: each request gets a fresh plugin
   instance (no state leaks across requests). Rust: one long-lived instance per
   plugin shared across requests/users. Two consequences: (a) any per-request
   state a PHP plugin keeps on `$this` must move into the request context or a
   `RefCell`/`RwLock`; (b) per-user state must come from storage (DB), never
   from the shared instance. Design plugins to be stateless across requests.

5. **[C] Concurrency / thread safety.** axum + tokio run requests on multiple
   threads. Hook callbacks and `PluginHost` services can be invoked from
   concurrent tasks. `Plugin` objects and registries must be `Send + Sync`
   (or the host serializes access). The hook registry and storage cache need
   interior mutability (`RwLock`) because `run_hooks` mutates nothing but
   `set/get` do. This is a net-new constraint PHP never had.

6. **[C] Per-user enablement & storage.** `_ENABLED_PLUGINS` (per-user, and
   the profile blacklist interplay seen in `Pref_Prefs`) gates which user
   plugins are active. `ttrss_plugin_storage` is keyed `(owner_uid, name)` and
   content is per-profile for `profile_get/profile_set`. The host's storage
   `get/set` must always scope by the current `uid` from the request context
   — never a global. (Already true of the Rust `prefs`/`_ENABLED_PLUGINS`
   read side in `plugins.rs`.)

7. **[C] Frontend asset injection.** `get_js`/`get_css`/`get_prefs_js`/
   `get_prefs_css` return text; the host concatenates the *active* plugins'
   output and serves it (the SPA already expects per-plugin JS/CSS). This is
   trivial even for statically-linked plugins — assets are just strings, no
   compilation involved.

8. **[D] Storage serialization interop.** `ttrss_plugin_storage.content` holds
   PHP `serialize()`-format bytes (e.g. the `note` plugin stores article notes
   there; `af_*` plugins store config). **If** the Rust port must read a DB
   that PHP wrote (shared/interoperable DB), it needs a PHP `serialize()`
   decoder. If we own the DB (Rust-only deployment), we can store in our own
   format and skip it. Depends on the interop decision.

9. **[D] Localization.** Plugins bundle gettext `.po` under `locale/`; the
   current port's `__()` is identity (no catalog). Plugin i18n is a later
   concern; parity for the *first* pass can assume English.

10. **[C→D] Auth as a plugin (`IAuthModule`).** To port `auth_remote` (and keep
    `auth_internal` uniform), the login path must call an auth-module trait via
    the `HOOK_AUTH_USER` chain rather than hardwiring the DB. `auth_internal`
    can be the default system plugin. (Touches `auth.rs`; a contained refactor.)

11. **[A] The hook call-site inventory.** The host must reproduce ~40 extension
    points. The current port treats all of them as no-ops (empty registry ⇒
    identity/no-op), which is exactly the right *default*: an empty registry
    reproduces the PHP "no plugins enabled" behavior, so no ported host code
    changes are needed until the first plugin is actually wired in. The full map
    of where these fire in the (already-ported) Rust surface is in §5.

---

## 5. Hook call-site map (where extension points live in the ported surface)

From `classes/` — the host methods that call into plugins, grouped by the Rust
module they correspond to. This is the checklist for "which ported code needs a
`run_hooks*` call to reach parity" once a plugin is active:

- **Headlines / article rows** (`Feeds::get_headlines*`, `HeadlinesQuery`):
  `HOOK_QUERY_HEADLINES`, `HOOK_ARTICLE_BUTTON` / `_LEFT_BUTTON`,
  `HOOK_RENDER_ARTICLE[_CDM|_API]`, `HOOK_HEADLINES_BEFORE`,
  `HOOK_HEADLINE_TOOLBAR_BUTTON` / `_SELECT_MENU_ITEM2`,
  `HOOK_HEADLINES_CUSTOM_SORT_MAP` / `_OVERRIDE`, `HOOK_FORMAT_ENCLOSURES`,
  `HOOK_RENDER_ENCLOSURE`, `HOOK_ENCLOSURE_ENTRY`, `HOOK_ARTICLE_IMAGE`,
  `HOOK_GET_FULL_TEXT`, `HOOK_ARTICLE_EXPORT_FEED`, `HOOK_IFRAME_WHITELISTED`.
- **Search** (`Feeds::search`): `HOOK_SEARCH` (may replace the built-in query).
- **Article mutations** (`RPC::markArticleRead`/`…`, `Article`, `RSSUtils`):
  `HOOK_ARTICLES_MARK_TOGGLED`, `HOOK_ARTICLES_PUBLISH_TOGGLED`.
- **Feed update/fetch** (`RSSUtils`): `HOOK_FETCH_FEED`, `HOOK_FEED_FETCHED`,
  `HOOK_FEED_PARSED`, `HOOK_FEED_BASIC_INFO`, `HOOK_ENCLOSURE_IMPORTED`,
  `HOOK_ARTICLE_FILTER`, `HOOK_FILTER_TRIGGERED`, `HOOK_HOUSE_KEEPING`.
- **Subscription** (`Feeds::subscribe*`): `HOOK_PRE_SUBSCRIBE`,
  `HOOK_SUBSCRIBE_FEED`, `HOOK_UNSUBSCRIBE_FEED`.
- **Sanitizer** (`Sanitizer`): `HOOK_SANITIZE`, `HOOK_IFRAME_WHITELISTED`.
- **Session/auth** (`Sessions`, `UserHelper`): `HOOK_VALIDATE_SESSION`,
  `HOOK_AUTH_USER`, `HOOK_POST_LOGOUT`, `HOOK_LOGINFORM_ADDITIONAL_BUTTONS`.
- **UI chrome** (`index.php`, `RPC`): `HOOK_FEED_TREE`,
  `HOOK_MAIN_TOOLBAR_BUTTON`, `HOOK_TOOLBAR_BUTTON`, `HOOK_ACTION_ITEM`,
  `HOOK_HOTKEY_MAP` / `_INFO`.
- **Prefs pages** (`Pref_*`): `HOOK_PREFS_TAB`, `HOOK_PREFS_TAB_SECTION`,
  `HOOK_PREFS_EDIT_FEED`, `HOOK_PREFS_SAVE_FEED`.
- **Filtering** (`Pref_Filters` engine): `HOOK_FILTER_TRIGGERED`, plus
  plugin-provided `filter_actions` (registry, not a call site).

None of these are wired in the Rust port yet (all no-ops). They become live
only when a plugin that registered that hook is active — so they can land
*with* their plugin, not before.

---

## 6. Recommendation for ttrss

**Separate the contract from the loader, and start static.**

1. **Pin the contract first (§4.1–4.2).** Define `Plugin` + `PluginHost` traits
   1:1 with PHP, with an explicit `HostCtx` for request-scoped access. This is
   loading-model-agnostic, unblocks porting individual plugins, and is where
   the real design work is. The empty-registry default already gives correct
   "no plugins" behavior, so this is additive and safe.

2. **Load first-party plugins statically (mechanism #4).** Compile the ported
   plugins into the server; register them via an explicit registry (a
   `Vec<fn() -> Box<dyn Plugin>>` or `inventory`/`linkme`). System plugins
   (`auth_internal`, the `af_*` ones) load at startup; user plugins' *code*
   loads at startup but is *gated per request* by `_ENABLED_PLUGINS`. This
   reaches runtime parity for every first-party plugin with **zero** dynamic-
   loading machinery — no ABI, no WASM runtime, no dlopen, no installer.

3. **Defer the dynamic loader (§3, §7).** Only if we keep the git-installer
   that installs plugins at runtime *without rebuilding the host* do we need
   #1 (WASM — portable, stable ABI, sandboxed; Zed/extism model) or #2 (native
   `.so` — lower overhead, PHP-like; Bevy/Godot model). Prefer **WASM** if we
   ever need to run *prebuilt third-party* plugin artifacts safely; prefer
   **native `.so`** if we only ever load first-party, host-ABI-matching `.so`s.

**Why static is the right default.** The "compiled vs interpreted" gap mostly
evaporates once we accept that plugins are **Rust, written by us, compiled into
(or shipped with) the host**. We don't need an interpreter, a sandbox, or a
foreign ABI to run code we control and ship ourselves. Static linking gives:

- full type safety across the plugin/host boundary (no `extern "C"` shims);
- no loader, no artifact distribution, no version-skew between host and plugin;
- the exact same hook/storage/enablement semantics as PHP, just in-process.

What it *costs* is only one thing: **installing a plugin = rebuilding the
host** (or shipping a prebuilt binary). That's acceptable for first-party
plugins and is the only reason to ever reach for #1/#2.

The **contract** (traits + `HostCtx`) is the durable part; the **loader** is a
swappable detail. By defining the contract against a `Plugin` trait, switching
the loader later from "linked in" to "loaded as `.so`" or "run as WASM" is a
change to the registration code, not to any plugin or host call site.

---

## 7. Open decisions (to revisit as scope firms up)

These are the choices that should be settled explicitly, each gated on a scope
decision rather than on the loading mechanism itself:

| # | Decision | Resolution (settled this session) |
|---|----------|------------------------------------|
| 7.1 | Git-based **plugin installer** (runtime install/update) | **Defer** until forced to have a dynamic loader. Static-first means it's never needed unless we install prebuilt artifacts live. |
| 7.2 | Shared/interoperable DB with PHP | **Wanted (confirmed: read+write)** — the same DB can be served by either app at any time, so the **PHP `serialize()` codec needs both encode and decode**. Interop is already true for all core tables (plain text/ints). The only gap is that codec, needed for `ttrss_plugin_storage` (and the ephemeral session blob); it lands *with the first real plugin*. (Feed `auth_pass` serialize+envelope already ported.) |
| 7.3 | Port `auth_remote` (external auth) or just `auth_internal`? | **`auth_remote` deferred (confirmed)** — external/proxy/cert auth is not a target out of the gate. Only `auth_internal` is needed; it's already the built-in Rust login path, so the `IAuthModule` refactor is also deferred until a plugin actually needs it. |
| 7.4 | Which first-party plugins to port, in what order? | **Out of the gate: `auth_internal` only** (`Config::PLUGINS` default; `_ENABLED_PLUGINS` defaults empty, so no user plugin is on by default). `auth_internal` is already implemented as the built-in login, so the executing plugin set is effectively **empty** → the plugin **substrate is parked at the no-op baseline** until the first real plugin is chosen. All other first-party plugins deferred. |
| 7.5 | Runtime install of **prebuilt third-party** plugins (WASM) | **Defer** — static first; revisit WASM once static is tried and true. |
| 7.6 | Plugin API **versioning** | Mirror the PHP `API_VERSION` constant; low priority for first-party. |

---

## 8. Suggested first increment (when we start the code)

Keep it small and verifiable, matching the established rhythm:

1. **A `crates/ttrss/src/pluginhost/` module** with:
   - `trait Plugin { fn name(&self)->&str; fn about(&self)->PluginAbout; fn init(&self, host:&mut PluginHost); /* + the ~50 hook methods with PHP-matching signatures */ }`
   - `trait PluginHost` (or a concrete `PluginHost` struct) with `add_hook`, `run_hooks*`, and `set/get` storage.
   - `struct HostCtx<'a>` (`&'a PgPool`, session `uid`/`profile`/`access_level`, request params).
   - a `Vec`-backed registry + an empty-by-default host (reproduces "no plugins").
2. **Port one trivial plugin end to end** — `note` (stores a per-article note
   via plugin storage, exposes a per-article UI hook) or `nsfw` (tags-based
   content hiding, a `HOOK_RENDER_ARTICLE`/sanitize hook) — as the proof that
   the contract + a static registration works, verified with a Rust-only DB
   test (per the established "no harness for write ops" rule).
3. **Wire the host call sites for that one plugin's hooks** (the subset from §5
   its plugin actually uses), leaving the rest no-ops.

That gives a working, testable plugin substrate without any dynamic-loading
machinery, and a concrete reference for porting the rest.

---

## 9. Sources

- PHP reference (authoritative): `tt-rss/classes/PluginHost.php`,
  `classes/Plugin.php`, `classes/PluginHandler.php`, `plugins/*/init.php`.
- Rust dynamic-plugin mechanisms (current, non-toy):
  - Zed editor extensions — WASM: https://deepwiki.com/zed-industries/zed/13-extensions-system
  - extism — WASM plugin framework: https://github.com/extism/extism
  - Bevy dynamic plugins (`bevy_dynamic_plugin`, `bevy_mod_pdk`) — native `.so`:
    https://github.com/bevyengine/bevy/discussions/8390 ,
    https://docs.rs/bevy_dynamic_plugin
  - Godot GDExtension — native `.so`: https://docs.godotengine.org/en/4.4/tutorials/scripting/gdextension/what_is_gdextension.html
  - Rust plugin-system trade-off surveys:
    https://www.arroyo.dev/blog/rust-plugin-systems/ ,
    https://kerkour.com/rust-plugin-system , https://blog.anirudha.dev/rust-plugin-system/

---

## 10. Backlog

### Plugin-trait `Value` documentation pass *(closed 2026-09-29)*

Document **every `Value` parameter and `Value` return** in the `Plugin` trait
(`crates/ttrss/src/pluginhost/plugin.rs`) so a plugin author can see the exact
shape and content of each value at the definition site — no wild-goose-chase
into call sites / PHP to work it out.

The format rules are encoded in the trait's doc comment ("Hook
documentation rules", `crates/ttrss/src/pluginhost/plugin.rs`) — follow them,
one line per field, descriptions derived from the dispatch site + frozen PHP.

Scope: the 18 hooks with a `Value` surface — all done:
`hook_article_button`, `hook_article_filter`, `hook_update_task`,
`hook_hotkey_map`, `hook_render_article`, `hook_render_article_cdm`,
`hook_render_article_api`, `hook_hotkey_info`, `hook_article_left_button`,
`hook_prefs_save_feed`, `hook_query_headlines`, `hook_search`,
`hook_format_article`, `hook_send_mail` *(provisional — Mailer unported)*,
`hook_enclosure_imported`, `hook_headlines_custom_sort_map`,
`hook_headlines_custom_sort_override`, `hook_post_logout`.

The dispatch-completion batch also documented the 17 hooks with no `Value`
surface that gained dispatch sites (dispatch site + return semantics per the
same rules). Remaining: 24 non-`Value` hooks (mostly the feed-pipeline and
simple `&str`/`bool`-signature hooks) still carry 2–3-line docs — a
separate, smaller pass if the trait-level documentation is ever taken to
full completeness.
