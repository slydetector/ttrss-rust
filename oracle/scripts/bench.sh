#!/bin/sh -e
# Measure PHP-FPM baseline latency for the key endpoints.
# Logs in (web + API), then times a fixed set of representative requests.
# Output: oracle/data/baseline.json (per-endpoint min/mean/max ms over N runs).
cd "$(dirname "$0")/../.."   # repo root

BASE="http://127.0.0.1:$(grep '^HTTP_PORT=' oracle/.env | cut -d= -f2)/tt-rss"
OUT=oracle/data/baseline.json
N=5

CJ=$(mktemp)
trap 'rm -f "$CJ" "$CJ.api" /tmp/bench_otp.$$' EXIT

# --- log in (admin, OTP) ---
curl -s -c "$CJ" -o /dev/null "$BASE/index.php"
OTP=$(python3 - <<'PY'
import hmac, hashlib, struct, time
c = int(time.time()) // 30
h = hmac.new(b'JBSWY3DPEHPK3PXP', struct.pack('>Q', c), hashlib.sha1).digest()
o = h[19] & 0xf
print(f'{((h[o]&0x7f)<<24 | (h[o+1]<<16) | (h[o+2]<<8) | h[o+3]) % 1000000:06d}')
PY
)
curl -s -b "$CJ" -c "$CJ" -o /dev/null \
    --data "op=login&login=admin&password=admin&otp=$OTP" \
    "$BASE/public.php?return=$BASE/index.php"

# --- log in (API, app password) ---
API_SID=$(curl -s -X POST "$BASE/api/index.php" -H "Content-Type: application/json" \
    -d '{"op":"login","user":"admin","password":"AppPass123"}' \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["content"]["session_id"])')

timed() {  # timed <name> <curl args...>
    name=$1; shift
    samples=""
    i=0
    while [ $i -lt $N ]; do
        ms=$(curl -s -o /dev/null -w '%{time_total}' "$@" | python3 -c 'import sys; print(int(float(sys.stdin.read())*1000))')
        samples="$samples $ms"
        i=$((i+1))
    done
    echo "$name|$samples"
}

{
    echo "== web endpoints (logged in) =="
    timed login_page      "$BASE/index.php" -b "$CJ"
    timed shell           "$BASE/index.php" -b "$CJ"
    timed prefs_shell     "$BASE/prefs.php" -b "$CJ"
    timed feedtree        -X POST "$BASE/backend.php" -b "$CJ" \
        --data "op=Pref_Feeds&method=getfeedtree&csrf_token=$(grep -oE '__csrf_token = "[a-f0-9]{32}"' <(curl -s -b "$CJ" "$BASE/index.php") | cut -d'"' -f2)"
    timed headlines       -X POST "$BASE/backend.php" -b "$CJ" \
        --data "op=Feeds&method=view&feed=1&csrf_token=$(grep -oE '__csrf_token = "[a-f0-9]{32}"' <(curl -s -b "$CJ" "$BASE/index.php") | cut -d'"' -f2)"
    timed search          -X POST "$BASE/backend.php" -b "$CJ" \
        --data "op=Feeds&method=search&q=kernel&csrf_token=$(grep -oE '__csrf_token = "[a-f0-9]{32}"' <(curl -s -b "$CJ" "$BASE/index.php") | cut -d'"' -f2)"

    echo "== api endpoints (session $API_SID) =="
    timed api_counters    -X POST "$BASE/api/index.php" -H "Content-Type: application/json" \
        -d "{\"op\":\"getCounters\",\"sid\":\"$API_SID\"}"
    timed api_headlines   -X POST "$BASE/api/index.php" -H "Content-Type: application/json" \
        -d "{\"op\":\"getHeadlines\",\"feed_id\":1,\"limit\":50,\"sid\":\"$API_SID\"}"
    timed api_feeds       -X POST "$BASE/api/index.php" -H "Content-Type: application/json" \
        -d "{\"op\":\"getFeeds\",\"sid\":\"$API_SID\"}"
} > /tmp/bench_raw.$$

python3 - /tmp/bench_raw.$$ "$OUT" <<'PY'
import json, sys, re
raw = open(sys.argv[1]).read()
out = {}
current = None
for line in raw.splitlines():
    if line.startswith("== "):
        current = line[3:].split(" (")[0]
        out[current] = []
        continue
    if "|" in line:
        name, samples = line.split("|", 1)
        vals = [int(s) for s in samples.split()]
        out[current].append({
            "endpoint": name,
            "samples_ms": vals,
            "min_ms": min(vals), "mean_ms": round(sum(vals)/len(vals), 1), "max_ms": max(vals),
        })
json.dump(out, open(sys.argv[2], "w"), indent=2)
print("wrote", sys.argv[2])
PY
rm -f /tmp/bench_raw.$$
