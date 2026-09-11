#!/bin/bash
# Run the corpus against the Rust server and diff it against a baseline.
#
# This is the canonical way to drive the record/replay harness for the Rust
# port. It:
#   1. resets the oracle DB to the golden baseline (reset-db.sh), so every run
#      starts from the same frozen state — the corpus is STATEFUL (logins,
#      Feeds::view pref-writes, readonly/OTP logins), and without the reset
#      mutations accumulate across runs and the diff drifts;
#   2. builds the server + harness;
#   3. starts the Rust server on 127.0.0.1:8281 (connected to the oracle DB);
#   4. runs the full scenario suite;
#   5. diffs the result against a baseline run dir;
#   6. always tears the server back down.
#
# NOTE: the harness is used for READ-ONLY ops for now. The write/mutating
# steps (login, logout, Feeds::view, ...) are stateful and are diffed only
# against a matching-baseline oracle run; they will get dedicated write-op
# verification later. The DB reset keeps each run reproducible regardless.
#
# Usage:
#   oracle/scripts/harness.sh [options]
#     --baseline <dir>      baseline run dir to diff against
#                           (default oracle/data/runs/oracle-a)
#     --scenarios <file>    scenario template
#                           (default oracle/corpus/scenarios.template.toml)
#     --vars <file>         pre-seeded vars (default oracle/data/ids.json)
#     --out <dir>           where to write the Rust run's fixtures
#                           (default a fresh mktemp dir under /tmp)
#     --no-build            skip the cargo build (reuse existing binaries)
#     --no-reset            do NOT reset the oracle DB to the golden baseline
#     --keep                leave the Rust server running after the run
#
# Requires the oracle stack to be up (oracle/scripts/up.sh) so the Rust server
# has a DB to connect to. DB creds / plugins / the encryption key are read from
# oracle/.env (the single source of truth); the Rust server's own listen port,
# self URL, docroot, and feed-icon dir are fixed here.
set -e
cd "$(dirname "$0")/../.." # repo root

BASELINE=oracle/data/runs/oracle-a
SCENARIOS=oracle/corpus/scenarios.template.toml
VARS=oracle/data/ids.json
OUT=""
DO_BUILD=1
DO_RESET=1
KEEP=0
BASE_PORT=8281
BASE_URL="http://127.0.0.1:${BASE_PORT}/tt-rss/"

while [ $# -gt 0 ]; do
    case "$1" in
    --baseline)
        BASELINE="$2"
        shift 2
        ;;
    --scenarios)
        SCENARIOS="$2"
        shift 2
        ;;
    --vars)
        VARS="$2"
        shift 2
        ;;
    --out)
        OUT="$2"
        shift 2
        ;;
    --no-build)
        DO_BUILD=0
        shift
        ;;
    --no-reset)
        DO_RESET=0
        shift
        ;;
    --keep)
        KEEP=1
        shift
        ;;
    -h | --help)
        grep '^#' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown arg: $1 (see --help)" >&2
        exit 2
        ;;
    esac
done

# --- pull the shared values from oracle/.env (single source of truth) ---
env_get() { grep -E "^$1=" oracle/.env | head -1 | cut -d= -f2-; }
DB_USER=$(env_get TTRSS_DB_USER)
DB_NAME=$(env_get TTRSS_DB_NAME)
DB_PASS=$(env_get TTRSS_DB_PASS)
PLUGINS=$(env_get TTRSS_PLUGINS)
ENC_KEY=$(env_get TTRSS_ENCRYPTION_KEY)
for v in DB_USER DB_NAME DB_PASS PLUGINS ENC_KEY; do
    [ -n "${!v}" ] || {
        echo "oracle/.env is missing $v" >&2
        exit 1
    }
done

# --- the golden baseline must match the current seed recipe ---
python3 oracle/scripts/check_sync.py

# --- build ---
if [ "$DO_BUILD" = 1 ]; then
    echo "building server + harness ..."
    cargo build -q -p ttrss-server --bin tt-rust -p tt-rust-harness
fi

# --- the oracle DB must be reachable ---
if ! (exec 3<>"/dev/tcp/127.0.0.1/5432") 2>/dev/null; then
    echo "oracle DB not reachable at 127.0.0.1:5432 — run oracle/scripts/up.sh first" >&2
    exit 1
fi
exec 3>&- 3<&- 2>/dev/null || true

# --- reset the oracle DB to the golden baseline (reproducible runs) ---
if [ "$DO_RESET" = 1 ]; then
    echo "resetting oracle DB to the golden baseline ..."
    oracle/scripts/reset-db.sh
fi

# --- start the Rust server ---
[ -n "$OUT" ] || OUT=$(mktemp -d /tmp/rust-run-XXXXXX)
SERVER_LOG=$(mktemp /tmp/rust-server-XXXXXX.log)
export TTRSS_DB_HOST=127.0.0.1
export TTRSS_DB_PORT=5432
export TTRSS_DB_USER="$DB_USER"
export TTRSS_DB_NAME="$DB_NAME"
export TTRSS_DB_PASS="$DB_PASS"
export TTRSS_PLUGINS="$PLUGINS"
export TTRSS_ENCRYPTION_KEY="$ENC_KEY"
export TTRSS_SELF_URL_PATH="$BASE_URL"
export TTRSS_RUST_LISTEN="127.0.0.1:${BASE_PORT}"
export TTRSS_RUST_DOCROOT="$PWD/tt-rss"
export TTRSS_RUST_FEED_ICONS_DIR="$PWD/oracle/data/feed-icons"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

./target/debug/tt-rust >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
cleanup() {
    if [ "$KEEP" = 0 ]; then kill "$SERVER_PID" 2>/dev/null || true; fi
    # Preserve the server log (incl. any panic backtrace) in the run dir so a
    # failed/panicked step can be inspected. Keep the mktemp path too.
    if [ -n "$OUT" ] && [ -f "$SERVER_LOG" ]; then
        mv -f "$SERVER_LOG" "$OUT/server.log"
        echo "server log: $OUT/server.log" >&2
    fi
}
trap cleanup EXIT

# wait for healthz (bounded)
ready=0
for _ in $(seq 1 40); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "${BASE_URL}healthz" 2>/dev/null || true)
    if [ "$code" = "200" ]; then
        ready=1
        break
    fi
    sleep 0.5
done
if [ "$ready" != 1 ]; then
    echo "Rust server did not become ready (base: $BASE_URL). Log:" >&2
    cat "$SERVER_LOG" >&2
    exit 1
fi
echo "Rust server up: $BASE_URL (pid $SERVER_PID)"
[ "$KEEP" = 1 ] && echo "(--keep: leaving it running; kill $SERVER_PID to stop)"

# --- run the corpus (steps that 404 still write fixtures, so continue) ---
echo "running corpus -> $OUT"
set +e
./target/debug/tt-rust-harness run \
    --base-url "$BASE_URL" \
    --scenarios "$SCENARIOS" \
    --vars "$VARS" \
    --out "$OUT"
RUN_RC=$?
set -e
[ "$RUN_RC" = 0 ] || echo "(run reported $RUN_RC step(s) failing; fixtures were still written)"

# --- diff against the baseline ---
echo "diffing $BASELINE vs $OUT"
./target/debug/tt-rust-harness diff --a "$BASELINE" --b "$OUT"
