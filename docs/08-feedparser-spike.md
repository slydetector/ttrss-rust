# Feed parser spike: `uppsala` vs `roxmltree`

Status: **complete (2026-09-11)** · Decision: **use `roxmltree`**

This spike decided which XML library to port Tiny Tiny RSS's `FeedParser.php`
onto for M6a. The working crate (`spike/feedparser/`, standalone, `publish =
false`) was removed after the decision; this document keeps the findings and
the decision.

## Question

PHP's `FeedParser` + `FeedItem_*` classes extract feed metadata by mixing two
mechanisms:

- **~32 `DOMXPath->query("…")` calls** — a real XPath 1.0 engine.
- **~15 `getElementsByTagName("…")` calls** — a DOM method (not XPath).

Candidates:

- **`uppsala` 0.10** — an XML parser *plus* a real XPath 1.0 engine, so PHP's
  `->query("…")` expressions port verbatim.
- **`roxmltree` 0.21** — a pure tree with **no query language**; every query is
  re-implemented as manual `children()`/`descendants()`/`attribute()` walks.

## Method

Both libraries were driven against the **same 14 fixtures** (the discriminating
cases from the PHP test suite) and compared against the PHP-expected value for
**27 fields** covering the genuinely discriminating behaviors:

- Feed type detection: RSS, Atom 1.0, Atom 0.3, RDF.
- `rel=self` / `rel=alternate` link filtering and link ordering.
- `xml:base` relative-URL rewriting.
- Namespaced fields: `dc:creator`, `thr:total`, `slash:comments`,
  `media:content` / `media:title` / `media:thumbnail`, nested `media:group`,
  `atom:content`.

## Result: correctness is *not* the differentiator

| case | field | expected | uppsala | roxmltree |
| --- | --- | --- | --- | --- |
| rss_valid | type / title / link / items | RSS / … / … / 2 | OK | OK |
| atom_default | type / title / link / items | ATOM / … / … / 2 | OK | OK |
| alt_link | item_link | …/entry/alternate | OK | OK |
| atom03 | type / title / items | ATOM / … / 1 | OK | OK |
| rdf | type / title / link / items | RDF / … / … / 2 | OK | OK |
| atom_link_order | item_link | …/first | OK | OK |
| atom_xml_base | item_link | <http://base.example/entry/relative> | OK | OK |
| atom_replies | comments_url | …/atom/2/replies | OK | OK |
| atom_thread | comments_count | 17 | OK | OK |
| rss_slash | comments_count | 99 | OK | OK |
| rss_dc_creator | author | Jane Doe | OK | OK |
| media_group | enc_link / enc_type / enc_title | … / … / … | OK | OK |
| media_thumb | enc_url | …/thumb.jpg | OK | OK |
| atom_entity | content | Entity body | OK | OK |

**27/27 on both libraries.** Both can reproduce the PHP extraction.

## The real difference is the extraction model

### `uppsala` — verbatim XPath, with two gaps

Verified working: `//`, `/`, union `|`, predicates, `not()`, `contains()`,
`and`, `string()`, `ancestor-or-self::`, and `.` (current node).

Two gaps found:

1. **`@xml:base` needs the `xml` prefix registered.** PHP's `DOMXPath`
   auto-binds the `xml` prefix; `uppsala` does not. The verbatim expression
   `string(ancestor-or-self::*[@xml:base][1]/@xml:base)` returns `""` until
   you call `evaluator.add_namespace("xml", "http://www.w3.org/XML/1998/namespace")`.
   Trivial one-line fix.
2. **`(union)/child` paths don't parse.** A `/step` after a parenthesized union
   → `Unexpected trailing tokens`. The bare union parses fine, but PHP's
   thumbnail query
   `(.|media:content|media:group|media:group/media:content)/media:thumbnail`
   does not — the port must split it into per-branch queries.

### `roxmltree` — manual walks, no query-engine gaps

Every extraction is a hand walk: type detection (element name + namespace),
namespaced fields, the `xml:base` ancestor walk, the thumbnail union, and the
`getElementsByTagName` equivalents. No query-engine coverage to depend on.

Caveat: the spike matched by **local name only** (sufficient for unambiguous
fixtures). A faithful port must disambiguate by **namespace** where PHP does
(e.g. `dc:creator` = local name `creator` + DC namespace).

## Decision: `roxmltree`

`roxmltree` is chosen because it removes the XPath-engine-coverage variable.
The two `uppsala` gaps are exactly the edge cases where a verbatim-XPath port
silently diverges from the PHP spec, and `roxmltree` is the more battle-tested
dependency for a long-lived port. Owning the extraction logic means the 1:1
ported PHP tests pin *our* code rather than a third-party engine's quirks.

What `roxmltree` **fills**: the XML parser + tree model (PHP's *DOM* half). It
deliberately has **no query language**, so it is *not* the DOMXPath half — the
"which node / what value" logic is written by hand on top of the tree.

`uppsala` remains a viable alternative if the goal shifts to minimizing hand
code (it ports the ~32 query calls verbatim, modulo the two workarounds).

## Fixtures note

Two fixture bugs were found and fixed during the spike (not library bugs):

- The raw-string fixtures opened with `r#"\n<?xml…`, putting the XML
  declaration on line 2. Per the XML spec the declaration must be on line 1, so
  **both** parsers correctly rejected it (`uppsala`: "Processing instruction
  target must not be 'xml'"; `roxmltree`: "unexpected XML declaration").
- `RSS_SLASH` used the `slash:` prefix without declaring the namespace.

The `<?xml?>` declarations were dropped (they exercised nothing); the leading
newline before the root element is harmless.
