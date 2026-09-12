#!/bin/sh -e
# Re-record the param differential fixtures from the frozen PHP reference.
#
# These drive the *real* PHP helpers (the frozen tt-rss reference) over a
# front-end-shaped input battery and record the ground truth as a fixture the
# Rust tests compare against. They run in the *public* PHP image matching the
# app's PHP 8.5 — no oracle stack required. See oracle/recorders/.
cd "$(dirname "$0")/../.." # repo root

PHP_IMAGE=${PHP_IMAGE:-php:8.5-cli}
for r in param_int_array param_to_bool clean_str php_int_cast; do
    docker run --rm \
        -v "$PWD/tt-rss":/tt-rss:ro \
        -v "$PWD/crates/ttrss/tests/fixtures":/fx:rw \
        -v "$PWD/oracle/recorders":/rec:ro \
        "$PHP_IMAGE" php "/rec/$r.php" "/fx/$r.json"
done
