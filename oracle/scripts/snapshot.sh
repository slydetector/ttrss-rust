#!/bin/sh -e
# Snapshot the current oracle state:
#   - pg_dump          -> oracle/data/golden-db.sql
#   - app cache (icons)-> oracle/data/cache/
#   - row counts/facts -> oracle/data/db-manifest.json
cd "$(dirname "$0")/../.."   # repo root

DATA=oracle/data
mkdir -p "$DATA"

BASE="http://127.0.0.1:$(grep '^HTTP_PORT=' oracle/.env | cut -d= -f2)"

echo "== pg_dump =="
docker compose -f oracle/compose.yaml exec -T db pg_dump -U postgres -d postgres \
    --no-owner --no-privileges --clean --if-exists \
    > "$DATA/golden-db.sql"
echo "wrote $DATA/golden-db.sql ($(du -h "$DATA/golden-db.sql" | cut -f1))"

echo "== app cache =="
rm -rf "$DATA/cache"
mkdir -p "$DATA/cache"
docker compose -f oracle/compose.yaml cp app:/var/www/html/tt-rss/cache/. "$DATA/cache/" 2>/dev/null || true

echo "== manifest =="
python3 oracle/scripts/make_manifest.py "$DATA" "$BASE"

echo "snapshot written to $DATA/"
