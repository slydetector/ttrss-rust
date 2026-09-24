# M10 — Internationalization (i18n)

**Goal:** The Rust port translates all user-facing strings (shell HTML, API responses, plugin strings, CLI output) using the same gettext `.mo` catalogs the PHP reference uses. A user who had a non-English locale in PHP gets the same translated UI under Rust.

**Exit criteria:**
- Shell HTML, API JSON responses, and plugin-rendered strings are translated per the user's locale.
- The `init` RPC `translations` field is populated (the frontend JS `App._translations` works).
- Locale selection matches PHP: `USER_LANGUAGE` pref wins; fallback to `Accept-Language` header; fallback to English.
- At least 5 locales verified end-to-end (shell + a few API strings + one plugin string).
- `T_sprintf` / `T_nsprintf` equivalents produce translated, formatted strings.

---

## How it works in PHP (reference)

| Layer | Mechanism |
|-------|-----------|
| **Catalog files** | `locale/<locale>/LC_MESSAGES/messages.mo` (~30 locales, GNU gettext format) |
| **Locale selection** | `startup_gettext()`: (1) parse `Accept-Language` header (q-factors, full-locale-then-2-letter match against available locales), (2) `Prefs::get(USER_LANGUAGE)` overrides if set and not `"auto"` |
| **Server-side `__()`** | PHP-gettext (`lib/gettext/`): loads the `.mo` for the selected locale, `__("string")` → translated string or the original (English) if no translation |
| **Server-side `T_sprintf()`** | `vsprintf(__(format), args)` — translate the format string, then format |
| **Server-side `T_nsprintf()`** | `vsprintf(_ngettext(singular, plural, n), args)` — plural-aware |
| **Frontend JS `__()`** | `App.l10n.__(msg)` looks up `App._translations[msg]` — a flat `{original: translated}` map |
| **Frontend translation source** | The `init` RPC response includes a `translations` field: `RPC::_translations_as_array()` iterates the loaded `.mo` catalog and returns every entry as `{original_string: translated_string}` (including plural forms as separate keys) |
| **Plugin JS strings** | Plugin `get_js()` output contains `__("...")` calls; these are translated client-side via `App._translations` (the same map covers plugin strings since they're in the same `.mo`) |

---

## Rust approach

**Crate:** `gettext` v0.4.0 (pure Rust, 964 SLoC, 1.1M downloads, MIT/Apache). Parses `.mo` files, provides `Catalog::parse()`, `catalog.gettext()`, `catalog.ngettext()`, `catalog.pgettext()`. `Send + Sync`. No C dependency, no LGPL.

**Architecture:**

```
┌─────────────────────────────────────────────────────────┐
│  AppState                                               │
│  ┌───────────────────────────────────────────────────┐  │
│  │  i18n: I18nCache                                  │  │
│  │  ┌─────────────────────────────────────────────┐  │  │
│  │  │ HashMap<locale, Arc<Catalog>>               │  │  │
│  │  │   "de_DE" → Arc<Catalog> (parsed .mo)       │  │  │
│  │  │   "fr_FR" → Arc<Catalog>                    │  │  │
│  │  │   ...                                       │  │  │
│  │  └─────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

- **Load once at startup** (or lazily on first use per locale): parse each `.mo` file from `docroot/locale/<locale>/LC_MESSAGES/messages.mo` into a `Catalog`.
- **Per-request locale resolution:** `USER_LANGUAGE` pref → if set and not `"auto"`, use it. Else parse `Accept-Language` header (same algorithm as PHP: q-factors, full-locale-then-2-letter). Else English (identity).
- **Translation context (task-local):** the resolved locale + the shared cache are stored once per request in a `tokio::task_local!` (`I18nCtx { locale, cache }`), set at each entry point after the locale is resolved. Call sites never thread `(locale, cache)` — they use the bare helpers below, which read the task-local (English/identity fallback when not scoped). This is the per-request analog of PHP's request-global gettext domain; Rust is multi-threaded, so it can't be a process-global (`setlocale`) like `gettext-rs`/`rust-i18n` use for CLIs.
- **`__()` in Rust:** bare helpers that read the task-local context and translate: `__(msg)`, `t_sprintf(fmt, args)`, `t_nsprintf(singular, plural, n, args)`. The minijinja `__` is the same `__` function. A pure `vsprintf(fmt, args)` formats without translating. The low-level primitives (`translate`, `translate_sprintf`) take `(locale, cache, …)` explicitly and are internal (unit-tested).
- **`translations` RPC field:** iterate the resolved catalog's entries and build the flat map (same as PHP's `_translations_as_array`).

**What gets translated:**

| Surface | How |
|---------|-----|
| Shell HTML (`shell.html.tpl`) | Template strings wrapped in `{{ __("...") }}` (minijinja) or post-processed. The locale is resolved per-request before rendering. |
| Login page / OTP / forgot-password | Same template mechanism. |
| API JSON responses (error messages, `translations` field) | `__()` calls at the point of string construction. |
| Plugin-rendered strings (e.g., `hook_prefs_tab` HTML, `hook_article_button` titles) | The plugin receives the locale/catalog via `HostCtx` and calls `__()` (or the string is post-processed). |
| Plugin JS strings (`get_js` output containing `__("...")`) | **Not translated server-side** — the frontend JS `App._translations` map handles them (same as PHP). The `translations` RPC field must include these strings. |
| CLI output (`update.php` port) | `__()` with a locale from `TTRSS_I18N_LOCALE` env or system locale. Lower priority. |
| Error messages (DB errors, 404s, etc.) | `__()` where practical. |

---

## Progress

| Increment | Status | Notes |
|-----------|--------|-------|
| M10.1 | ✅ Done (`6cfdb93`, `65f8fce`) | `I18nCache` (lazy `.mo`), `resolve_locale`, `resolve_request_locale`, `__`, `t_sprintf`. Wired into `AppState`. |
| M10.2 | ✅ Done | Shell template → minijinja with `__()` ✅. `t_nsprintf` ✅. Login/OTP/forgotpass templates ✅. **`translations` RPC `init` field populated** — it was an empty `Value::Array` placeholder since M2 (the field was *present*, never *populated*, so SPA client-side i18n via `App._translations` was dead): now resolves the locale (profile-aware `USER_LANGUAGE` + `Accept-Language`) and returns the original→translated object (PHP `RPC.php:198`). Guard: `tests/rpc_init_translations.rs`. |
| M10.2b | ✅ Complete | **Translation-context re-architecture** (2026-09-26): moved from the explicit `(locale, i18n)` pass to the canonical task-local + bare `__()` design (increment below). Steps 1–4 behavior-identical (no new strings, all tests green); step 5 is the small deliberate change that localizes the 7 identity-formatter gaps PHP translates via `T_sprintf`/`_ngettext`. Prerequisite for the rest of M10.3. |
| M10.3 | 🔨 In progress | The interactive `view` path, `hotkeyhelp`, the search-syntax error, and the machine API + syndication already carry the resolved locale (explicit pass, since 2026-09-23; `tests/rpc_init_translations.rs::test_view_feed_not_found_translated`). M10.2b re-lands this on the task-local design, then the remaining ~400 UI/API strings get bare `__()` wrappers. (The stray `getconfig` `translations` field — added in M10.2 in the wrong endpoint, where PHP sends none — was removed 2026-09-23; `tests/api_getconfig.rs` now pins its absence.) |
| M10.4 | ⬜ Pending | Plugin strings via `HostCtx`. |
| M10.5 | ⬜ Deferred to M8 | CLI strings. |
| M10.6 | ⬜ Pending | Verification & parity (5 locales end-to-end). |

## Increments

### M10.1 — i18n substrate (catalog loading + locale resolution)

**Scope:**
- Add `gettext` v0.4.0 dependency.
- `I18nCache` type: `HashMap<String, Arc<Catalog>>`, loaded from `docroot/locale/*/LC_MESSAGES/messages.mo` at startup (or lazily).
- `resolve_locale(accept_language: Option<&str>, user_pref: Option<&str>) -> Option<&str>` — the PHP `startup_gettext()` algorithm:
  1. If `user_pref` is set and not `"auto"` → use it.
  2. Else parse `Accept-Language` (q-factors, full-locale-then-2-letter match against available locales).
  3. Else `None` (English / identity).
- Wire into `AppState` (the cache) and the session/request context (the resolved locale per request).
- The `USER_LANGUAGE` pref is already read from `ttrss_user_prefs2` — just add it to the locale resolution.

**Test:** Unit tests for `resolve_locale` (q-factor sorting, full-locale match, 2-letter fallback, `"auto"` pref, no header). Catalog loading test (parse a real `.mo`, verify a known string translates).

**Size:** ~150-200 lines + tests.

---

### M10.2 — `__()` / `T_sprintf` / `T_nsprintf` in the render path

**Scope:**
- `i18n::translate(locale: Option<&str>, cache: &I18nCache, msg: &str) -> String` — the Rust `__()`.
- `i18n::t_sprintf(locale, cache, fmt: &str, args: &[&str]) -> String` — translate + format.
- `i18n::t_nsprintf(locale, cache, singular: &str, plural: &str, n: i64, args: &[&str]) -> String`.
- **Shell template:** Converted to minijinja. Translatable strings use `{{ __("…") }}` (a function registered on the per-request environment, capturing the locale + `&Arc<I18nCache>`). Dynamic values use `{{ variable }}`. JS strings that need JSON encoding use `| tojson` filter. Environment is created per-request (two HashMap inserts: `__` function + `tojson` filter). NOTE: revisiting caching after first pass — the `| t(locale)` filter approach (cached env + context var) was tried but the `__("…")` syntax is preferred for readability/PHP parity.
- **Login / OTP / forgot-password templates:** ✅ Done. Same minijinja + `__()` treatment. Each render function creates a per-request `Environment` (one `add_function` insert). Locale resolution: login page uses `Accept-Language` header (no session pref for unauthenticated users); OTP and forgotpass use `None` (locale is resolved at the handler level and passed through, or defaults to English when no session exists).
- **The `translations` RPC field:** In the `init` RPC response, build the flat map from the resolved catalog (same as PHP's `_translations_as_array`). This is what populates `App._translations` in the frontend JS — it covers all the JS-side `__()` calls (including plugin JS strings).

**Test:** Render the shell with locale `de_DE` and assert a known string is translated (e.g., "Loading, please wait..." → German). Verify the `translations` RPC field is non-empty for a non-English locale and empty (or identity) for English/no locale.

**Size:** ~200-300 lines (mostly the template string substitutions) + tests.

---

### M10.2b — Translation-context re-architecture (task-local + bare `__()`)

**Why:** M10.2/M10.3 threaded the resolved `(locale, i18n)` pair into every rendering function. That works, but ~576 plain-translate call sites each carry two parameters they don't need. The canonical design keeps the catalog shared on `AppState`, stores the *per-request* locale in a `tokio::task_local!`, and exposes bare helpers that read it — call sites become `__("Special")`, not `__(locale, i18n, "Special")`. This increment re-lands the existing threading on that design. **Behavior-identical:** no new strings are translated, no output changes, all current tests stay green.

**The design (canonical target):**
```rust
// i18n.rs
pub struct I18nCtx { pub locale: Option<String>, pub cache: Arc<I18nCache> }
tokio::task_local! { static I18N: I18nCtx; }

// public bare helpers — the ONLY thing call sites use; read I18N, English fallback when not scoped
pub fn __(msg: &str) -> String
pub fn t_sprintf(fmt: &str, args: &[&str]) -> String
pub fn t_nsprintf(singular: &str, plural: &str, n: i64, args: &[&str]) -> String

// internal pure primitives (renamed from the current threaded fns); unit-tested directly
fn translate(locale: Option<&str>, cache: &I18nCache, msg: &str) -> String
fn translate_sprintf(locale: Option<&str>, cache: &I18nCache, fmt: &str, args: &[&str]) -> String

// util.rs — pure format, no i18n (was the identity `util::t_sprintf`)
pub fn vsprintf(fmt: &str, args: &[&str]) -> String
```
- **Naming mirrors PHP** (`__()`/`_()`, `T_sprintf`, `T_nsprintf`/`_ngettext`, `vsprintf`); `__` compiles (single `_` is the reserved wildcard; `__` is a normal identifier). End-state call-site split (counted from the PHP reference): `__` ~576, `t_sprintf` ~49, `t_nsprintf` ~30, `vsprintf` ~5.
- **minijinja:** `env.add_function("__", |msg| i18n::__(&msg))` — the template's `{{ __(… ) }}` *is* the Rust `__`.
- **Scope set-points** (wrap the post-auth handling in `I18N.scope(ctx, async {…})`): `rpc::dispatch`, the `api` handler, `syndication`, and the three `http` handlers (login / shell / otp).

**Inventory (every surface that must move to the bare helpers):**
1. **Substrate (`i18n.rs`)** — add the `I18N` task-local + `I18nCtx` + bare `__`/`t_sprintf`/`t_nsprintf`; rename the current threaded primitives to internal `translate`/`translate_sprintf` (call sites stop using them). Resolution fns (`resolve_request_locale`, `resolve_user_locale`) unchanged.
2. **Entry points (4)** — set the scope: `rpc.rs` `dispatch` (~335), `api.rs` handler (~798), `syndication.rs` (~151), `http.rs` (120 / 149 / 201).
3. **Leaf fns — drop `(locale, i18n)` params, call bare helpers** — `hotkeys::hotkey_help` (265); `headlines::view` (68), `get_headlines_query` (1397), `cat_title` (1964), `format_headlines_list` (~738/845); `search::search_syntax_error` (~317); plus every `crate::i18n::__` / `t_sprintf` call (headlines 163/572/707/738/785/790/795/802/808/845/852/1671/1716/1972–1974; hotkeys 281/289/295; search 328).
4. **Render path** — `render.rs` `render_shell` (111), `render_login_page` (174), the third (239) + `forgotpass.rs:302`: the `add_function("__", …)` closure becomes `|msg| i18n::__(&msg)`; drop the `(locale, i18n)` params.
5. **Identity-formatter gaps → translating** — verify each against PHP `T_sprintf`; convert the user-facing ones to `t_sprintf(…)`, rename the pure formatter `util::t_sprintf` → `util::vsprintf`: `pref_ui.rs:571`, `system.rs:139`, `filters.rs:66/115/133/217/253`.
6. **Tests** — `render.rs` tests (317–460) → `#[tokio::test]` + `I18N.scope(…)`; any test passing `(locale, i18n)` to a leaf fn → drop the args; `i18n.rs` primitive tests unchanged (they test `translate`/`resolve_*`); integration tests unchanged (scope set at the entry).
7. **`HostCtx`** — unchanged (no locale field); M10.4 plugins call bare `__()`.

**Verification:** behavior-identical — the whole existing suite (lib + integration) stays green; the de_DE regression tests (`test_view_feed_not_found_translated`, the shell/render `de_DE` tests) still assert the same translated output; the integration tests pass because the scope is set at each entry.

**Size:** ~300–450 lines (mostly mechanical param/thread removal + the new substrate fns) + test updates.

**Sequencing:** lands before continuing M10.3 — after it, translating the remaining ~400 strings is just wrapping literals in bare `__(…)`, no threading.

**Status:** ✅ Complete (2026-09-26). Executed as six green checkpoints (each compiles + the suite stays green):
1. **Substrate** — the `I18N` task-local + `I18nCtx` + bare `__`/`t_sprintf`/`t_nsprintf`; the threaded primitives demoted to internal `translate`/`translate_sprintf`/`translate_nsprintf`.
2. **Entry points** — set the scope (post-auth) in `rpc::dispatch`, the `api` handler, `syndication`, and the three `http` handlers.
3. **Leaf fns** — `hotkeys::hotkey_help`, `search::search_syntax_error` (via `dry_run`), and all of `headlines` (`view`, `get_headlines`, `get_headlines_query`, `format_headlines_list`, `cat_title`) drop their `locale`/`i18n` params and call the bare helpers; callers (rpc/api/syndication) + the `get_headlines_query` integration tests updated (`common::empty_i18n` helper dropped).
4. **Render path** — the four template `__` closures (`render_shell`/`render_login_page`/`render_otp_challenge` + forgotpass's local `render_shell`) capture nothing and read `I18N`; the two `de_DE` render tests become `#[tokio::test]` + `I18N.scope(I18nCtx{…})`.
5. **Identity-formatter gaps → translating** — the 7 sites (`pref_ui`, `system`, `filters`) now call `crate::i18n::t_sprintf`/`t_nsprintf` (verified 1:1 against PHP `T_sprintf`/`_ngettext`). *This is the one deliberate behavior change* in M10.2b: these strings localize under a non-English locale.
6. **`vsprintf` rename** — the identity `util::t_sprintf` → `util::vsprintf` (core → `fmt_args`, the unused identity `util::t_nsprintf` dropped, the i18n English-fallback sites + tests follow).

**Verification:** lib (636) + a broad integration sweep (shell_locale, headlines_parity, search_scope, rpc_init_translations, prefs_parity, pref_system, api_getheadlines_sandbox, filter_read, article_handler_parity — 81 tests) all green; clippy adds no new warnings (the 15 flagged patterns predate M10.2b).

---

### M10.3 — API / remaining UI strings (post-M10.2b)

**Status:** planned (M10.2b is done — the re-architecture is in place; this is the remaining string work).

**Why it's now trivial:** M10.2b removed the `(locale, i18n)` threading. The locale lives in the `I18N` task-local (set once at the 4 entry points), so translating a string is a one-line wrap of the literal — **no signature changes, no threading**: `"…"` → `__("…")`, formatted → `t_sprintf("…", &[…])`, plural → `t_nsprintf("…", "…", n, &[…])`.

**Scope:**
- Wrap the remaining user-facing string literals in the bare helpers wherever the PHP reference wraps them in `__()`/`T_sprintf()`/`T_nsprintf()`: the RPC/API error + status strings, and the prefs/users/updates/etc. surfaces.
- **Already pulled into M10.2b** (so *not* part of this increment): the headlines path (feed titles, the "Search results: %s" feed title, "Archived articles", the empty-state messages, "Imported at", "Feeds last updated at", the feed-error link), the filter descriptions + `Unknown action` + `(%d action(s))`, `Page %d of %d`, `Current server time`, hotkey help, the search-syntax error, and the render templates (shell/login/otp/forgotpass).
- **Target shape** (counted from the PHP reference): ~576 `__`, ~49 `t_sprintf`, ~30 `t_nsprintf` (+ ~5 pure `vsprintf`).
- No `HostCtx` change is needed for the non-plugin strings (the task-local is already set at the entry); plugins (M10.4) read the locale via `HostCtx`.

**How to audit:** for each module, read the corresponding PHP file's `__()`/`T_sprintf()`/`T_nsprintf()` calls and wrap the matching Rust literal in the same bare helper. Work module-by-module, one green increment at a time (verify each wrap against the PHP call, not by guessing).

**Test:** per surface, assert a known string matches the PHP `.mo` translation for a fixed locale (de_DE) — `shell_locale.rs` / `rpc_init_translations.rs` are the template. English/no-locale output is unchanged, so the existing suite stays green throughout.

**Size:** ~100–200 lines (scattered `__()` wrappers) + a few locale tests.

---

### M10.4 — Plugin strings

**Scope:**
- Plugins that emit user-facing HTML (e.g., `hook_prefs_tab`, `hook_article_button` titles, `hook_article_rendered_cdm`) call `__()` via the `HostCtx`.
- Plugin JS strings (in `get_js()` output) are **not** translated server-side — they're covered by the `translations` RPC map (the frontend `App._translations` handles them). Verify the `translations` map includes plugin strings (it should, since they're in the same `.mo`).
- The `HostCtx` carries the locale + a reference to the `I18nCache` (or a closure `Box<dyn Fn(&str) -> String>`).

**Test:** Render a plugin's prefs tab HTML with a non-English locale and assert the strings are translated.

**Size:** ~50-100 lines (HostCtx field + plugin call sites) + tests.

---

### M10.5 — CLI strings (low priority)

**Scope:**
- The CLI (`update.php` port, M8) uses `__()` for its output messages.
- Locale source: `TTRSS_I18N_LOCALE` env var, or `LC_ALL`/`LANG` system env, or English.
- Lower priority — most CLI output is operational (not user-facing in the browser).

**Size:** ~50 lines. Can be folded into M8 if desired.

---

### M10.6 — Verification & parity

**Scope:**
- End-to-end test: set `USER_LANGUAGE` pref to `de_DE`, load the shell, verify the `lang` attribute, translated toolbar strings, and the `translations` RPC field.
- Cross-locale test: for 5+ locales, verify a sample of strings (shell + API + plugin) match the PHP translations (extracted from the `.mo` files).
- Edge cases: locale not available (fallback to English), partial translations (missing strings fall back to the original), plural forms (e.g., "1 article" / "N articles").
- The `get_translations()` list (the prefs dropdown) must match the PHP's available locales (the keys of the `get_translations()` array).

**Size:** ~100-150 lines of tests.

---

## Decisions (resolved)

| # | Decision | Resolution |
|---|----------|------------|
| 1 | **Shell template mechanism** | Keep `str::replace`. Pre-translate per-locale. No minijinja conversion. |
| 2 | **When to load `.mo` files** | **Lazily on first use per locale.** An `I18nCache` with per-locale `OnceLock` entries; the first request for a given locale triggers the `.mo` parse, subsequent requests reuse the cached `Arc<Catalog>`. |
| 3 | **`HostCtx` i18n access** | `locale: Option<String>` + `i18n_cache: &I18nCache` fields. Plugins call `__()` directly. |
| 4 | **`Accept-Language` parsing** | Use the `accept-language` crate. Match the PHP's full-locale-then-2-letter matching logic on top. |
| 5 | **Scope of translated strings in the shell** | All ~100-150 user-visible strings. Full parity with PHP. |
| 6 | **The `translations` RPC field** | Full catalog (all entries in the `.mo`). Match PHP exactly. |
| 7 | **CLI i18n** | Defer to M8. Operational output, not user-facing. |
| 8 | **Plural forms in the `translations` map** | Include both singular and plural (match PHP). The frontend `ngettext()` handles selection. |
| 9 | **Rust function name** | `__()`. Matches the PHP reference and the frozen frontend JS. (See #11 for the full bare-helper scheme.) |
| 10 | **Translation-context mechanism** | `tokio::task_local!` holding `I18nCtx { locale, cache }`, set once per request at each entry point (post-auth); call sites use the bare helpers, which read it (English/identity fallback when not scoped). Supersedes the earlier explicit `(locale, i18n)` pass (the "axum-ecosystem norm" noted in plan.md). Rationale: PHP's gettext keeps the domain in a request-global; Rust is multi-threaded, so the per-request analog is a task-local (a process-global `setlocale` à la `gettext-rs`/`rust-i18n` would race across concurrent requests). |
| 11 | **Bare-helper naming scheme** | `__()` / `t_sprintf()` / `t_nsprintf()` (translating, read the task-local) + `vsprintf()` (pure format, no i18n) — mirrors PHP's function families (`__()`/`_()`, `T_sprintf`, `T_nsprintf`/`_ngettext`, `vsprintf`). `__` compiles (single `_` is the reserved wildcard; `__` is a normal identifier). The `(locale, cache, …)` primitives are demoted to internal `translate` / `translate_sprintf`; the identity `util::t_sprintf` becomes `util::vsprintf`. |
---

## What's NOT in M10

- **Generating `.po`/`.mo` files from the Rust source** — the translations are maintained by the community and shipped in the frozen `tt-rss/locale/` directory. The Rust port *consumes* them, not produces them.
- **Right-to-left (RTL) layout** — the frontend CSS handles that; the server just needs to set the `lang` attribute (the shell already does this in PHP via `<html lang="...">`).
- **Locale-specific number/date formatting** — the PHP uses `date()` / `number_format()` which are locale-aware; the Rust port uses `chrono` with explicit formats. Full ICU-level locale formatting is out of scope (the date formats are hardcoded in the port, matching PHP's `Config::DATE_FORMAT` etc.).
- **Adding new translations** — that's a community / maintainer task, not a port task.

---

## Estimated total size

| Increment | Lines (code + tests) |
|-----------|---------------------|
| M10.1 | ~200 |
| M10.2 | ~300 |
| M10.2b | ~350 |
| M10.3 | ~150 |
| M10.4 | ~75 |
| M10.5 | ~50 (deferred to M8) |
| M10.6 | ~125 |
| **Total** | **~1,350** |
