#!/bin/bash
# Reset the shared Postgres to the golden baseline (for M3 mutation runs).
# Restores oracle/data/golden-db.sql into the running DB container, then
# refreshes planner stats so the natural plan order matches the golden.
set -e
cd "$(dirname "$0")/../.."

python3 oracle/scripts/check_sync.py
docker exec -i ttrss-oracle-db-1 psql -U postgres -d postgres -v ON_ERROR_STOP=0 \
    <oracle/data/golden-db.sql >/tmp/reset-db.log 2>&1
docker exec ttrss-oracle-db-1 psql -U postgres -d postgres -c "ANALYZE;" >/dev/null 2>&1
echo "DB reset to golden baseline."
docker exec ttrss-oracle-db-1 psql -U postgres -d postgres -tA \
    -c "SELECT count(*) FILTER (WHERE unread) || ' unread / ' || count(*) FILTER (WHERE NOT unread) || ' read (uid 1)' FROM ttrss_user_entries WHERE owner_uid=1;"
