<?php
// Recorder for the `param_int_array` differential test fixture
// (crates/ttrss/tests/fixtures/param_int_array.json).
//
// It drives the *real* frozen PHP reference — `Handler::_param_to_int_array`
// (tt-rss/classes/Handler.php), which delegates to `clean()` in
// tt-rss/include/functions.php — over the "in" battery already stored in the
// fixture, and writes the "out" fields back as the PHP ground truth.
//
// Normalization (so the fixture stores a dense integer list, the same shape
// the Rust `Vec<i32>` produces):
//   * PHP `null`  -> []   (Rust treats an absent/invalid input as empty)
//   * sparse array -> array_values() (PHP's empty token yields a hole; the
//     ordered *value list* is what the consumer actually iterates)
//
// Run it with the *public* PHP image (matching the app's PHP 8.5), mounting
// the frozen `tt-rss/` reference read-only — no oracle stack required:
//
//   make record-params
//
// or by hand:
//
//   docker run --rm \
//     -v "$PWD/tt-rss":/tt-rss:ro \
//     -v "$PWD/crates/ttrss/tests/fixtures":/fx:rw \
//     -v "$PWD/oracle/recorders":/rec:ro \
//     php:8.5-cli php /rec/param_int_array.php /fx/param_int_array.json
//
error_reporting(E_ALL & ~E_DEPRECATED);
require '/tt-rss/vendor/autoload.php';

$fixture = $argv[1] ?? '/fx/param_int_array.json';
$cases   = json_decode(file_get_contents($fixture), true);
if (!is_array($cases)) {
    fwrite(STDERR, "cannot read fixture: $fixture\n");
    exit(1);
}

$out = ["["];
$n = count($cases);
foreach (array_values($cases) as $i => $c) {
    $r = Handler::_param_to_int_array($c['in']);
    $vals = ($r === null) ? [] : array_values($r);
    $comma = ($i === $n - 1) ? '' : ',';
    $out[] = sprintf(
        '  { "in": %s, "out": %s }%s',
        json_encode($c['in']),
        json_encode(array_values($vals)),
        $comma
    );
}
$out[] = ']';
file_put_contents($fixture, implode("\n", $out) . "\n");
echo "wrote $n cases to $fixture\n";
