#!/bin/sh -e
# Restore the golden dataset into a fresh oracle stack:
#   1. down + wipe db volume
#   2. up (app re-initializes schema v151 + admin user)
#   3. stop the updater daemon (corpus runs need a frozen dataset)
#   4. restore golden-db.sql
#   5. re-import the cached disk state (feed icons)
cd "$(dirname "$0")/../.."   # repo root

DATA=oracle/data
PROJ=tt-rust-oracle
C="docker compose -f oracle/compose.yaml"

$C down --remove-orphans
docker volume rm "${PROJ}_db-data" 2>/dev/null || true

oracle/scripts/up.sh

$C stop updater

echo "== restore db =="
$C exec -T db psql -U postgres -d postgres -v ON_ERROR_STOP=0 \
    < "$DATA/golden-db.sql" > /tmp/restore.log 2>&1 || true
tail -5 /tmp/restore.log

echo "== restore cache =="
rm -rf /tmp/oracle-cache
mkdir -p /tmp/oracle-cache
cp -r "$DATA/cache/." /tmp/oracle-cache/ 2>/dev/null || true
$C cp /tmp/oracle-cache/. app:/var/www/html/tt-rss/cache/ 2>/dev/null || true

echo "golden dataset restored."
