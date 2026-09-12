# PHP Test Suite Sufficiency + Browser Automation for the ttrss Port

**Date:** 2026-09-07
**Status:** research / planning (no code changes)
**Scope:** *is the frozen PHP test suite itself a sufficient correctness floor, and how
should browser automation — the user's actual UX surface — fit in?* Part A inventories the
PHP suite and its gaps; Part B proposes a lightweight browser-automation layer.

> **Status note (maintained):** the PHP-suite inventory (§2) and the Part B browser design
> are stable. The test-parity figures quoted below are a snapshot as of **2026-09-07**;
> `docs/plan.md` (the parity program + remaining-coverage tables) is the maintained source
> of truth for counts/rows, and since that date M5 (external API + admin) has largely completed.

---

## 1. TL;DR

1. **The PHP suite is a strong *class-level* spec, not a *system-level* proof.** The frozen
   checkout ships **≈1,273 PHPUnit test methods across five suites** plus a 3-test Selenium
   smoke, with real-Postgres integration infrastructure (temp DB, schema migration, seeded
   synthetic data, handler invocation through the real router, live-API tests). It covers the
   backend logic the Rust port re-implements **very deeply** (sanitizer 256, feed parsing ~282,
   helpers ~300, DB-backed behavior ~550) — and the port has already consumed most of it via the
   test-parity program (15/15 rows ported; `docs/plan.md` tracks the maintained count).
2. **What it does *not* cover is exactly what the user judges the port by:** the browser
   (SPA boot, feed tree, counters, article rendering, prefs), the update engine end-to-end (its
   one integration test is *disabled* — file named `UpdateRssFeedIntegrationTest.php~`),
   first-party plugin behavior (the golden DB runs 9 plugins the suite never exercises),
   time-based/daemon behavior, and anything visual. The suite's only browser test is 74 lines of
   Selenium that requires a Selenium Grid.
3. **Verdict on sufficiency:** the PHP suite, fully ported (parity program rows 1–15 ≈ done for
   the ported surface; remaining ~500 tests ride along with M5–M8 modules), gives a solid
   *"the logic is correct"* floor. It cannot establish *"the port is correct"* at the UX level.
   That missing layer is browser automation — and the repo already contains a template for it
   (`tests/integration/selenium_test.py`).
4. **Browser-automation recommendation (lightweight):** **Playwright** (the 2026 default:
   auto-wait, built-in pixel-diff screenshot assertions, a clock API that makes relative-date UI
   deterministic, pinned Docker images that kill rendering drift) driving **headless Chromium**
   against the golden DB. Two small spec files: (a) a 1:1 port of the existing Selenium smoke,
   (b) ~8–12 "golden UI states" — each producing a text/JSON snapshot of key DOM regions plus a
   full-page screenshot with volatile elements masked via injected CSS. **Record the baselines
   once against the PHP oracle, commit them, and assert the Rust server matches** — a UX-level
   blind golden master. Total new surface: a `browser/` directory, ~200–300 lines of
   spec/config, one pinned Docker image. No grid, no SaaS, no framework.

---

## 2. Part A — The PHP test suite, inventoried

### 2.1 Layout (frozen checkout at `2f7d831`)

| Directory | Role | Test methods |
| --- | --- | --- |
| `tests/` | Unit tests (no DB; some mocked) | ~102 |
| `tests/mocked/` | Unit tests with mocked `Config`/`Prefs`/`PluginHost` (no DB at all) | 171 |
| `tests_functional/` | Class-level "functional" tests (no DB; pure helpers/parsers) | 435 |
| `tests_integration/` | DB-backed integration (real Postgres, seeded, real router/server) | 556 |
| `tests/integration/` | Legacy API tests + Selenium smoke | 9 (+ 3 Selenium, Python) |
| **Total** | | **≈1,273 + 3** |

Per-file counts (significant files):

| File | Tests | Covers |
| --- | --- | --- |
| `tests_integration/SanitizerTest` | 162 | content sanitization (largest suite) |
| `tests/mocked/SanitizerUnitTest` | 81 | sanitizer unit surface |
| `tests_functional/RSSUtilsTest` | 81 | feed-fetch/parse utilities |
| `tests_functional/FunctionsTest` | 73 | `clean`, `truncate_*`, `arr_qmarks`, crypto helpers… |
| `tests/mocked/UrlHelperTest` | 75 | URL validation/canonicalization |
| `tests_functional/UrlHelperTest` | 69 | URL helpers (functional surface) |
| `tests_functional/FeedParserTest` | 53 | feed parsing |
| `tests_integration/DiskCacheTest` | 51 | disk cache lifecycle |
| `tests_functional/FeedItemCommonTest` | 50 | feed-item field extraction |
| `tests_functional/FeedItemAtomTest` | 42 | Atom-specific extraction |
| `tests_integration/TimeHelperTest` | 41 | time helpers |
| `tests_integration/FeedsCountersIntegrationTest` | 40 | counter queries (DB) |
| `tests_integration/LabelsIntegrationTest` | 35 | label CRUD/ops (DB) |
| `tests_integration/PluginHostTest` | 34 | plugin host mechanics |
| `tests/FeedItemTest` | 35 | feed-item parsing (unit) |
| `tests_integration/ArticleTest` | 32 | article statics (DB) |
| `tests_integration/SessionsIntegrationTest` | 29 | session handlers (DB) |
| `tests_integration/PrefsTest` | 29 | prefs get/set/profiles (DB) |
| `tests/FeedParserTest` | 21 | feed parsing (unit, RSS2/Atom variants) |
| `tests_integration/FeedsHeadlinesIntegrationTest` | 20 | headline queries (DB) |
| `tests_functional/CryptTest` | 18 | crypto |
| `tests/ErrorsTest` | 17 | error-formatting utilities |
| `tests/DebugTest` | 17 | debug utilities |
| `tests_functional/ConfigTest` | 16 | config |
| `tests_integration/SchedulerIntegrationTest` | 15 | scheduled-task engine |
| `tests_functional/LabelsTest` | 15 | label helpers |
| `tests_integration/FeedsIntegrationTest` | 13 | feed title resolution (DB) |
| `tests_integration/ApiTest` | 11 | external API against a **live server** (DB) |
| `tests_integration/RPCTest` | 10 | RPC handler (mark/publ/delete) |
| `tests_integration/ArticleHandlerTest` | 17 | Article handler (score/tags/labels/metadata/redirect) |
| `tests/ConfigTest` | 12 | config (unit) |
| `tests_functional/SanitizerTest` | 11 | sanitizer (functional) |
| `tests/mocked/TimeHelperTest` | 10 | time helpers (mocked) |
| `tests/integration/ApiTest` | 7 | legacy API tests |
| `tests_functional/TemplatorTest` | ~14 | template rendering (digests/mail) |
| `tests_functional/FeedsTest` | 3 | feeds |
| `tests_integration/LoggerSqlTest` | 8 | SQL log adapter |
| `tests_integration/UserHelperTest` | 8 | user helpers (DB) |
| `tests/integration/SanitizerTest` | 2 | legacy |
| `tests/mocked/CryptTest` | 5 | crypto (mocked) |
| **`tests/integration/selenium_test.py`** | **3** | **browser smoke (login / index / prefs)** |
| `tests_integration/UpdateRssFeedIntegrationTest.php~` | ~10 | **update engine — DISABLED (tilde filename)** |

### 2.2 Infrastructure (how it runs)

- **`tests_integration/run.sh`** — self-hosted lifecycle: (1) start a **temporary PostgreSQL**
  (port 55432) via `pg_helper.sh`, (2) create user/db, (3) run real schema migrations
  (`update.php --update-schema=force-yes`), enable the API for admin, (4) start **two PHP dev
  servers** (9080 = API under test, 9081 = mock *outgoing* server — it serves
  `tests_integration/feed.xml`, so feed-fetch tests hit a local "network"), (5) run PHPUnit
  with `phpunit.integration.xml`. Everything torn down on exit. `IS_INTEGRATION_TESTING=true`
  and xdebug coverage are set for the whole run.
- **`DbTestCase`** — before each test: `init_plugins()`, **re-seed the DB from
  `tests_integration/seed.sql`** (TRUNCATE-based, inside a transaction, ~41 feeds → in fact a
  small synthetic set: categories, labels, feeds, entries, user_entries, enclosures), set
  `$_SESSION['uid'] = 1`; on teardown destroy the session, reload Config, roll back any
  transaction. This is the per-test isolation model: *fresh seed per test method*.
- **`HandlerTestCase`** — invokes backend handlers **through `Router_Backend`, the same path as
  `backend.php`**, with a real admin session (`uid`, `pwd_hash`, `access_level`), multi-user
  mode on (so session validation is exercised), captures output, returns parsed JSON. This is
  what the parity program mirrors as "call the handler function directly".
- **`IntegrationTestCase`** — for API tests: real HTTP against the running PHP server, with
  login via the API, per-test DB re-seed, and a `waitForApiReady()` guard against
  dev-server deadlocks (the single-threaded PHP dev server is why there are two of them).
- **`tests/MockedDepsBootstrap`** — pre-loads mock `Config`/`Prefs`/`PluginHost` classes *before*
  the autoloader, so unit tests never touch a DB (documented rationale: static methods
  instantiate objects that call `Db::pdo()`).
- **`seed.sql`** is versioned and small/synthetic — deliberately different from the golden DB
  (the oracle's realistic dataset). The suite validates *logic on synthetic data*; the golden
  corpus validates *behavior on realistic data*. They are complementary.
- **`phpunit.xml`** (unit suite) is strict: `failOnRisky`, `failOnWarning`,
  `failOnPhpunitDeprecation` — the suite is kept green-by-construction upstream.
- **`selenium_test.py`** — the only browser test. Uses a **remote Selenium Grid**
  (`SELENIUM_GRID_ENDPOINT`, `APP_URL` env vars — CI-only), 3 tests:
  1. `test_login`: load login page (`body.ttrss_login`), fill `login`/`password`, click the
     Dojo login button.
  2. `test_index`: login → wait for `#feedTree` to render → `Feeds.open({feed:-4})` (all
     articles) → `#headlines-frame` visible → `Filters.edit()` → `#filterEditDlg` visible.
  3. `test_prefs`: login → `/prefs.php` → `body.ttrss_prefs` → wait for the Dojo accordion →
     switch to the feeds tab → `#feedTree` visible.

### 2.3 Mapping to the Rust port (parity status)

From `docs/plan.md` (parity program — 15/15 rows ported and green; the maintained count lives there):

| PHP suite(s) | Count | Rust status |
| --- | --- | --- |
| Sanitizer (all four suites) | 256 | ✅ ported 1:1 (row 2; 5 NOTEs for PHP-own bugs/libxml diffs) |
| Functions | 73 | ✅ 69 + 4 parked (row 4) |
| UrlHelper (144 total) | 144 | ✅ ported applicable subset (row 5: `validate` 9, `url_to_youtube_vid` 6); fetch/redirect cases are M6 |
| Crypt | 23 | ✅ 12 primitives + 7 N/A (row 6) |
| Config | 28 | ✅ applicable subset (row 7; env-dependent cases N/A) |
| UserHelper | 8 | ✅ (row 8) |
| Sessions | 29 | ✅ (row 9) |
| ArticleTest | 32 | ✅ (row 10) |
| FeedsCounters | 40 | ✅ (row 11) |
| FeedsHeadlines | 20 | ✅ (row 12) |
| Labels (15+35) | 50 | ✅ (row 13) |
| RPCTest | 10 | ✅ (row 14) |
| ArticleHandlerTest | 17 | ✅ (row 15) |
| Prefs | 29 | ✅ (row 3) |
| TimeHelper (51 total) | 51 | ✅ 24 applicable (row 1); rest N/A |
| FeedsTest (3) | 3 | ✅ (M2 close-out; feeds_parity) |
| DiskCache | 51 | ⚠️ `rewrite_urls` subset ported; lifecycle is M6 |
| **FeedItem / FeedParser / RSSUtils** | **~282** | ❌ **not ported — M6 (feed engine)** |
| **ApiTest (18)** | 18 | ⚠️ read ops covered by harness corpus; writes are M5 |
| **PluginHost** | 34 | ❌ M7 (plugin system) |
| **Scheduler** | 15 | ❌ M8 |
| **LoggerSql** | 8 | ❌ (system log — M5) |
| **Templator** | ~14 | ❌ (digest/mail rendering — M5/M6) |
| **UpdateRssFeedIntegrationTest** | ~10 | ❌ **disabled upstream (filename `~`); it is the M6 spec** |
| **selenium_test.py** | 3 | ❌ not ported (no browser layer yet) |

Roughly: **~700 of the ~1,273 PHP tests map to already-ported Rust modules and have been
consumed by the parity program; ~500 ride along with unported milestones (M5–M8)**, per the
project rule that tests land *with* their module, not after.

### 2.4 What the suite does NOT cover (gap analysis)

1. **The browser / SPA.** Beyond 3 smoke assertions (login page, feed tree appears, headlines
   frame appears, filter dialog, prefs accordion), there is zero UI coverage: no article-content
   rendering, no counter values, no search UI, no label UI, no settings round-trips, no
   syndication, no icon display, no error states. The product surface the user actually sees is
   essentially untested.
2. **The update engine, end-to-end.** The one test that would cover it
   (`UpdateRssFeedIntegrationTest`) is disabled (tilde filename — upstream work-in-progress).
   Feed fetching/parsing/dedup is covered only at the unit level (`FeedParser`, `RSSUtils`,
   `FeedItem*`), never as a pipeline.
3. **First-party plugin behavior.** The golden DB has 9 plugins enabled
   (`af_psql_trgm` search, `auto_assign_labels`, `nsfw`, `af_youtube_embed`, `share`,
   `bookmarklets`, `note`, `auth_internal`, `nginx_xaccel`); the suite tests only the
   *PluginHost mechanism* (34 tests), never what the plugins do to UI/search/labels.
4. **Time-based / daemon behavior** (update intervals, purge, digest scheduling, session
   expiry) — nothing runs a clock.
5. **Mail** (digest emails, recovery mails) — `Templator` is unit-tested, no send path.
6. **Security/concurrency/performance** — none.
7. **Realistic data.** The integration seed is synthetic; nothing in the PHP suite exercises the
   golden-scale dataset (that is the oracle harness's job).
8. **API write surface** is thinly covered (18 tests; lifecycle test exists for
   subscribe/unsubscribe but admin/config API writes are not).

### 2.5 Sufficiency verdict

**Question: "are the PHP tests sufficient to ensure some semblance of correctness?"**

- **For backend logic: yes, with caveats.** The suite is unusually good for a legacy PHP app:
  real DB + real migrations + real router + live-server API tests, strict PHPUnit config,
  per-test re-seeding, and a mocked bootstrap that keeps unit tests DB-free. Porting its cases
  1:1 (the parity program) pins the Rust port's *functions* to the PHP spec at high density —
  the parity program's full suite of Rust tests is green. **Caveat:** it pins behavior *as the PHP code behaves on
  synthetic inputs*; PHP's own bugs become spec (a deliberate characterization choice), and the
  ~500 unported tests only land when their modules do.
- **For system/UX correctness: no — and the suite was never designed to provide it.** The
  missing layers are precisely: browser rendering/interaction, the feed pipeline end-to-end,
  plugins, time, and mail. None of these is *wrong* in the port unless we check them, and the
  PHP suite cannot check them for us.
- **So "some semblance of correctness" decomposes into three claims, each needing its own
  evidence:**
  1. *The logic is right* → PHP-suite parity (largely done; completes per milestone).
  2. *The system behaves like the PHP system on realistic data* → the existing oracle harness
     (already 53/53 + 21/21; extends as the write/mutating surface lands).
  3. *The product looks and works in the browser* → **browser automation (Part B)**. This is
     the layer the user personally relies on to judge the port, and it currently has no
     automated evidence at all.

---

## 3. Part B — Browser automation for the port

### 3.1 Why a browser layer is the right missing piece

The tt-rss UI is a Dojo/SPA shell: the user never sees raw `backend.php` JSON — they see the
feed tree, counters, headlines, article pane, dialogs, and prefs. A port can be byte-exact at
the API level (harness) and still be "wrong" if the SPA boots differently, renders different
HTML (sanitizer output in the article pane!), mis-computes visible counters, or breaks a
dialog. The user's acceptance criterion is visual/interactive: *"is the port carried out
correctly?"* — which is a question only a browser can answer. The PHP repo's own
`selenium_test.py` confirms the upstream project treats the browser as the top of the test
pyramid (login → shell → feed tree → headlines → dialogs → prefs).

### 3.2 Tool landscape (2026)

| Option | Notes | Fit |
| --- | --- | --- |
| **Playwright** (Microsoft) | 2026 default for new projects (45M+ npm DL/mo): built-in **auto-waiting** + actionability checks (no timing flakes by construction), **built-in visual assertions** (`toHaveScreenshot`, pixelmatch), **clock API** (deterministic `Date`/timers), network interception, auto trace/video on failure, codegen for selector discovery, official **pinned Docker images**, no grid needed, first-party runners in Node/Python/Java/.NET | **Best fit** |
| Selenium (WebDriver) | The legacy incumbent; what `selenium_test.py` uses. Requires managing browser + driver (or a Grid), no built-in auto-wait, no built-in pixel diff, more boilerplate | Overkill for this; keep only if reusing the existing `.py` verbatim |
| Cypress | Great DX, but same-origin constraints (XHR-level) and a JS-only runner; weaker for "drive an arbitrary server from outside" | Worse fit |
| Puppeteer | Chromium-only, lower-level than Playwright; no built-in visual assertions | Fine, but Playwright strictly supersedes here |
| Rust-native (`thirtyfour` WebDriver client, `playwright` crate port, `chromiumoxide`) | Keeps the repo single-language; but **no mature equivalent of Playwright's visual-assertion + clock + trace tooling** — you'd hand-roll PNG diffing (e.g. with the `image` crate) and clock mocking | Viable, more work; only if a hard "Rust-only repo" constraint is adopted |

**Recommendation: Playwright, headless Chromium, in a pinned Docker image**
(`mcr.microsoft.com/playwright:v<version>-noble` — pin the exact tag; *never* `latest`, since a
silent browser change can turn a green visual suite flaky overnight). Runner language:
**Node/TS or Python, both first-party**. The repo already uses Python for oracle tooling
(`seed.py`, `selenium_test.py`), so a Python `playwright` runner fits the existing toolchain
with zero new runtimes; Node/TS has the most example coverage for the visual APIs. Either is
fine — the *tool*, not the language, is what matters.

Why the pinned-image point matters (research consensus): "Docker normalization is the single
highest-leverage fix — running the official Playwright image locally and in CI eliminates
cross-OS font and spacing flake almost entirely" ([TestQuality 2026 guide](https://testquality.com/playwright-visual-regression-guide/);
[Autonoma Playwright-Docker guide](https://getautonoma.com/blog/playwright-docker-guide)).
Browser rendering varies by OS, font config, headless mode, and power state
([Playwright docs](https://playwright.dev/docs/test-snapshots)) — same image on both sides of
the comparison removes the whole class.

### 3.3 The Playwright features this use case actually needs

1. **Auto-waiting / actionability checks** — every action waits for the element to be visible,
   stable, and enabled; this alone removes the majority of E2E flakiness (no `sleep`s).
2. **`expect(page).toHaveScreenshot()`** — built-in visual regression on top of **pixelmatch**:
   - first run generates the reference ("golden") PNG; later runs compare;
   - `maxDiffPixels` / `maxDiffPixelRatio` thresholds (e.g. `maxDiffPixelRatio: 0.005–0.01` —
     tight enough to catch real regressions, loose enough to absorb anti-aliasing);
   - `animations: 'disabled'`, `caret: 'hide'` for stable captures;
   - **`stylePath`** — inject a CSS stylesheet *only while taking the screenshot*: the correct
     way to mask volatile elements (a `#sessionInfo` div, relative-date spans) instead of
     tuning thresholds;
   - non-image snapshots: `expect(text).toMatchSnapshot('feedtree.txt')` — compare extracted
     DOM text exactly (deterministic, and far more informative than pixels for "is the data
     right");
   - `npx playwright test --update-snapshots` re-baselines (the record mode).
   ([Playwright visual comparisons docs](https://playwright.dev/docs/test-snapshots))
3. **The clock API** — `page.clock.setFixedTime(new Date('2026-09-07T12:00:00'))` fixes
   `Date`/`Date.now`/timers **inside the page**, making the UI's relative dates ("5 min ago",
   "2 hours") fully deterministic regardless of when the suite runs. This is the browser-side
   twin of the harness's timestamp normalization, and it works *for the app's own JS* (which
   computes "time ago" client-side from the feed dates in the golden DB).
   ([Playwright clock docs](https://playwright.dev/docs/clock))
4. **Network interception / response capture** — assert on the `backend.php` JSON the page
   actually consumed (stronger than DOM text alone: "the UI shows X because the API returned
   X" can be checked at both levels).
5. **Failure diagnostics** — automatic trace (`.zip` with DOM snapshots, console, network),
   optional video, screenshot-on-failure; the trace viewer makes "which page state broke?"
   trivial.
6. **`reducedMotion: 'reduce'` + fixed viewport** in the config — standard determinism settings.

### 3.4 Proposed design for ttrss (the lightweight version)

A new top-level `browser/` directory (kept *out* of the Rust workspace — different toolchain,
different layer):

```
browser/
  playwright.config        # chromium-only project, viewport 1280x800, reducedMotion,
                           # snapshot config, retries=0 locally / 1 in CI
  screenshot.css           # masks volatile elements for toHaveScreenshot
  utils.ts / utils.py      # login(BASE_URL) helper (mirrors the corpus login step),
                           # fixed clock install, wait-for-shell helper
  smoke.spec               # ← 1:1 port of tests/integration/selenium_test.py
  golden.spec              # ← the golden-UI-state suite
  baselines/               # committed reference PNGs + .txt snapshots (recorded from PHP oracle)
```

**Suite 1 — Smoke (≈3 tests, ports `selenium_test.py` 1:1):**

- login: `body.ttrss_login` → fill `login`/`password` (admin/admin from the golden DB) → click
  → shell loads.
- index: `#feedTree` visible → `Feeds.open({feed:-4})` → `#headlines-frame` visible →
  `Filters.edit()` → `#filterEditDlg` visible.
- prefs: `/prefs.php` → `body.ttrss_prefs` → Dojo accordion ready → feeds tab → `#feedTree`.

This proves the SPA boots and core navigation works against the Rust server — the same bar the
PHP project itself sets, with the same selectors.

**Suite 2 — Golden states (≈8–12 states; the actual "is the port correct" check):**

| # | State (deterministic via golden DB + fixed clock) | Evidence |
| --- | --- | --- |
| 1 | Login page | screenshot |
| 2 | Shell home after login (feed tree + counters visible) | text snapshot of feed-tree titles + counter values; screenshot |
| 3 | Feed open (e.g. feed 1 / Ars Technica): headline list | text snapshot: first N headline titles + feed counter; screenshot |
| 4 | Article open (`{{article_1}}` = ref 20): content pane | text snapshot of article title + rendered content structure (sanitized HTML!); screenshot |
| 5 | All-articles view (`feed:-4`) | text snapshot: top headline titles |
| 6 | Label view (e.g. "Read later") | text snapshot: labeled article titles |
| 7 | Starred view (`feed:-1`) | text snapshot |
| 8 | Prefs: profile tab | text snapshot of pref labels; screenshot |
| 9 | Prefs: feeds tab (feed tree) | text snapshot |
| 10 | Error state (e.g. invalid CSRF / unknown method via UI) | text snapshot |

Per state: **text/JSON snapshot (exact, deterministic) + full-page screenshot
(pixel-diff, masked)**. The text snapshots carry the data-correctness signal ("the right
titles/counters rendered"); the screenshots carry the layout/rendering signal.

**Oracle A/B (blind golden master at the UX level):**

1. Run both suites once against the **PHP oracle** (golden DB, same browser build, same
   viewport/clock) → `--update-snapshots` → commit the baselines (PNG + txt).
2. Every check runs the suites against the **Rust server** and asserts against those baselines.
3. Re-baseline only when the *oracle* changes (new golden data / PHP change) — which never
   happens, since the oracle is frozen. So the baselines are stable for the life of the project.

This reuses the existing workflow primitives (golden DB reset, login, `ids.json`) and costs one
extra Playwright run per check. It is the minimal form of the record-against-the-oracle-then-
compare idea, applied where the user actually looks.

**Determinism checklist (per state):**

- golden DB (identical data both sides); fixed user (admin); icons from the existing
  `oracle/data/feed-icons` cache (no network);
- `page.clock.setFixedTime(<fixed>)` **before** navigation → all client-side "time ago" text
  deterministic;
- viewport fixed (1280×800), `reducedMotion: 'reduce'`, single browser project (chromium),
  `animations: 'disabled'` + `caret: 'hide'` in screenshot options;
- `stylePath` CSS to hide anything inherently volatile (session/csrf text, anything keyed on
  the csrf token);
- wait strategy: explicit `expect(...).toBeVisible()` on the state's anchor element
  (e.g. `#headlines-frame`) — Playwright's auto-wait handles the rest; avoid bare
  `networkidle` unless needed;
- threshold only after determinism: start with exact PNG compare; if anti-aliasing noise
  appears, raise to `maxDiffPixelRatio: 0.005`.

**Effort estimate:** `playwright.config` (~20 lines), `screenshot.css` (~10), login/clock utils
(~40), smoke spec (~60), golden spec (~150), baselines (one recording pass against the oracle).
~250–300 lines total; suite runtime a few seconds per spec on headless Chromium. Makefile
integration: a `make browser` target = reset golden DB → start Rust server → `npx playwright
test` (plus a `make browser-record` target pointed at the oracle for re-baselining).

### 3.5 Flakiness management (what the research says to do / avoid)

**Do:**

- Same pinned browser build for recording and comparison (Docker image, exact tag);
  local and CI identical ([TestQuality](https://testquality.com/playwright-visual-regression-guide/),
  [Autonoma](https://getautonoma.com/blog/playwright-docker-guide)).
- Element-level screenshots for components, full-page for whole states
  ([TestDino](https://testdino.com/blog/playwright-visual-testing)); treat baselines like code —
  review every re-baseline (a changed baseline is a behavioral claim).
- Mask volatile content with `stylePath` CSS rather than widening thresholds — thresholds are
  the last lever, not the first.
- Auto-wait + actionability over sleeps; explicit waits for the state anchor.
- 1 retry in CI for transient env failures; keep local retries at 0 so flakes are visible;
  rely on traces for diagnosis.
- Keep the suite small (golden-set thinking): a dozen high-signal states beat a hundred
  brittle ones.

**Avoid:**

- Selenium Grid / remote drivers (the only reason the PHP test uses them is CI history);
- SaaS visual tools (Percy/Chromatic) — the comparison is local and free here;
- `latest` browser images, multi-browser projects (every extra browser is extra baselines and
  extra drift surface — one browser is enough to prove the port),
- asserting on inherently volatile text (csrf, wall-clock) instead of masking/clocking it,
- writing Selenium-style `implicitly_wait`-driven scripts in Playwright (defeats the
  auto-wait design).

### 3.6 How this composes with the other layers

- **PHP-suite parity (Part A):** the logic floor — function-level equivalence.
- **Oracle harness (existing):** the API/data floor — byte-level response + state equivalence
  over the golden corpus.
- **Browser layer (this report):** the UX floor — the SPA actually renders and navigates the way
  the user expects, with the right data, against the oracle's own recorded appearance.

Together: *if a state is right at the API level (harness) and the UI layer renders the API
output the same way as PHP (browser golden states), the product is correct at every level the
user can observe.* No heavyweight frameworks, no production shadowing — just the existing
golden-DB workflow plus one pinned browser.

---

## 4. Open decisions

1. **Runner language:** Python (fits existing toolchain) vs Node/TS (richest visual-API
   examples) — both first-party Playwright; pick one, low stakes.
2. **Baseline provenance:** record golden baselines from the PHP oracle once (recommended) vs
   from the Rust server (would only test Rust-vs-Rust stability).
3. **Screenshot threshold policy:** exact compare first, tighten/loosen per state after the
   first runs (data will say).
4. **Clock value:** one fixed instant for all states (simplest) vs per-state instants (matters
   only if a state's text depends on the *delta* to "now" — e.g. "5 min ago" — in which case
   pick a time a few minutes after the latest seed date so relative strings are stable and
   non-degenerate).
5. **Scope of the golden set:** the 10 states above vs starting with 5 (login, shell, feed
   headlines, article, prefs) and growing per milestone.

## 5. Sources

**Local (frozen checkout, `tt-rss/` at `2f7d831`):**

- `tests/`, `tests/mocked/`, `tests_functional/`, `tests_integration/`, `tests/integration/`
  (all files inventoried in §2; test counts measured by method enumeration)
- `tests_integration/run.sh`, `pg_helper.sh`, `bootstrap.php`, `DbTestCase.php`,
  `HandlerTestCase.php`, `IntegrationTestCase.php`, `seed.sql`, `feed.xml`
- `tests/MockedDepsBootstrap.php`, `phpunit.xml`, `phpunit.integration.xml`
- `tests/integration/selenium_test.py` (74 lines; Selenium Grid; 3 tests)
- `tests_integration/UpdateRssFeedIntegrationTest.php~` (disabled; update-engine spec)
- `docs/plan.md` (parity program rows 1–15; per-module status)

**Browser automation (web research, 2026):**

- Playwright vs Selenium/Cypress 2026:
  [BrowserStack](https://www.browserstack.com/guide/playwright-vs-selenium),
  [Browserless](https://www.browserless.io/blog/playwright-vs-selenium-browser-automation-comparison),
  [QA Skills](https://qaskills.sh/blog/playwright-vs-selenium-2026-which-better),
  [Thinksys](https://thinksys.com/qa-testing/playwright-vs-selenium-vs-cypress/),
  [Ranorex](https://www.ranorex.com/blog/playwright-vs-selenium/)
- Visual comparisons (primary docs):
  [Playwright — Visual comparisons](https://playwright.dev/docs/test-snapshots)
  (`toHaveScreenshot`, pixelmatch options, `maxDiffPixels`/`maxDiffPixelRatio`, `stylePath`,
  non-image `toMatchSnapshot`, `--update-snapshots`, same-environment warning)
- Visual practice guides:
  [TestQuality — Playwright Visual Regression Guide 2026](https://testquality.com/playwright-visual-regression-guide/)
  (Docker normalization = highest-leverage anti-flake fix),
  [TestDino — Playwright Visual Testing](https://testdino.com/blog/playwright-visual-testing)
  (element vs full-page; threshold guidance; "treat baselines like code"),
  [OneUptime — Playwright Visual Testing](https://oneuptime.com/blog/post/2026-01-27-playwright-visual-testing/view)
  (`animations: 'disabled'`, `caret: 'hide'`),
  [OneUptime — Visual Regression Handling](https://oneuptime.com/blog/post/2026-01-24-visual-regression-testing/view)
  (config skeleton: viewport, `reducedMotion`, `maxDiffPixelRatio`, single browser project)
- Clock API (primary docs):
  [Playwright — Clock](https://playwright.dev/docs/clock)
  (`setFixedTime` / `install` / `pauseAt` / `fastForward`; overrides `Date`, timers,
  `performance`, `Event.timeStamp`);
  practice: [QASkills — Clock API guide](https://qaskills.sh/blog/playwright-clock-api-time-testing-guide),
  [Testing with Edd](https://testingwithedd.github.io/testing/playwright/2024/06/28/test-time-with-playwright.html)
- Docker/CI:
  [Playwright — Docker docs](https://playwright.dev/docs/docker),
  [Playwright — Browsers](https://playwright.dev/docs/browsers) (headless shell, `--only-shell`),
  [Autonoma — Playwright Docker Guide](https://getautonoma.com/blog/playwright-docker-guide)
  (pin exact `mcr.microsoft.com/playwright:vX-noble`, never `latest`),
  [Browserless — Run Playwright in Docker](https://www.browserless.io/blog/run-playwright-in-docker)
- Rust-native options (evaluated, not recommended):
  [thirtyfour — Rust WebDriver/CDP client](https://vrtgs.github.io/thirtyfour/),
  [`playwright` crate (Rust port)](https://crates.io/crates/playwright),
  [lib.rs testing category](https://lib.rs/development-tools/testing?sort=popular)
