#!/usr/bin/env bash
# php-sync.sh — sync to tt-rss php upstream: list the commits since the
# pinned php submodule commit and which of them need porting.
#
# The submodule pin (superproject `HEAD`) is the spec the port was built
# against; this lists what php upstream has since, classified by COMMIT
# (not file): a commit touching classes/, update.php, include/, sql/ or
# .docker/ is REVIEW (port it — frontend and translations ride along with
# it; .docker/ is considered, not ported), everything else (locale/, js/,
# themes/, images/, messages.pot, upstream infra) is AUTO (a pin bump
# propagates it with no port work).
#
# Usage: make php-sync                # fetches origin, reports pin..origin/main
# See docs/21-php-upstream-sync.md.
set -euo pipefail
cd "$(dirname "$0")/.."

S=tt-rss
git -C "$S" fetch -q origin main
PIN=$(git ls-tree HEAD "$S" | awk '{print $3}')
UP=$(git -C "$S" rev-parse origin/main)

if [ "$PIN" = "$UP" ]; then
    echo "reference up to date at ${UP:0:7}"
    exit 0
fi

N=$(git -C "$S" rev-list --count "$PIN..$UP")
NM=$(git -C "$S" rev-list --count --no-merges "$PIN..$UP")
F=$(git -C "$S" diff --name-only "$PIN..$UP" | wc -l)
echo "PHP SYNC  pin=${PIN:0:7}  upstream=${UP:0:7}"
echo "  range: $N commits ($NM non-merge), $F files; merges are subsumed by their non-merge commits"
echo

review=0 auto=0
review_commits=() auto_commits=()
for c in $(git -C "$S" rev-list --reverse --no-merges "$PIN..$UP"); do
    files=$(git -C "$S" show --name-only --format= "$c")
    if echo "$files" | grep -qE '^(classes/|update\.php|include/|sql/|\.docker/)'; then
        review=$((review+1)); review_commits+=("$c")
    else
        auto=$((auto+1)); auto_commits+=("$c")
    fi
done

echo "== REVIEW ($review commits) — port these, with test parity =="
for c in "${review_commits[@]:-}"; do
    [ -n "$c" ] || continue
    echo
    git -C "$S" show --format='commit %h %s' "$c"
done

echo
echo "== AUTO ($auto commits) — pin bump only, no port work =="
for c in "${auto_commits[@]:-}"; do
    [ -n "$c" ] || continue
    git -C "$S" log -1 --format='  %h %s' "$c"
done
