--
-- Test baseline: the minimal state PHP's own integration bootstrap produces
-- on top of the base schema (sql/pgsql/schema.sql in the frozen reference):
--
--   `update.php --update-schema=force-yes` on a fresh database runs
--   `Db_Migrations::migrate()`: version -1 → migrate_to(0) applies the base
--   schema file and records `schema_version = <max>` (SCHEMA_VERSION, 151) in
--   the migrations table (`ttrss_version`). The base schema file does not
--   create that table itself. No numbered migrations run on a fresh DB.
--
--   The port's bootstrap (`core::schema`) also runs `CREATE EXTENSION
--   IF NOT EXISTS pg_trgm` after the base schema (best-effort: a warning,
--   not a failure, when the extension is unavailable — the test Postgres
--   images have it, so the fixture is a hard statement, mirroring the
--   common path).
--
-- NOTE: the initial admin user (uid 1, login 'admin', password 'password' —
-- pwd_hash 'SHA1:5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8', i.e. SHA1 of
-- "password" with no salt, access_level 10 = ACCESS_LEVEL_ADMIN) is created
-- by schema.sql itself; do not add it here.

CREATE TABLE IF NOT EXISTS ttrss_version (schema_version integer not null);
INSERT INTO ttrss_version (schema_version) VALUES (151);
CREATE EXTENSION IF NOT EXISTS pg_trgm;
