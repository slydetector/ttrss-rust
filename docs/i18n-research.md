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
- **`__()` in Rust:** a function/macro that takes the resolved locale + catalog and returns the translated string. For the shell template (rendered per-request), the locale is known at render time. For API responses, same.
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
| M10.2 | 🔄 In progress | `translations` RPC field ✅. Shell template → minijinja with `__()` ✅. `t_nsprintf` ✅. Login/OTP/forgotpass templates ✅ (minijinja + `__()`). |
| M10.3 | ⬜ Pending | API response string audit + `__()` wrappers. |
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

### M10.3 — API response strings

**Scope:**
- Audit all user-facing strings in API/RPC responses (error messages, status strings, the `get_headlines` `feed_title` for search results, filter descriptions, etc.).
- Wrap them in `__()` where the PHP reference does.
- The `HostCtx` gains a `locale: Option<String>` field (or the cache is accessible) so plugins can call `__()`.

**Test:** For a few known translated strings in API responses (e.g., the search-results feed title "Search results: %s"), verify the Rust response matches the PHP translation for a given locale.

**Size:** ~100-200 lines (scattered `__()` wrappers) + tests.

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
| 9 | **Rust function name** | `__()`. Matches the PHP reference and the frozen frontend JS. A module-level `i18n::__()` function. |
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
| M10.3 | ~150 |
| M10.4 | ~75 |
| M10.5 | ~50 (deferred to M8) |
| M10.6 | ~125 |
| **Total** | **~1,000** |
