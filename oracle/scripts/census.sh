#!/bin/sh -e
# Census: summarize the request distribution seen by the oracle nginx, so the
# corpus/bench can be weighted toward what the real UI actually does.
# Usage: census.sh [minutes-back]
cd "$(dirname "$0")/../.."   # repo root

BACK=${1:-}
docker compose -f oracle/compose.yaml logs web $([ -n "$BACK" ] && echo "--since ${BACK}m") 2>/dev/null \
    | grep -oE '"[A-Z]+ [^ "]+ HTTP/[0-9.]+" [0-9]+ [0-9]+' \
    | awk '{
        if (match($0, /^"[A-Z]+ ([^ "]+) HTTP\/[0-9.]+" ([0-9]+) ([0-9]+)/, m)) {
            path = m[1];
            sub(/\?.*$/, "", path);
            status = m[2];
            if (path ~ /backend\.php$/)                     b = "POST backend.php (RPC)";
            else if (path ~ /index\.php$/)                  b = "GET  index.php (shell)";
            else if (path ~ /prefs\.php$/)                  b = "GET  prefs.php (prefs shell)";
            else if (path ~ /public\.php$/ && $0 ~ /op=rss/) b = "GET  public.php?op=rss (syndication)";
            else if (path ~ /public\.php$/)                 b = "GET  public.php (other)";
            else if (path ~ /^\/api\//)                     b = "API  " path;
            else if (path ~ /\.css$/)                       b = "static css";
            else if (path ~ /\.js$/)                        b = "static js";
            else if (path ~ /\.(svg|png|jpe?g|gif|woff2?|ttf)$/) b = "static other";
            else                                            b = "other " path;
            count[b]++;
            if (status == "500") err[b]++;
        }
    }
    END {
        for (b in count)
            printf "%-45s %8d %8d\n", b, count[b], err[b]+0;
    }' | { echo "bucket                                    requests errors"; sort -k2 -rn; }
