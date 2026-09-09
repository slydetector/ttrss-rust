<?php
// Recorder for the `clean`/`clean_str` differential test fixture
// (crates/server/tests/fixtures/clean_str.json).
//
// Drives the real `clean()` (tt-rss/include/functions.php) —
// `trim(strip_tags($v))` for strings — over the fixture's "in" battery and
// writes the "out" fields back as the PHP ground truth. This exercises the
// Rust's hand-rolled `strip_tags`/`php_trim` against PHP's, including
// malformed-tag and entity edge cases.
//
// Run via `make record-params` (public php:8.5-cli, frozen tt-rss/ mounted).
//
error_reporting(E_ALL & ~E_DEPRECATED);
require '/tt-rss/vendor/autoload.php';

$fixture = $argv[1] ?? '/fx/clean_str.json';
$cases   = json_decode(file_get_contents($fixture), true);
if (!is_array($cases)) {
    fwrite(STDERR, "cannot read fixture: $fixture\n");
    exit(1);
}

$out = ["["];
$n = count($cases);
foreach (array_values($cases) as $i => $c) {
    $r = clean($c['in']);
    $comma = ($i === $n - 1) ? '' : ',';
    $out[] = sprintf(
        '  { "in": %s, "out": %s }%s',
        json_encode($c['in']),
        json_encode($r, JSON_UNESCAPED_UNICODE),
        $comma
    );
}
$out[] = ']';
file_put_contents($fixture, implode("\n", $out) . "\n");
echo "wrote $n cases to $fixture\n";
