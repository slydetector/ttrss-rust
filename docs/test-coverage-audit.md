# tt-rss PHP Backend — Test Coverage Audit (quantified)

Source: direct audit of the checkout in `tt-rss/` (PHP 8.x, PHPUnit 12, PostgreSQL).
All numbers measured from the tree; "LOC" = lines of test code, "prod LOC" =
`classes/` + `include/` + `api/` + root entrypoints (~24.4k).

## 1. Suite inventory

| # | Suite | Location | Files | Test methods | Assertion calls | Test LOC | Where it runs |
|---|---|---|---|---|---|---|---|
| 1 | Functional (main unit/functional) | `tests_functional/` | 12 | **435** | **723** | 5,242 | GitHub CI, matrix PHP 8.3/8.4/8.5 (+8.6 experimental), `phpunit --exclude-group integration` |
| 2 | Mocked pure-unit | `tests/mocked/` | 4 | **171** | **397** | ~1,400 | GitHub CI, 2nd run with `MockedDepsBootstrap` (Config/PluginHost mocked, no DB) |
| 3 | Integration (real Postgres + 2 PHP dev servers) | `tests_integration/` | 16 test + 4 infra | **555** | **1,115** | 7,200 | **Local only** via `run.sh` (ephemeral PG :55432, PHP servers :9080/:9081, xdebug coverage). Not referenced by any GitHub workflow |
| 4 | Browser E2E (Selenium) | `tests/integration/selenium_test.py` | 1 | **3** | ~10 | 74 | Manual / external (needs `SELENIUM_GRID_ENDPOINT`); not in CI |
| 5 | Legacy (orphaned — not run anywhere) | `tests/*.php` + `tests/integration/` | 7 | 111 | 238 | ~1,800 | Nowhere (phpunit.xml only includes `tests_functional`) |

**Totals: ~39 test files, ~1,280 test methods, ~2,480 assertion calls, ~16.3k test LOC**
→ test:production ratio ≈ **0.67 : 1** (by LOC).

### Suite 1 breakdown (per file, methods):
FeedItemCommon 50 · FeedItemAtom 42 · FeedParser 53 · Functions 73 · UrlHelper 69 ·
RSSUtils 81 · Config 16 · Crypt 18 · Labels 15 · Sanitizer 11 · Templator 4 · Feeds 3

### Suite 3 breakdown (per file, methods):
Sanitizer 162 (1,153 LOC — largest single test file) · DiskCache 51 · FeedsCounters 40 ·
Labels 35 · PluginHost 34 · Article 32 · Prefs 29 · Sessions 29 · TimeHelper 41 ·
FeedsHeadlines 20 · ArticleHandler 17 · Scheduler 15 · Feeds 13 · ApiTest 11 (live HTTP
against the external JSON API: login, getVersion, getUnread, getCounters, getFeedTree,
getHeadlines, getArticle+markAsRead, markArticleAsMarked, subscribe/subscribeUnsubscribe
lifecycle, catchupFeed, Content-Length header) · RPC 10 · LoggerSql 8 · UserHelper 8

### Assertion style (all suites):
`assertEquals` 736 · `assertStringContainsString` 418 · `assertTrue` 257 ·
`assertFalse` 161 · `assertCount` 129 · `assertSame` 119 · `assertArrayHasKey` 119 ·
`assertStringNotContainsString` 109 · `assertNotFalse` 77 · `assertNotEmpty/Empty` 79 ·
`assertContains/NotContains` 62 · type asserts ~60 · custom helpers 36
(`assertResponseOk`, `assertResponseHasKey`, `assertTagSurvives`,
`assertNormalizeCategoriesEquals`)

## 2. Test *types* present

| Type | Present? | Evidence |
|---|---|---|
| Unit (pure, mocked deps) | ✅ | suite 1 + 2 (606 methods in CI) |
| Functional (domain classes, real code paths, no I/O) | ✅ | suite 1 (filter evaluation, hashing, parsing, URL rules, crypto round-trips) |
| Integration w/ real database | ✅ | suite 3 (ephemeral PG, migrations run via `update.php --update-schema`, `seed.sql` 170 LOC re-seeded per test in a transaction via `DbTestCase`) |
| HTTP integration (real server) | ✅ (limited) | `ApiTest` (11 tests) hits a live PHP dev server; `HandlerTestCase` simulates `backend.php` in-process (sets `$_REQUEST`/`$_SESSION`/CSRF, ob_start, invokes handler, parses JSON) |
| Outgoing-HTTP mocking | ✅ | 2nd PHP dev server (:9081) serves `feed.xml` as mock upstream feed |
| Browser E2E | ✅ (minimal) | 3 Selenium scenarios: login → index (feedTree, headlines, filter dialog) → prefs (tabs, feed tree) |
| Static analysis | ✅ | **PHPStan level 6** over `classes/` (CI-gated) + Rector dry-run + PHPUnit `failOnWarning/failOnRisky` |
| Lint | ✅ | eslint (js/plugins), stylelint (CSS/LESS), shellcheck, SQL-execution validation workflow (schema + every migration executed against Postgres 15) |
| Coverage collection | ⚠️ configured only | `phpunit.integration.xml` has a `<coverage>` block; `run.sh` sets `XDEBUG_MODE=coverage`; **CI explicitly runs `coverage: none`** — no coverage % is ever produced or published |
| Golden-master / record-replay / contract tests | ❌ none | No fixture/response files; all expectations hand-written inline |
| Data-driven tests | ❌ | 0 `#[DataProvider]` attributes; fixture variety via inline arrays (e.g. ~105 URL/fixture strings in FeedItemAtomTest) |
| Property-based / fuzzing | ❌ | — |
| Performance / load / timing | ❌ | — |
| Chaos / failure-injection | ❌ | — |

## 3. Coverage by production area (active suites only)

| Production area (approx. LOC) | Active test methods | Coverage quality |
|---|---|---|
| Feed parsing (`FeedParser`, `FeedItem_*`, ~700) | 145 (+111 legacy) | **Strong** — the best-tested area |
| URL helpers / validation / SSRF (`UrlHelper`, 610) | 144 | **Strong** (validation logic; no live-network tests) |
| Sanitizer (319) | 254 | **Strong** (81 pure + 162 integration, incl. `assertTagSurvives` corpus) |
| `RSSUtils` filters/hashing/housekeeping (2,019) | 81 + scheduler 15 | **Moderate** — filter evaluation well covered; full `update_rss_feed` pipeline has **no active** test (only an orphaned `UpdateRssFeedIntegrationTest.php~` backup file) |
| Counters/headlines/feeds/labels queries | 108 (integration) + 15 + 73 | **Moderate** — real DB, single-user seeded fixtures |
| Prefs class (518) | 29 + 16 (config) | **Moderate** |
| Sessions (DB-backed, encryption) | 29 + 18 + 5 (crypto) | **Moderate** (encrypted round-trip tested in functional CryptTest) |
| Users/auth (482) | 8 (UserHelper) + 11 (ApiTest login) + 3 (selenium) | **Thin** — TOTP, app passwords, hash-format matrix not tested |
| PluginHost (943) / Plugin (743) | 34 | **Thin-to-moderate** (registry/storage/hooks basics) |
| DiskCache (467) | 51 | **Moderate** |
| TimeHelper | 51 | **Strong** |
| Templator/MiniTemplator | 4 | **Minimal** |
| External JSON API surface (`API`, 932) | 11 (10 of ~20 methods) | **Thin** |
| `backend.php` handler surface: `RPC` (710), `Feeds` (2,413), `Article` (705) | 10 + 17 + 32 + 20 | **Thin** — only a handful of methods per handler |
| **All six `Pref_*` handler classes (4,900+ LOC combined)** | **0 dedicated tests** | **Absent** |
| `Handler_Public` (login form flow, Atom/JSON syndication, forgotpass, dbupdate, cached, feed_icon, healthcheck; 848) | 0 (only ApiTest-adjacent) | **Absent** |
| `OPML` (619) | 0 | **Absent** |
| `Digest` + `Mailer` (193 + ~120) | 0 | **Absent** |
| Search (`_search_to_sql`, tsvector/trgm) | 0 direct (implicit via headlines tests) | **Absent** |
| `update_daemon2.php` fork/lock lifecycle (269) | 0 | **Absent** |
| CLI `update.php` subcommands (579) | 0 direct (only `--update-schema` exercised as harness setup) | **Absent** |

## 4. Gaps & non-determinism handling

- **No published coverage numbers** — coverage tooling exists but is off in CI; the
  only runs that collect it are local integration runs. No baseline % is known
  (would need one xdebug run of suite 3 to produce it).
- **No golden/record-replay infrastructure** — the closest things are inline
  expected values and `feed.xml` fixtures; there are no saved API response
  snapshots, so parity verification for a Rust port must be built from scratch.
- **Integration suite is maintainer-only** (not in CI) → it can silently rot;
  the orphaned `~` backup of the update-feed integration test is evidence of that.
- **Seed data is minimal**: `seed.sql` (170 LOC) creates ~2 categories, a handful
  of feeds/entries/labels for uid 1 — far from a "realistic populated dataset".
- **No live-network tests anywhere** — good for determinism, but feed-fetch edge
  cases (429, gzip, redirects, proxies) are only unit-tested in isolation.
- Non-determinism in tests is handled by per-test DB re-seed in a transaction,
  `TTRSS_LOG_DESTINATION=stdout`, fixed session fixtures (e.g. hardcoded
  `SHA1('password')` hash), and local mock servers — i.e., manual normalization,
  no fake-clock or response-normalization tooling.

## 5. Implications for the Rust port

1. The existing suite is a **solid starting spec for ~40–50% of the domain**
   (parsing, URL, sanitizer, filters, prefs, counters, sessions, time) and can be
   ported 1:1 as Rust executable specs (per Fowler: port the old unit tests).
2. **~50% of the HTTP/handler surface is untested upstream** (all Pref_* handlers,
   Handler_Public, OPML, digest, search, daemon, CLI) — exactly where the Rust
   port is most likely to diverge. The golden-diff harness from RESEARCH-REPORT
   (M0) is therefore *load-bearing*, not optional.
3. To get a true baseline, run suite 3 once locally with xdebug and record the
   line/branch coverage % per class (one-time measurement, feeds the PLAN risk
   table).
4. Upstream has no data-driven/golden infrastructure; our M0 harness adds a
   capability that doesn't exist in the project today.
