# 20 — Plan: built-in schema bootstrap & migration at startup

Status: **design** (not implemented). Supersedes the "No schema
bootstrap/migration" gap in `docs/19-plan-docker-image.md` (P0) and defines
the mechanism the Docker image's entrypoint will rely on.

## 1. Requirements

1. **Built into the `ttrss` binary** — no external tool, no shell SQL.
2. **Automatic, on every startup** — before the server accepts traffic (and
   before the in-process daemon starts, since it runs schema-dependent SQL).
3. **Idempotent** — an up-to-date DB costs one `SELECT`; a fresh DB gets the
   full schema; an old DB is walked forward, one file at a time.
4. **Fail closed** — a migration failure stops the process (non-zero exit)
   before the listener binds; never serve a schema the code doesn't know.
5. **Drop-in interop** — the on-disk version state must stay compatible with
   the PHP reference in *both* directions (a DB written by Rust is readable
   and migratable by `update.php`, and vice versa).
6. Faithful to the PHP reference (`tt-rss/classes/Db_Migrations.php`, frozen
   at 2f7d831) — the migration files are the same artifacts, applied with
   the same semantics.

## 2. The PHP reference, distilled

- **Artifacts**: `sql/pgsql/schema.sql` (407 lines, 33 tables, 115
  statements — the *complete* v151 schema, seeding `admin`/`password`) and
  `sql/pgsql/migrations/3.sql … 151.sql` (149 files, contiguous, 593
  statements total). No file contains `$$` dollar-quoting.
- **Version store**: single-row table `ttrss_version (schema_version
  integer not null)` — no primary key; created lazily on first read;
  version `-1` = table absent; `SCHEMA_VERSION = 151` (in `Config`) is the
  ceiling that overrides the highest file on disk.
- **Fresh DB**: `migrate_to(0)` loads `schema.sql`; because
  `base_is_latest = true`, the version is stamped straight to 151 — no
  numbered migrations run.
- **Upgrade**: `for v in version+1..=151: migrate_to(v)` — each file in its
  **own transaction** (all statements + the `ttrss_version` update commit
  together; any failure rolls back the file and rethrows).
- **Statement parsing** (`get_lines`): split lines on `[\r\n]`, drop lines
  that are empty after trim *or* start with `--` (untrimmed), join the rest
  with no separator, split on `;`, drop empties and literal `begin`/`commit`
  tokens (case-insensitive). The files' own `begin;`/`commit;` wrappers and
  132 of 149 self-`update ttrss_version` lines are harmless under this.
- **Actors**: only `update.php --update-schema` migrates; the web app is
  read-gated (`E_SCHEMA_MISMATCH`); nothing is interactive in the Docker
  flow (`force-yes`).

## 3. Design

### 3.1 SQL file location & delivery

- **Runtime files under the docroot — no copy, no embedding.** The app
  already depends on the `tt-rss/` tree for static assets (HTML, JS,
  templates); the SQL artifacts are no different. The migration root is
  derived exactly like PHP's `Config::get_self_dir() . "/sql"`:
  `config.docroot.join("sql/pgsql")` (`docroot` = `tt-rss`, CWD-relative,
  `TTRSS_DOCROOT` override). Single source of truth: the frozen reference
  tree that the image bakes into the docroot and the repo ships as-is.
- `Migrations::from_root(root: PathBuf)` is the one constructor —
  production passes `<docroot>/sql`, tests pass a temp dir with a doctored
  migration file (mirrors PHP's `initialize($root_path, …)`).
- `get_max_version()` lists `root/migrations/*.sql` at runtime
  (`fs::read_dir` + numeric filename parse — the port of PHP's
  `glob` + `natsort`; filenames are plain integers, so numeric sort
  equals natsort) and caps at `SCHEMA_VERSION`.
- **Failure mode**: a missing file is a startup error naming the exact
  path (`sql/pgsql/migrations/150.sql not found under <docroot>`) — PHP's
  own "Requested schema file not found". A misconfigured docroot fails
  closed at startup, before any request.

### 3.2 Module & API — `crates/ttrss/src/core/schema.rs`

```rust
pub const SCHEMA_VERSION: i32 = 151;      // re-export/alias of config's, i32 = INT4

pub struct Migrations { /* root: PathBuf, table, base_is_latest, max_override */ }

impl Migrations {
    pub fn from_root(root: PathBuf) -> Self;  // <docroot>/sql in prod, temp dir in tests
    pub async fn get_version(&self, pool: &PgPool) -> Result<i32>;  // -1 = absent
    pub async fn migrate(&self, pool: &PgPool) -> Result<()>;       // the whole job
    pub async fn is_migration_needed(&self, pool: &PgPool) -> Result<bool>;
}

/// Startup entry point (what the server startup calls; any future CLI
/// reuses the same function):
/// pg_trgm + ensure up to date, fail-closed.
pub async fn ensure_up_to_date(db: &Db) -> Result<()>;
```

- The table name, `base_is_latest`, and the version ceiling are fields,
  exactly like PHP's `initialize()` — the plugin-migration path
  (`ttrss_migrations_plugin_*`) can reuse the same engine later (out of
  scope here, §6).
- `schema_version` decodes as **`i32`** (INT4, per the sqlx strict-decode
  rule); the i64 `SCHEMA_VERSION` in `config` stays as-is, cast at the
  boundary.

### 3.3 Algorithm (port of `Db_Migrations::migrate`)

```
create table if not exists {table} (schema_version integer not null)   # eager, §4-d5
let v = get_version()                                                   # -1 if no row
if v == -1:
    apply(schema.sql) in one tx:  statements → set_version(max)
for i in (v+1)..=max:
    apply(migrations/{i}.sql) in one tx:  statements → set_version(i)
```

`apply(file)`:
1. `split_statements(file_text)` — the exact PHP `get_lines` port (§3.4).
2. `tx = pool.begin()`; execute each statement sequentially
   (`sqlx::query(...).execute(&mut *tx)` — `statement_cache_capacity(0)`
   pools already do this everywhere, same strictness).
3. `set_version(i)` inside the same tx: `SELECT` the (single) row →
   `UPDATE … SET schema_version = i` or `INSERT (i)` — PHP's exact
   fetch-then-branch on a PK-less table.
4. `commit`; on any error: `rollback`, `tracing::error!`, rethrow.

- **One transaction per file** (PHP parity). Base schema is re-runnable by
  construction (`drop table if exists` prefix) — a failed fresh install
  retries cleanly on the next boot. Numbered migrations are *not*
  idempotent; correctness comes solely from the version guard.
- **Logging** (parity with PHP's log levels): `info!` per file
  ("schema: applying migration {i}" / "schema: loaded base schema → v{max}"
  / "schema: up to date (v{v})"), `debug!` per statement.

### 3.4 Statement splitter (unit-tested, byte-faithful)

```rust
pub fn split_statements(text: &str) -> Vec<String> {
    text.split(|c| c == '\r' || c == '\n')
        .filter(|l| !l.trim().is_empty() && !l.starts_with("--"))
        .collect::<Vec<_>>().join("")
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !matches!(s.to_ascii_lowercase().as_str(), "begin" | "commit"))
        .map(str::to_string)
        .collect()
}
```

Deliberately naive (no `;`-in-string handling) — that matches PHP and is
verified sufficient: all 150 files parse (115 + 593 statements, no empty
files), and the parity test pins the exact statement counts so a future
file that breaks the assumption fails CI, not production.

### 3.5 Startup integration — `main.rs`

```
config → Db::connect → schema::ensure_up_to_date(&db).await?   # NEW
       → daemon loop spawn → router build → listener bind → serve
```

- Runs in **every** process that connects the DB and serves (the single
  binary today; the user-verb bootstrap steps, §4, run in the same startup
  path and any future CLI wraps the same function).
- `TTRSS_DAEMON=0` and the harness paths are unaffected (harness tests
  build DBs via the test-db fixtures, not this path).
- No interactive confirmation (Docker); the failure is a logged `anyhow`
  error and a non-zero exit *before* `TcpListener::bind`.

### 3.6 `pg_trgm`

`ensure_up_to_date` first runs `CREATE EXTENSION IF NOT EXISTS pg_trgm`
(mirrors the official image's `startup.sh` ordering: pg_trgm → schema).
It's a **trusted** extension (PG ≥ 14; the target is PG 17), so a plain DB
owner can create it. Idempotent no-op when present. Failure → `warn!` and
continue (the only consumer, `af_psql_trgm`, is off by default and reports
the missing extension itself) — see decision d6.

### 3.7 Concurrency

Wrap the whole migration window in a session-level **advisory lock** on a
dedicated connection: `SELECT pg_advisory_lock(4289)` (fixed constant) →
re-read the version (the lock may have been held by a concurrent boot that
already migrated) → apply as needed → release on drop. Two containers
starting against one DB serialize instead of racing on `ttrss_version`.
PHP has no such guard (it ships one container per DB); this is hardening,
d4.

## 4. Bootstrap seam (docker plan P0)

The official image's boot (`tt-rss/.docker/app/startup.sh`) runs, after the
schema step, the admin-password policy + auto-user creation — six `update.php`
user-verb invocations, all env-var driven (`ADMIN_USER_PASS` applied every
boot; stock `password` still set → random 16-char + star banner;
`ADMIN_USER_ACCESS_LEVEL`; `AUTO_CREATE_USER*`).

**Direction (set by the maintainer): those steps fold into the binary's
startup path** — env-driven and in-process, the same pattern as this schema
work — instead of separate CLI invocations from the entrypoint shell.
Concretely:

- the six user verbs become small library functions over `ttrss_users`
  (each is one or two queries — `update.php`'s own implementations are the
  porting spec);
- the server startup runs the full bootstrap sequence before binding:
  `schema::ensure_up_to_date` → admin-password policy → access level →
  auto-user, each step a no-op when its env var is unset / its precondition
  unmet (idempotent, like the schema step);
- the entrypoint shell shrinks to: secrets expansion → DB wait → chown →
  `exec ttrss`;
- any later CLI form (manual ops) is a thin wrapper over the same
  functions and the same `ensure_up_to_date`, so no path can diverge.

**The full sequence has landed (2026-10-14).** The user verb library
functions live in `ttrss::users` (`user_add`, `user_modify`,
`user_has_password`, `user_exists`, `user_check_password`,
`user_set_password`, `user_set_access_level`, `user_enable_api`,
`user_remove` — 1:1 ports of the `UserHelper` statics + `update.php` verb
semantics, `tests/user_verbs.rs`, 12 cases; no PHP CLI suite exists for
these, so the port carries its own coverage). The env-driven **policy
wiring** is `ttrss::bootstrap` (`main.rs`, after `ensure_up_to_date`,
fail-closed): admin-password star-banner, `ADMIN_USER_ACCESS_LEVEL`,
`AUTO_CREATE_USER*`, and the post-init SQL patches at
`<data_dir>/sql/post-init.d` (the reference's `${SCRIPT_ROOT}/sql/
post-init.d`, mapped to the data dir — our image has no separate script
root; the dir + the single-`app`-volume layout are in `docs/19` §3).
`tests/bootstrap.rs` (12) covers the policy; `Config.data_dir`
(`TTRSS_DATA_DIR`) is the app-data root (`docs/19`).

## 5. Tests

1. **Statement-count pin** (unit, no DB): `split_statements` yields exactly
   115 statements from `schema.sql` and 593 across the 149 migrations
   (files read from `tt-rss/sql/pgsql/` via a `CARGO_MANIFEST_DIR`-relative
   path — test-only, so the repo-relative location is fine). This pins the
   naive-parser assumptions against the real artifacts.
2. **Splitter unit tests**: `--` line (untrimmed check), indented comment
   (kept, like PHP), empty lines, `begin;`/`commit;` removal
   (case-insensitive), multi-statement lines, CRLF.
3. **DB tests** (test-db crate, `Migrations::from_root` pointed at
   `tt-rss/sql` via a `CARGO_MANIFEST_DIR`-relative path):
   - fresh DB → `migrate()` → `ttrss_version = 151`, all 33 tables,
     `admin` user present (the stock hash), `pg_trgm` present;
   - **no-op**: DB at 151 → `migrate()` → version unchanged, zero writes
     (assert via `pg_stat` counters or a snapshot of `ttrss_version` + a
     probe table);
   - **upgrade**: DB stamped 149 (schema.sql + `set 149`) → exactly
     `150.sql`, `151.sql` applied → 151;
   - **rollback**: temp dir whose `150.sql` contains a failing statement →
     `migrate()` errors, version stays 149, the file's earlier statements
     are gone (e.g. the new column from a doctored `150.sql` is absent);
   - **ceiling**: temp dir with a `152.sql` + `max_override = 151` → 151
     wins (PHP's `SCHEMA_VERSION` override behavior).
4. **Startup**: an integration test boots `main`-equivalent startup against
   a fresh test-db URL and asserts `/healthz` 200 (the P1 fresh-install
   smoke in docs/19 becomes redundant with this and can shrink to the
   image-level checks).

No PHP-side unit tests for `Db_Migrations` exist (checked `tt-rss/tests/`)
— the port adds real coverage for a class PHP never tested.

## 6. Out of scope (deliberate)

- **Plugin migrations** (`initialize_for_plugin`,
  `ttrss_migrations_plugin_*` tables) — needed for plugin install parity,
  not for startup; the `Migrations` fields already leave the seam.
- **Downgrades** — PHP can't do them (ascending loop only); neither can we.
  A DB stamped *above* `SCHEMA_VERSION` (e.g. run by a newer PHP) makes
  `is_migration_needed()` true with an empty apply loop → **fail startup**
  with "schema v{v} is newer than this build (v151); upgrade ttrss" (d7).
- **Interactive `--update-schema` confirmation** — Docker is
  non-interactive; there's no stdin in the entrypoint.
- Replacing the PHP `E_SCHEMA_MISMATCH` web gate — the port serves only
  known-schema DBs by construction.

## 7. Decisions to revisit

- **d1 — runtime files under the docroot, no embedding, no copy** (set):
  the app already treats `tt-rss/` as its runtime asset tree (static files,
  templates); the SQL artifacts are no different — `<docroot>/sql/pgsql`
  is the single source of truth, exactly PHP's `get_self_dir() . "/sql"`.
  Consequence: the binary needs the docroot present to start (it already
  does — static serving is impossible without it), and a future port-owned
  migration `152` simply lands in the tree the image ships.
- **d3 — hand-rolled engine, not `sqlx::migrate`** despite the feature
  being enabled: sqlx's migrator keeps its own `_sqlx_migrations` table and
  file layout — a DB migrated by it would no longer interop with PHP's
  `ttrss_version`, breaking requirement 5.
- **d4 — advisory lock** added (no PHP equivalent): cheap insurance against
  concurrent boots; one extra `SELECT` per startup.
- **d5 — eager `CREATE TABLE IF NOT EXISTS ttrss_version`** vs. PHP's lazy
  creation on read failure: same end state, one fewer exception path.
- **d6 — `pg_trgm` failure is a warning, not fatal**: PHP's container makes
  it fatal (superuser DB); a warning keeps non-superuser deployments alive
  at the cost of `af_psql_trgm` being unusable — flip to fatal if the
  image contract says superuser-only.
- **d7 — fail closed on version > ceiling** (serve nothing) vs. PHP web's
  `E_SCHEMA_MISMATCH` (serve the error page): chosen for a backend that
  also runs a daemon; the error message is actionable.
- **d8 — `schema.rs` in `core/`** (DB-level, next to `db.rs`) vs. top-level
  like `updates.rs`: core is the "no-HTTP" home; the startup call site is
  still just one line in `main.rs`.

## 8. Work items (when implementing)

1. Add the statement-count pin + splitter unit tests (read the real files
   from `tt-rss/sql/pgsql/`).
2. `core/schema.rs`: `split_statements` + `Migrations` (rooted at
   `<docroot>/sql`) + `apply` + `ensure_up_to_date` (advisory lock,
   pg_trgm, eager version table).
3. `main.rs`: the startup call between `Db::connect` and the daemon spawn.
4. DB tests per §5.3; wire the startup integration test.
5. Docs: update `docs/19` gap item 1 (bootstrap half done — the user verbs
   remain P0) and the `docs/01-plan.md` test-parity board.
