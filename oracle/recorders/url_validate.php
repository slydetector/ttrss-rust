<?php
// Recorder for the `UrlHelper::validate` differential test fixture
// (crates/ttrss/tests/fixtures/url_validate.json).
//
// Drives the real `UrlHelper::validate` (tt-rss/classes/UrlHelper.php) over the
// fixture's "in" battery, with each case's "ext" as the `extended_filtering`
// argument, and writes the "out" fields back as the PHP ground truth.
// `false` (validation failure) is encoded as JSON `false`; a validated URL
// string is encoded as the JSON string.
//
// Run via the real tt-rss oracle image (which ships intl / idn_to_ascii, so
// the IDNA path is exercised) with the frozen tt-rss/ mounted:
//   docker run --rm --entrypoint php \
//       -v "$PWD/tt-rss":/tt-rss:ro \
//       -v "$PWD/crates/ttrss/tests/fixtures":/fx:rw \
//       -v "$PWD/oracle/recorders":/rec:ro \
//       ghcr.io/tt-rss/tt-rss:sha-2f7d831 /rec/url_validate.php /fx/url_validate.json
//
error_reporting(E_ALL & ~E_DEPRECATED);
require '/tt-rss/vendor/autoload.php';

$fixture = $argv[1] ?? '/fx/url_validate.json';
$cases   = json_decode(file_get_contents($fixture), true);
if (!is_array($cases)) {
    fwrite(STDERR, "cannot read fixture: $fixture\n");
    exit(1);
}

$out = ["["];
$n = count($cases);
foreach (array_values($cases) as $i => $c) {
    $res = UrlHelper::validate($c['in'], $c['ext']);
    $comma = ($i === $n - 1) ? '' : ',';
    $out[] = sprintf(
        '  { "in": %s, "ext": %s, "out": %s }%s',
        json_encode($c['in']),
        json_encode((bool)$c['ext']),
        json_encode($res),
        $comma
    );
}
$out[] = ']';
file_put_contents($fixture, implode("\n", $out) . "\n");
echo "wrote $n cases to $fixture\n";
