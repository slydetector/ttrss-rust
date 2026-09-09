<?php
// Ground-truth generator for `Pref_Filters::_feed_multi_select` (the feed
// multi-select). Replicates the method body verbatim (with `ENABLE_FEED_CATS`
// forced true) over a fixed nested dataset, so the Rust port can be diffed
// against the real PHP output.
//
// Usage:
//   FMS_PGHOST=127.0.0.1 FMS_PGPORT=32768 FMS_PGDATABASE=postgres \
//     docker run --rm -e FMS_PGHOST -e FMS_PGPORT -e FMS_PGDATABASE \
//       -v "$PWD:/w:ro" -w /w php:8.5-cli \
//       php oracle/recorders/feed_multi_select.php <ids-csv>
//
// It creates + clears two scratch tables (fms_cats / fms_feeds) and re-inserts
// the fixed dataset each run (idempotent). Drop them afterwards if desired.

const EM = "\u{2003}";
function __(string $s): string { return $s; }

$host = getenv('FMS_PGHOST') ?: '127.0.0.1';
$port = getenv('FMS_PGPORT') ?: '5432';
$db   = getenv('FMS_PGDATABASE') ?: 'postgres';
$pdo = new PDO("pgsql:host=$host;port=$port;dbname=$db", 'postgres', 'postgres',
    [PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION]);

// Fixed nested dataset:
//   categories: 1 "Zeta" (top), 2 "Alpha" (top), 3 "Mid" (child of 2), 4 "Deep" (child of 3)
//   feeds:      10 "Feed A2" (cat 2), 11 "Feed A1" (cat 2), 12 "Feed M" (cat 3),
//               13 "Feed D" (cat 4), 14 "UnCat 1" (NULL), 15 "UnCat 2" (NULL),
//               16 "TopZ" (cat 1)
$pdo->exec("CREATE TABLE IF NOT EXISTS fms_cats (id int, owner_uid int, title text, parent_cat int)");
$pdo->exec("CREATE TABLE IF NOT EXISTS fms_feeds (id int, owner_uid int, title text, cat_id int)");
$pdo->exec("DELETE FROM fms_cats");
$pdo->exec("DELETE FROM fms_feeds");
foreach ([
    [1, 1, 'Zeta', null], [2, 1, 'Alpha', null], [3, 1, 'Mid', 2], [4, 1, 'Deep', 3],
] as [$id, $uid, $title, $parent]) {
    $pdo->prepare("INSERT INTO fms_cats (id, owner_uid, title, parent_cat) VALUES (?,?,?,?)")
        ->execute([$id, $uid, $title, $parent]);
}
foreach ([
    [10, 1, 'Feed A2', 2], [11, 1, 'Feed A1', 2], [12, 1, 'Feed M', 3],
    [13, 1, 'Feed D', 4], [14, 1, 'UnCat 1', null], [15, 1, 'UnCat 2', null], [16, 1, 'TopZ', 1],
] as [$id, $uid, $title, $cat]) {
    $pdo->prepare("INSERT INTO fms_feeds (id, owner_uid, title, cat_id) VALUES (?,?,?,?)")
        ->execute([$id, $uid, $title, $cat]);
}

function feed_multi_select($pdo, $uid, $id, $default_ids, $attributes, $include_all_feeds, $root_id, $nest_level): string {
    $rv = "";
    if (!$root_id) {
        $rv .= "<select multiple=\true\" id=\"$id\" name=\"$id\" $attributes>";
        if ($include_all_feeds) {
            $is_selected = (in_array("0", $default_ids)) ? "selected=\"1\"" : "";
            $rv .= "<option $is_selected value=\"0\">".__('All feeds')."</option>";
        }
    }
    if (!$root_id) $root_id = null;
    // ENABLE_FEED_CATS = true branch.
    $sth = $pdo->prepare("SELECT id,title,
        (SELECT COUNT(id) FROM fms_cats AS c2 WHERE c2.parent_cat = fms_cats.id) AS num_children
        FROM fms_cats WHERE owner_uid = :uid AND
        (parent_cat = :root_id OR (:root_id IS NULL AND parent_cat IS NULL)) ORDER BY title");
    $sth->execute([":uid" => $uid, ":root_id" => $root_id]);
    while ($line = $sth->fetch(PDO::FETCH_ASSOC)) {
        for ($i = 0; $i < $nest_level; $i++)
            $line["title"] = EM . $line["title"];
        $is_selected = in_array("CAT:".$line["id"], $default_ids) ? "selected=\"1\"" : "";
        $rv .= sprintf("<option $is_selected value='CAT:%d'>%s</option>",
            $line["id"], htmlspecialchars($line["title"]));
        if ($line["num_children"] > 0)
            $rv .= feed_multi_select($pdo, $uid, $id, $default_ids, $attributes,
                $include_all_feeds, $line["id"], $nest_level+1);
        $f_sth = $pdo->prepare("SELECT id,title FROM fms_feeds WHERE cat_id = ? AND owner_uid = ? ORDER BY title");
        $f_sth->execute([$line['id'], $uid]);
        while ($fline = $f_sth->fetch(PDO::FETCH_ASSOC)) {
            $is_selected = (in_array($fline["id"], $default_ids)) ? "selected=\"1\"" : "";
            $fline["title"] = EM . $fline["title"];
            for ($i = 0; $i < $nest_level; $i++)
                $fline["title"] = EM . $fline["title"];
            $rv .= sprintf("<option $is_selected value='%d'>%s</option>",
                $fline["id"], htmlspecialchars($fline["title"]));
        }
    }
    if (!$root_id) {
        $is_selected = in_array("CAT:0", $default_ids) ? "selected=\"1\"" : "";
        $rv .= sprintf("<option $is_selected value='CAT:0'>%s</option>", __("Uncategorized"));
        $f_sth = $pdo->prepare("SELECT id,title FROM fms_feeds WHERE cat_id IS NULL AND owner_uid = ? ORDER BY title");
        $f_sth->execute([$uid]);
        while ($fline = $f_sth->fetch(PDO::FETCH_ASSOC)) {
            $is_selected = in_array($fline["id"], $default_ids) ? "selected=\"1\"" : "";
            $fline["title"] = EM . $fline["title"];
            for ($i = 0; $i < $nest_level; $i++)
                $fline["title"] = EM . $fline["title"];
            $rv .= sprintf("<option $is_selected value='%d'>%s</option>",
                $fline["id"], htmlspecialchars($fline["title"]));
        }
    }
    if (!$root_id) {
        $rv .= "</select>";
    }
    return $rv;
}

$default_ids = ($argv[1] ?? '') !== '' ? explode(",", $argv[1]) : [];
$attributes = 'required="1" style="width : 100%; height : 300px" dojoType="fox.form.ValidationMultiSelect"';
if (getenv('FMS_FLAT') === '1') {
    // ENABLE_FEED_CATS = false: <select> + "All feeds" + flat feed list + </select>.
    $rv = "<select multiple=\\true\" id=\"feed_id\" name=\"feed_id\" $attributes>";
    $is_selected = (in_array("0", $default_ids)) ? "selected=\"1\"" : "";
    $rv .= "<option $is_selected value=\"0\">".__('All feeds')."</option>";
    $sth = $pdo->prepare("SELECT id,title FROM fms_feeds WHERE owner_uid = ? ORDER BY title");
    $sth->execute([1]);
    while ($line = $sth->fetch(PDO::FETCH_ASSOC)) {
        $is_selected = (in_array($line["id"], $default_ids)) ? "selected=\"1\"" : "";
        $rv .= sprintf("<option $is_selected value='%d'>%s</option>", $line["id"], htmlspecialchars($line["title"]));
    }
    $rv .= "</select>";
    echo $rv, "\n";
} else {
    echo feed_multi_select($pdo, 1, "feed_id", $default_ids, $attributes, true, null, 0), "\n";
}
