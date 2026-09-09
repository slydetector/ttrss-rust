<?php
// Recorder for the `param_to_bool` differential test fixture
// (crates/server/tests/fixtures/param_to_bool.json).
//
// Drives the real `Handler::_param_to_bool` (tt-rss/classes/Handler.php) —
// `clean($p) && ($p !== "f" && $p !== "false")` — over the fixture's "in"
// battery and writes the "out" fields back as the PHP ground truth.
//
// Run via `make record-params` (public php:8.5-cli, frozen tt-rss/ mounted).
//
error_reporting(E_ALL & ~E_DEPRECATED);
require '/tt-rss/vendor/autoload.php';

$fixture = $argv[1] ?? '/fx/param_to_bool.json';
$cases   = json_decode(file_get_contents($fixture), true);
if (!is_array($cases)) {
    fwrite(STDERR, "cannot read fixture: $fixture\n");
    exit(1);
}

$out = ["["];
$n = count($cases);
foreach (array_values($cases) as $i => $c) {
    $bool = Handler::_param_to_bool($c['in']);
    $comma = ($i === $n - 1) ? '' : ',';
    $out[] = sprintf(
        '  { "in": %s, "out": %s }%s',
        json_encode($c['in']),
        json_encode($bool),
        $comma
    );
}
$out[] = ']';
file_put_contents($fixture, implode("\n", $out) . "\n");
echo "wrote $n cases to $fixture\n";
