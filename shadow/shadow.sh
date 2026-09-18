#!/usr/bin/env bash
# shadow — bring up a Rust-backed "shadow" of a production tt-rss database.
#
#   preflight  check tools, shadow/.env, and prod schema drift (hard-fail)
#   up         build + db up + reset shadow + copy prod + rust up + health
#   copy       re-sync: reset shadow + re-copy prod + restart rust (no rebuild)
#   stop       stop the shadow stack (db + rust); keeps the data volume
#   db         (internal) ensure the shadow db is up + reset the shadow database
#
# Prod credentials live in the gitignored shadow/.env (see shadow/.env.example).
set -euo pipefail

SHADOW_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SHADOW_DIR/.." && pwd)"
ENV_FILE="$SHADOW_DIR/.env"
DB_CONTAINER="ttrss-shadow-db-1"

# --- defaults (overridable via shadow/.env; kept in sync with compose.yaml) ---
SHADOW_PORT="${SHADOW_PORT:-8282}"
SHADOW_DB_PORT="${SHADOW_DB_PORT:-5433}"
SHADOW_DB_USER="${SHADOW_DB_USER:-shadow}"
SHADOW_DB_PASS="${SHADOW_DB_PASS:-shadow}"
SHADOW_DB_NAME="${SHADOW_DB_NAME:-shadow}"
SHADOW_SELF_URL="${SHADOW_SELF_URL:-http://127.0.0.1:${SHADOW_PORT}/tt-rss/}"
SHADOW_ENC_KEY="${SHADOW_ENC_KEY:-9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08}"
EXPECTED_SCHEMA_VERSION="${EXPECTED_SCHEMA_VERSION:-151}"

info() { echo "==> $*"; }
die()  { echo "error: $*" >&2; exit 1; }

load_env() {
  if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
  fi
}

# psql against the local shadow db (host port SHADOW_DB_PORT); $1=db, $2=sql
shadow_psql() {
  PGPASSWORD="$SHADOW_DB_PASS" psql -h 127.0.0.1 -p "$SHADOW_DB_PORT" -U "$SHADOW_DB_USER" -d "$1" -tAc "$2"
}

# --- preflight ---------------------------------------------------------------
cmd_preflight() {
  command -v docker >/dev/null 2>&1 || die "docker not found"
  command -v pg_dump  >/dev/null 2>&1 || die "pg_dump not found"
  command -v pg_restore >/dev/null 2>&1 || die "pg_restore not found"
  [ -f "$ENV_FILE" ] || die "$ENV_FILE not found — create it from shadow/.env.example (fill in PROD_DB_*)"
  load_env
  : "${PROD_DB_HOST:?PROD_DB_HOST not set in $ENV_FILE}"
  : "${PROD_DB_USER:?PROD_DB_USER not set in $ENV_FILE}"
  : "${PROD_DB_NAME:?PROD_DB_NAME not set in $ENV_FILE}"
  : "${PROD_DB_PASS:?PROD_DB_PASS not set in $ENV_FILE}"

  # Drift check (hard-fail): prod's schema version must match the port's target.
  info "drift check: prod tt-rss schema_version (expected $EXPECTED_SCHEMA_VERSION)"
  local prod_v
  prod_v="$(PGPASSWORD="$PROD_DB_PASS" psql -h "$PROD_DB_HOST" -p "${PROD_DB_PORT:-5432}" \
    -U "$PROD_DB_USER" -d "$PROD_DB_NAME" -tAc 'SELECT schema_version FROM ttrss_version' 2>/dev/null)" \
    || die "could not read prod ttrss_version.schema_version (bad creds / not reachable / not a tt-rss DB?)"
  [ "$prod_v" = "$EXPECTED_SCHEMA_VERSION" ] \
    || die "SCHEMA DRIFT: prod schema_version=$prod_v, port expects $EXPECTED_SCHEMA_VERSION — refusing (hard-fail)"
  info "drift check OK (schema_version $prod_v)"

  # Informational: prod DB size (to gauge copy time).
  local size
  size="$(PGPASSWORD="$PROD_DB_PASS" psql -h "$PROD_DB_HOST" -p "${PROD_DB_PORT:-5432}" \
    -U "$PROD_DB_USER" -d "$PROD_DB_NAME" -tAc "SELECT pg_size_pretty(pg_database_size('$PROD_DB_NAME'))" 2>/dev/null)" || size="?"
  info "prod database size: $size"
}

# --- steps --------------------------------------------------------------------
build_image() {
  info "building the rust image (cached when the source is unchanged)"
  ( cd "$SHADOW_DIR" && docker compose build rust )
}

db_up() {
  ( cd "$SHADOW_DIR" && docker compose up -d db )
  local s=""
  for _ in $(seq 1 90); do
    s="$(docker inspect --format '{{.State.Health.Status}}' "$DB_CONTAINER" 2>/dev/null || true)"
    [ "$s" = "healthy" ] && break
    sleep 1
  done
  [ "$s" = "healthy" ] || die "shadow db did not become healthy (status: ${s:-none})"
  info "db healthy"
}

stop_rust() {
  ( cd "$SHADOW_DIR" && docker compose stop rust >/dev/null 2>&1 ) || true
}

reset_db() {
  info "resetting the shadow database ($SHADOW_DB_NAME)"
  shadow_psql postgres "DROP DATABASE IF EXISTS $SHADOW_DB_NAME;" || true
  shadow_psql postgres "CREATE DATABASE $SHADOW_DB_NAME;"
}

copy_prod() {
  [ -n "${PROD_DB_HOST:-}" ] || die "PROD_DB_HOST not set (did preflight run?)"
  local dump="$SHADOW_DIR/prod.dump"
  info "pg_dump prod ($PROD_DB_HOST) -> $dump"
  PGPASSWORD="$PROD_DB_PASS" pg_dump -h "$PROD_DB_HOST" -p "${PROD_DB_PORT:-5432}" \
    -U "$PROD_DB_USER" -d "$PROD_DB_NAME" --format=custom --no-owner --no-acl -f "$dump"
  info "pg_restore -> shadow.$SHADOW_DB_NAME"
  PGPASSWORD="$SHADOW_DB_PASS" pg_restore -h 127.0.0.1 -p "$SHADOW_DB_PORT" \
    -U "$SHADOW_DB_USER" -d "$SHADOW_DB_NAME" --no-owner "$dump"
  info "copied ($(du -h "$dump" | cut -f1))"
}

rust_up() {
  ( cd "$SHADOW_DIR" && docker compose up -d rust )
}

health_summary() {
  local ok=0
  for _ in $(seq 1 60); do
    if curl -sf "http://127.0.0.1:${SHADOW_PORT}/tt-rss/healthz" 2>/dev/null | grep -q OK; then ok=1; break; fi
    sleep 1
  done
  [ "$ok" = 1 ] || die "shadow server did not answer healthz on :$SHADOW_PORT"
  echo
  echo "shadow is serving: $SHADOW_SELF_URL"
  echo "  re-sync from prod: make shadow-copy | stop: make shadow-stop"
}

# --- subcommands --------------------------------------------------------------
cmd_up() {
  load_env
  build_image
  db_up
  stop_rust
  reset_db
  copy_prod
  rust_up
  health_summary
}

cmd_copy() {
  load_env
  stop_rust
  reset_db
  copy_prod
  rust_up
  health_summary
}

cmd_stop() {
  ( cd "$SHADOW_DIR" && docker compose down )
  echo "shadow stack stopped (data volume kept)"
}

cmd_db() {
  load_env
  db_up
  reset_db
  echo "shadow db ready ($SHADOW_DB_NAME, port $SHADOW_DB_PORT)"
}

case "${1:-}" in
  preflight) cmd_preflight ;;
  up)        cmd_up ;;
  copy)      cmd_copy ;;
  stop)      cmd_stop ;;
  db)        cmd_db ;;
  *) echo "usage: $(basename "$0") {preflight|up|copy|stop|db}" >&2; exit 2 ;;
esac
