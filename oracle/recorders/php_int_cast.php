<?php
// Recorder for the `php_int_cast` differential test fixture
// (crates/server/tests/fixtures/php_int_cast.json).
//
// Records PHP's `(int)` cast — the leading-integer-prefix, whitespace-trimming
// truncation — over the fixture's "in" battery and writes the "out" fields
// back. This exercises the Rust's `php_int_cast` approximation against PHP's.
//
// Run via `make record-params` (public php:8.5-cli, frozen tt-rss/ mounted).
//
error_reporting(E_ALL & ~E_DEPRECATED);
require '/tt-rss/vendor/autoload.php';

$fixture = $argv[1] ?? '/fx/php_int_cast.json';
$cases   = json_decode(file_get_contents($fixture), true);
if (!is_array($cases)) {
    fwrite(STDERR, "cannot read fixture: $fixture\n");
    exit(1);
}

$out = ["["];
$n = count($cases);
foreach (array_values($cases) as $i => $c) {
    $r = (int) $c['in'];
    $comma = ($i === $n - 1) ? '' : ',';
    $out[] = sprintf(
        '  { "in": %s, "out": %d }%s',
        json_encode($c['in']),
        $r,
        $comma
    );
}
$out[] = ']';
file_put_contents($fixture, implode("\n", $out) . "\n");
echo "wrote $n cases to $fixture\n";
