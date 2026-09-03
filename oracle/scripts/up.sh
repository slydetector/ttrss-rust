#!/bin/sh -e
# Bring up the oracle stack and wait until it is ready.
cd "$(dirname "$0")/.."

docker compose up -d --remove-orphans

BASE="http://127.0.0.1:$(grep '^HTTP_PORT=' .env | cut -d= -f2)/tt-rss"

echo "waiting for $BASE/healthz ..."
for i in $(seq 1 120); do
    if resp=$(curl -sf "$BASE/healthz" 2>/dev/null) && [ "$resp" = "OK" ]; then
        echo "oracle is up: $BASE (healthz: $resp)"
        exit 0
    fi
    sleep 2
done

echo "oracle did not become ready in time; recent logs:" >&2
docker compose logs --tail=40 >&2 || true
exit 1
