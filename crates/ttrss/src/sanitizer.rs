//! Port of `tt-rss/classes/Sanitizer.php::sanitize`.
//!
//! Security-critical: parse the raw article HTML fragment, rewrite links/images
//! (URL validation, `rel`/`target`, `referrerpolicy`/`loading`, `srcset`),
//! sandbox iframes, strip disallowed elements/attributes, and re-serialize.
//!
//! Parsing uses the canonical `html5ever` tree (the same family the harness uses
//! for semantic HTML comparison), so entity spelling and inter-tag whitespace
//! need not match libxml byte-for-byte — only the *structure* does.
//!
//! Robustness: every traversal and the serializer are **iterative** (heap
//! stacks), not recursive, so a malicious/deeply-nested feed cannot overflow
//! the call stack.

use html5ever::{LocalName, Namespace, QualName};
use kuchikiki::traits::TendrilSink;
use kuchikiki::{parse_html, ElementData, NodeData, NodeRef};
use regex::Regex;
use std::collections::BTreeSet;

use crate::pluginhost::host::HostCtx;
use crate::pluginhost::{HOOK_IFRAME_WHITELISTED, HOOK_SANITIZE};
use crate::with_trailing_slash;

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";

fn html_ns() -> Namespace {
    Namespace::from(HTML_NS)
}

fn qualname(name: &str) -> QualName {
    QualName::new(None, html_ns(), LocalName::from(name.to_string()))
}

/// The element's local name as a String (Atom doesn't compare to &str directly).
fn el_name(el: &ElementData) -> String {
    el.name.local.to_string()
}

/// `Sanitizer::$allowed_elements` (Sanitizer.php). `iframe` is added when the
/// client has a sandbox (see `sanitize`).
const ALLOWED_ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "acronym",
    "audio",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "body",
    "br",
    "caption",
    "cite",
    "center",
    "code",
    "col",
    "colgroup",
    "data",
    "dd",
    "del",
    "details",
    "description",
    "dfn",
    "div",
    "dl",
    "font",
    "dt",
    "em",
    "footer",
    "figure",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "html",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "noscript",
    "ol",
    "p",
    "picture",
    "pre",
    "q",
    "ruby",
    "rp",
    "rt",
    "s",
    "samp",
    "section",
    "small",
    "source",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "track",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
    "video",
    "xml:namespace",
];

/// `Sanitizer::$disallowed_attributes` (Sanitizer.php).
const DISALLOWED_ATTRIBUTES: &[&str] =
    &["id", "style", "class", "width", "height", "allow", "srcdoc"];

/// HTML void elements (no closing tag) — for the serializer.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// `UrlHelper::EXTRA_HREF_SCHEMES`.
const EXTRA_HREF_SCHEMES: &[&str] = &["magnet", "mailto", "tel"];

/// `UrlHelper::EXTRA_SCHEMES_BY_CONTENT_TYPE`.
const EXTRA_SCHEMES_BY_CONTENT_TYPE: &[(&str, &[&str])] =
    &[("application/x-bittorrent", &["magnet"])];

/// Data-URI allowlist for `<img src>` (PHP regex on the URL *path*):
/// `^image/(webp|gif|jpg|png|svg);base64,`.
static DATA_URI_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

fn data_uri_re() -> &'static Regex {
    DATA_URI_RE.get_or_init(|| Regex::new(r"^image/(webp|gif|jpg|png|svg);base64,").unwrap())
}

/// The allowed-elements set (lowercased), optionally including `iframe`.
fn allowed_elements(has_sandbox: bool) -> BTreeSet<String> {
    let mut s: BTreeSet<String> = ALLOWED_ELEMENTS.iter().map(|s| s.to_string()).collect();
    if has_sandbox {
        s.insert("iframe".to_string());
    }
    s
}

/// Find the `<body>` element of a full-document parse (the fragment is placed
/// there, mirroring PHP's `DOMDocument::loadHTML`). Exposed for
/// `diskcache::rewrite_urls`.
pub(crate) fn find_body(doc: &NodeRef) -> Option<NodeRef> {
    for html in doc.children() {
        if let Some(el) = html.as_element() {
            if el_name(el) == "html" {
                for c in html.children() {
                    if let Some(b) = c.as_element() {
                        if el_name(b) == "body" {
                            return Some(c);
                        }
                    }
                }
            }
        }
    }
    None
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Build an element with named attributes (kuchikiki stores names as the
/// IndexMap key; `new_element` takes bare `Attribute`s, so insert by name).
fn build_element(name: &str, attrs: &[(String, String)]) -> NodeRef {
    let el = NodeRef::new_element(qualname(name), vec![]);
    for (k, v) in attrs {
        el.as_element()
            .unwrap()
            .attributes
            .borrow_mut()
            .insert(k.clone(), v.clone());
    }
    el
}

/// Elements that close libxml2's implicit `<p>` (block-level, i.e. not
/// phrasing content in the HTML DTD).
const IMPLICIT_P_BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "div",
    "dd",
    "dl",
    "dt",
    "fieldset",
    "figure",
    "figcaption",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

/// Emulate libxml2 (PHP `DOMDocument::loadHTML`) implicit markup handling,
/// which the spec-compliant html5ever parser does not reproduce:
///
/// 1. Leading whitespace-only text nodes and comments before the first
///    significant child of `<body>` are dropped.
/// 2. If the first significant child is text containing a non-whitespace
///    character (an entity counts as text), libxml2 opens an implicit `<p>`
///    (leading whitespace of that text node is trimmed) which absorbs all
///    following children until a block-level element starts.
pub fn apply_libxml_implicit_p(body: &NodeRef) {
    // 1. Collect leading droppable nodes.
    let mut lead: Vec<NodeRef> = Vec::new();
    let mut cur = body.first_child();
    while let Some(n) = cur.clone() {
        let drop = n
            .as_text()
            .map(|t| t.borrow().trim().is_empty())
            .unwrap_or(false)
            || n.as_comment().is_some();
        if !drop {
            break;
        }
        let next = n.next_sibling();
        lead.push(n);
        cur = next;
    }
    for n in &lead {
        n.detach();
    }

    // 2. Implicit `<p>` when the first significant node is non-blank text.
    let first = match cur {
        Some(n) => n,
        None => return,
    };
    let text_node = match first.as_text() {
        Some(t) => {
            let original = t.borrow().to_string();
            let trimmed_start = original.trim_start_matches(char::is_whitespace);
            if trimmed_start.is_empty() {
                first.detach();
                return;
            }
            if trimmed_start != original {
                // Drop leading whitespace only (trailing is kept).
                *t.borrow_mut() = trimmed_start.to_string();
            }
            first.clone()
        }
        None => return,
    };

    let p = NodeRef::new_element(qualname("p"), vec![]);
    body.prepend(p.clone());
    let mut cur = Some(text_node);
    while let Some(n) = cur {
        let is_block = n
            .as_element()
            .map(|el| IMPLICIT_P_BLOCK_ELEMENTS.contains(&lower(&el_name(el)).as_str()))
            .unwrap_or(false);
        if is_block {
            break;
        }
        cur = n.next_sibling();
        n.detach();
        p.append(n);
    }
}

/// The sanitized fragment HTML — the sync **core** (M7.6): parse, rewrite
/// links/images, sandbox + wrap iframes, strip disallowed elements/attributes,
/// highlight words. `admitted` is the M7.11 `HOOK_IFRAME_WHITELISTED` host set —
/// an iframe whose src host is in it keeps the loose PHP sandbox value, the rest
/// get the restrictive one. Production read paths call [`sanitize`] (which
/// computes the set by firing the hook, runs this core, then the `HOOK_SANITIZE`
/// stage); the parity/unit tests drive this core directly (no runtime) with
/// `admitted: None`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sanitize_core(
    input: &str,
    force_remove_images: bool,
    strip_images: bool,
    has_sandbox: bool,
    bw_limit: bool,
    site_url: Option<&str>,
    highlight_words_opt: Option<&[String]>,
    admitted: Option<&BTreeSet<String>>,
) -> String {
    let res = input.trim();
    if res.is_empty() {
        return String::new();
    }

    let doc = parse_html().one(res);
    let Some(body) = find_body(&doc) else {
        return String::new();
    };

    // html5ever routes "head" elements (e.g. `<noscript>`) into `<head>` even
    // in fragment parses, while libxml's `loadHTML` puts the whole fragment in
    // `<body>`. Move any head children to the front of body to match. (Only
    // `<noscript>` can matter — every other head element is disallowed and
    // would be stripped anyway.)
    for html in doc.children() {
        let Some(html_el) = html.as_element() else {
            continue;
        };
        if el_name(html_el) != "html" {
            continue;
        }
        for c in html.children() {
            let Some(h) = c.as_element() else {
                continue;
            };
            if el_name(h) == "head" {
                let kids: Vec<NodeRef> = c.children().collect();
                for k in kids.into_iter().rev() {
                    k.detach();
                    body.prepend(k);
                }
            }
        }
    }

    // 0. Emulate libxml2 (PHP `DOMDocument::loadHTML`) implicit markup:
    // drop leading whitespace/comments, open an implicit `<p>` for leading
    // bare text.
    apply_libxml_implicit_p(&body);

    let rewrite_base = site_url.unwrap_or("http://domain.invalid/");

    // 1. Rewrite links/images (a[@href], img/source[src|srcset], video[poster]).
    rewrite_url_elements(
        &body,
        rewrite_base,
        force_remove_images,
        strip_images,
        bw_limit,
    );

    // 2. iframes: sandbox.
    process_iframes(&body, admitted);

    // 3. Strip disallowed elements (whole subtree) + disallowed attributes.
    strip_harmful_tags(&body, &allowed_elements(has_sandbox));

    // 4. Wrap surviving iframes in <div class="embed-responsive">
    // (Sanitizer.php: the second `//iframe` pass, after strip_harmful_tags).
    wrap_iframes_in_div(&body);

    // 5. Highlight words (if the feed has any).
    if let Some(words) = highlight_words_opt {
        if !words.is_empty() {
            highlight_words(&body, words);
        }
    }

    // Serialize the body's children (the fragment), iteratively.
    let mut out = String::new();
    for child in body.children() {
        serialize_node(&child, &mut out);
    }
    out
}

/// Pass 1: rewrite `href`/`src`/`srcset`/`poster`, decorate `<a>`/`<img>`,
/// replace elements whose source is invalid, and strip images on request.
fn rewrite_url_elements(
    root: &NodeRef,
    base: &str,
    force_remove_images: bool,
    strip_images: bool,
    bw_limit: bool,
) {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        // Compute the condition in a scoped block so the element/attribute
        // borrows drop before we mutate the tree in rewrite_single_element.
        // The PHP xpath: (//a[@href]|//img[@src|@srcset]|//source[@src|@srcset]
        // |//video[@poster]).
        let needs_rewrite = if let Some(el) = node.as_element() {
            let name = lower(&el_name(el));
            let has = |k: &str| el.attributes.borrow().get(k).is_some();
            match name.as_str() {
                "a" => has("href"),
                "img" | "source" => has("src") || has("srcset"),
                "video" => has("poster"),
                _ => false,
            }
        } else {
            false
        };
        if needs_rewrite {
            rewrite_single_element(&node, base, force_remove_images, strip_images, bw_limit);
        }
        for c in node.children() {
            stack.push(c);
        }
    }
}

/// Rewrite a single link/image element in place (or replace it).
fn rewrite_single_element(
    node: &NodeRef,
    base: &str,
    force_remove_images: bool,
    strip_images: bool,
    bw_limit: bool,
) {
    let el = match node.as_element() {
        Some(e) => e,
        None => return,
    };
    let name = lower(&el_name(el));

    // Read all URL-bearing attributes up front (as owned Strings) so no shared
    // Ref outlives the borrow_mut()s below.
    let attrs = el.attributes.borrow();
    let href = attrs.get("href").map(|s| s.to_string());
    let src = attrs.get("src").map(|s| s.to_string());
    let srcset = attrs.get("srcset").map(|s| s.to_string());
    let poster = attrs.get("poster").map(|s| s.to_string());
    drop(attrs);

    // href (a)
    if let Some(href) = href {
        // PHP: `setAttribute('href', rewrite_relative(...))` — a failed
        // rewrite coerces `false` to the empty string (href="").
        match rewrite_relative(base, &href, &name, "href", "") {
            Some(u) => {
                el.attributes.borrow_mut().insert("href", libxml_space_encode(&u));
            }
            None => {
                el.attributes.borrow_mut().insert("href", String::new());
            }
        }
        el.attributes
            .borrow_mut()
            .insert("rel", "noopener noreferrer".to_string());
        el.attributes
            .borrow_mut()
            .insert("target", "_blank".to_string());
    }

    let mut should_replace_element = false;
    let mut src_valid = true;

    if let Some(src) = src {
        match rewrite_relative(base, &src, &name, "src", "") {
            Some(u) if u.to_lowercase().starts_with("data:") => {
                el.attributes.borrow_mut().insert("src", libxml_space_encode(&u));
            }
            Some(u) if !has_disallowed_ip(&u) => {
                el.attributes.borrow_mut().insert("src", libxml_space_encode(&u));
            }
            _ => {
                should_replace_element = true;
                src_valid = false;
            }
        }
    }

    if let Some(srcset) = srcset {
        let mut validated: Vec<(String, String)> = Vec::new();
        for (url, size) in decode_srcset(&srcset) {
            if let Some(u) = rewrite_relative(base, &url, &name, "srcset", "") {
                if !has_disallowed_ip(&u) {
                    validated.push((u, size));
                }
            }
        }
        if !validated.is_empty() {
            el.attributes
                .borrow_mut()
                .insert("srcset", encode_srcset(&validated));
            should_replace_element = false;
        } else {
            el.attributes.borrow_mut().remove("srcset");
        }
    }

    // Replace with escaped text if 'src' and 'srcset' are both invalid.
    if should_replace_element {
        let mut html = String::new();
        serialize_node(node, &mut html);
        if node.parent().is_some() {
            node.insert_before(NodeRef::new_text(html));
            node.detach();
        }
        return;
    }

    // Drop 'src' if invalid and the element wasn't replaced.
    if !src_valid {
        el.attributes.borrow_mut().remove("src");
    }

    if name == "img" {
        el.attributes
            .borrow_mut()
            .insert("referrerpolicy", "no-referrer".to_string());
        el.attributes
            .borrow_mut()
            .insert("loading", "lazy".to_string());
    }

    if let Some(poster) = poster {
        match rewrite_relative(base, &poster, &name, "poster", "") {
            Some(u) if !has_disallowed_ip(&u) => {
                el.attributes.borrow_mut().insert("poster", u);
            }
            _ => {
                el.attributes.borrow_mut().remove("poster");
            }
        }
    }

    // STRIP_IMAGES / bw_limit / force_remove_images: replace the image with a
    // <p><a href=src>src</a></p>.
    let has_src = el.attributes.borrow().get("src").is_some();
    if has_src && (strip_images || force_remove_images || bw_limit) {
        let src = el
            .attributes
            .borrow()
            .get("src")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let a = build_element(
            "a",
            &[
                ("href".into(), src.clone()),
                ("target".into(), "_blank".into()),
                ("rel".into(), "noopener noreferrer".into()),
            ],
        );
        a.append(NodeRef::new_text(src));
        let p = build_element("p", &[]);
        p.append(a);
        if name == "source" {
            // replace the parent (picture) with the <p>
            if let Some(picture) = node.parent() {
                picture.insert_before(p);
                picture.detach();
            }
        } else {
            node.insert_before(p);
            node.detach();
        }
    }
}

/// Pass 2: sandbox iframes.
fn process_iframes(root: &NodeRef, admitted: Option<&BTreeSet<String>>) {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element() {
            if el_name(el) == "iframe" {
                // M7.11 — an iframe whose src host a plugin admitted via
                // `HOOK_IFRAME_WHITELISTED` (in `admitted`) keeps the loose
                // PHP sandbox value (the M5a.3 residual); the rest get the
                // restrictive one (Sanitizer.php). The `allow` attribute the
                // reference also sets is stripped later (a disallowed attr).
                let src = el.attributes
                    .borrow()
                    .get("src")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let sandbox = if admitted.is_some_and(|s| s.contains(&url_host(&src))) {
                    "allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox"
                } else {
                    "allow-scripts"
                };
                el.attributes.borrow_mut().insert("sandbox", sandbox.to_string());
            }
        }
        for c in node.children() {
            stack.push(c);
        }
    }
}

/// `Sanitizer::iframe_whitelisted` — true when a plugin hooks
/// `HOOK_IFRAME_WHITELISTED` for the iframe's `src` host (the PHP passes
/// `parse_url($src, PHP_URL_HOST)` — the host, which `af_youtube_embed` matches
/// against its `YOUTUBE_HOSTS` list). A `src` with no host can't be a
/// whitelisted iframe, so it short-circuits to false.
pub async fn iframe_whitelisted(plugins: &mut HostCtx<'_>, src: &str) -> bool {
    let host = url_host(src);
    if host.is_empty() {
        return false;
    }
    for idx in plugins.hook_order(HOOK_IFRAME_WHITELISTED) {
        if plugins
            .host()
            .plugin(idx)
            .hook_iframe_whitelisted(plugins, &host)
            .await
        {
            return true;
        }
    }
    false
}

/// The iframe `src` attributes of a fragment (sync — the parse `Document` is
/// dropped before return, so no [`NodeRef`] escapes into the async context).
/// M7.11 uses this to compute the `HOOK_IFRAME_WHITELISTED` decisions without
/// holding a (non-`Send`) parse tree across an `await`.
fn collect_iframe_srcs(input: &str) -> Vec<String> {
    let doc = parse_html().one(input.trim());
    let Some(body) = find_body(&doc) else {
        return Vec::new();
    };
    let mut stack = vec![body.clone()];
    let mut out = Vec::new();
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element() {
            if el_name(el) == "iframe" {
                if let Some(src) = el.attributes.borrow().get("src") {
                    out.push(src.to_string());
                }
            }
        }
        for c in node.children() {
            stack.push(c);
        }
    }
    out
}

/// `parse_url($src, PHP_URL_HOST)` — the iframe src's host (IPv6 brackets
/// stripped, `""` when absent). Split out so the plugin system (M7.6/M7.11)
/// can feed it to `HOOK_IFRAME_WHITELISTED`.
pub(crate) fn url_host(src: &str) -> String {
    parse_url(src)
        .and_then(|p| p.host)
        .map(|h| h.trim_matches(|c| c == '[' || c == ']').to_string())
        .unwrap_or_default()
}

/// The `HOOK_SANITIZE` stage (M7.6): every active plugin that registered
/// the hook gets the sanitized fragment (`std::mem::take` — no clone),
/// mutates it, and the result is written back in hook order. PHP mutates
/// the shared `DOMDocument` in place; the composition is the same (each
/// hook sees the previous hooks' output). `site_url`/`allowed_elements`/
/// `disallowed_attributes`/`article_id` are the fixed hook contract
/// (M7.1); a `None` site URL is passed as `""` (PHP's `null` → `''` in the
/// hook arguments is not reachable here — the call sites pass the feed
/// site URL or `None` → PHP's sanitize defaults to the base URL; the hook
/// receives what PHP's `HOOK_SANITIZE` receives, i.e. the `$site_url`
/// argument as-is).
/// The full read-side sanitize pipeline (M7.6): [`sanitize_core`] followed
/// by the `HOOK_SANITIZE` stage — every active plugin that registered the
/// hook gets the fragment (`std::mem::take` hand-off, no clone) and mutates
/// it in hook order (PHP mutates the shared `DOMDocument`; the composition
/// is the same — each hook sees the previous hooks' output). Production read
/// paths call this, not [`sanitize_core`], so the hook stage cannot be
/// skipped. `site_url: None` → `""` in the hook contract (the core uses its
/// own base-URL default for `rewrite_base`).
#[allow(clippy::too_many_arguments)] // mirrors the core's 7 + article_id + plugin ctx
pub async fn sanitize(
    input: &str,
    force_remove_images: bool,
    strip_images: bool,
    has_sandbox: bool,
    bw_limit: bool,
    site_url: Option<&str>,
    highlight_words_opt: Option<&[String]>,
    article_id: i32,
    mut plugins: Option<&mut HostCtx<'_>>,
) -> String {
    // M7.11 — compute the `HOOK_IFRAME_WHITELISTED` decisions (the admitted
    // host set) before the core runs. The srcs are plain `String`s, so no
    // (non-`Send`) parse tree is held across the await that fires the hook.
    let mut admitted: BTreeSet<String> = BTreeSet::new();
    if has_sandbox {
        if let Some(plugins) = plugins.as_mut() {
            for src in collect_iframe_srcs(input) {
                if iframe_whitelisted(plugins, &src).await {
                    admitted.insert(url_host(&src));
                }
            }
        }
    }
    let mut out = sanitize_core(
        input,
        force_remove_images,
        strip_images,
        has_sandbox,
        bw_limit,
        site_url,
        highlight_words_opt,
        Some(&admitted),
    );
    if let Some(plugins) = plugins {
        let order = plugins.hook_order(HOOK_SANITIZE);
        if !order.is_empty() {
            let site = site_url.unwrap_or("").to_string();
            let allowed: Vec<String> = ALLOWED_ELEMENTS.iter().map(|s| s.to_string()).collect();
            let disallowed: Vec<String> = DISALLOWED_ATTRIBUTES.iter().map(|s| s.to_string()).collect();
            for idx in order {
                let mut d = std::mem::take(&mut out);
                plugins.host().plugin(idx).hook_sanitize(
                    plugins,
                    &mut d,
                    &site,
                    &allowed,
                    &disallowed,
                    article_id,
                ).await;
                out = d;
            }
        }
    }
    out
}

/// Wrap surviving iframes in `<div class="embed-responsive">` (Sanitizer.php:
/// the second `//iframe` pass, runs after strip_harmful_tags).
fn wrap_iframes_in_div(root: &NodeRef) {
    let mut stack = vec![root.clone()];
    let mut iframes: Vec<NodeRef> = Vec::new();
    while let Some(n) = stack.pop() {
        if n.as_element()
            .map(|e| el_name(e) == "iframe")
            .unwrap_or(false)
        {
            iframes.push(n.clone());
        }
        for c in n.children() {
            stack.push(c);
        }
    }
    for iframe in iframes {
        let div = build_element("div", &[("class".into(), "embed-responsive".into())]);
        iframe.insert_before(div.clone());
        div.append(iframe);
    }
}

/// Pass 3: drop disallowed elements (whole subtree) and disallowed attributes.
fn strip_harmful_tags(root: &NodeRef, allowed: &BTreeSet<String>) {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        let is_disallowed = if let Some(el) = node.as_element() {
            let name = lower(&el_name(el));
            let disallowed = !allowed.contains(&name);
            if !disallowed {
                drop_disallowed_attrs(el);
            }
            disallowed
        } else {
            false
        };
        if is_disallowed {
            node.detach();
            continue; // do not descend into the removed subtree
        }
        for c in node.children() {
            stack.push(c);
        }
    }
}

fn drop_disallowed_attrs(el: &ElementData) {
    let mut to_remove = Vec::new();
    for (exp_name, attr) in el.attributes.borrow().map.iter() {
        let lname = exp_name.local.to_string().to_ascii_lowercase();
        let is_on = lname.starts_with("on");
        let is_data = lname.starts_with("data-");
        let is_js_href = lname == "href" && value_starts_with_js(&attr.value);
        let in_list = DISALLOWED_ATTRIBUTES.contains(&lname.as_str());
        if is_on || is_data || is_js_href || in_list {
            to_remove.push(exp_name.clone());
        }
    }
    // IndexMap::remove is a swap-remove (it moves the last entry into the
    // vacated slot); shift_remove keeps the surviving attribute order, which
    // the PHP DOM output (and the tests) depends on.
    let mut attrs = el.attributes.borrow_mut();
    for name in &to_remove {
        attrs.map.shift_remove(name);
    }
}

fn value_starts_with_js(value: &str) -> bool {
    let v = value.trim_start().to_ascii_lowercase();
    v.starts_with("javascript:")
}

/// `Sanitizer::highlight_words_str` — highlight words in an HTML *string*
/// (PHP wraps the input in `<span>…</span>`, highlights, and returns the
/// body content; if no text node was replaced the input is returned
/// unchanged).
pub fn highlight_words_str(input: &str, words: &[String]) -> String {
    let doc = parse_html().one(format!("<span>{input}</span>"));
    let Some(body) = find_body(&doc) else {
        return input.to_string();
    };
    if highlight_words(&body, words) {
        let mut out = String::new();
        for c in body.children() {
            serialize_node(&c, &mut out);
        }
        out
    } else {
        input.to_string()
    }
}

/// Case-insensitive find (PHP `mb_stripos`): `(byte_pos, byte_len)` of the
/// first occurrence of `needle` in `hay` under Unicode case folding.
fn ci_find(hay: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let lower = hay.to_lowercase();
    if lower.len() == hay.len() {
        return lower.find(needle).map(|pos| (pos, needle.len()));
    }
    // Fallback for non-length-preserving folds: char-level, 1-to-1 folding.
    let h: Vec<char> = hay.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let hl: Vec<char> = h.iter().map(|c| c.to_lowercase().next().unwrap()).collect();
    for i in 0..=h.len().saturating_sub(n.len()) {
        if hl[i..i + n.len()] == n[..] {
            let byte_pos: usize = h[..i].iter().map(|c| c.len_utf8()).sum();
            let byte_len: usize = h[i..i + n.len()].iter().map(|c| c.len_utf8()).sum();
            return Some((byte_pos, byte_len));
        }
    }
    None
}

/// Pass 4: `Sanitizer::highlight_words` — wrap *all* case-insensitive
/// occurrences of each word in each text node in `<span class="highlight">`
/// (original case of the matched text preserved). Returns `true` if any text
/// node was replaced (the PHP `rv`). Iterative.
pub fn highlight_words(root: &NodeRef, words: &[String]) -> bool {
    let mut rv = false;
    for word in words {
        if word.is_empty() {
            continue; // PHP would loop forever on an empty word
        }
        let w = word.to_lowercase();
        // Snapshot the text nodes (PHP's `//*/text()`, re-evaluated per word
        // so splits from earlier words are visible).
        let mut stack = vec![root.clone()];
        let mut nodes: Vec<NodeRef> = Vec::new();
        while let Some(n) = stack.pop() {
            if n.as_text().is_some() {
                nodes.push(n.clone());
            }
            for c in n.children() {
                stack.push(c);
            }
        }
        for node in nodes {
            let text: String = node.as_text().unwrap().borrow().to_string();
            let mut frag: Vec<NodeRef> = Vec::new();
            let mut rest = text;
            while let Some((pos, len)) = ci_find(&rest, &w) {
                frag.push(NodeRef::new_text(rest[..pos].to_string()));
                let matched = &rest[pos..pos + len];
                let highlight = build_element("span", &[("class".into(), "highlight".into())]);
                highlight.append(NodeRef::new_text(matched.to_string()));
                frag.push(highlight);
                rest = rest[pos + len..].to_string();
            }
            if !rest.is_empty() {
                frag.push(NodeRef::new_text(rest));
            }
            // replace the node with the fragment's children (in order)
            for n in &frag {
                node.insert_before(n.clone());
            }
            node.detach();
            rv = true;
        }
    }
    rv
}

// ---------------------------------------------------------------------------
// URL validation (UrlHelper)
// ---------------------------------------------------------------------------

/// URL components, mirroring PHP `parse_url()`. Absent fields are `None`; the
/// `port` field is `Some("")` when the caller explicitly set an empty port
/// (`rewrite_relative` does), which `build_url` renders as a bare `:`.
#[derive(Debug, Clone, Default)]
struct UrlParts {
    scheme: Option<String>,
    host: Option<String>,
    port: Option<String>,
    path: Option<String>,
    query: Option<String>,
    fragment: Option<String>,
}

/// Lenient port of PHP `parse_url()` for the URL shapes the sanitizer sees.
/// Returns `None` where PHP returns `false`: empty authority after
/// `scheme://`, a non-numeric port, or a control character in the
/// scheme/authority. (PHP is otherwise lenient: spaces in hosts and relative
/// paths parse fine; the strictness lives in `validate`'s `filter_var` gate.)
fn parse_url(url: &str) -> Option<UrlParts> {
    if url.is_empty() {
        return Some(UrlParts {
            path: Some(String::new()),
            ..Default::default()
        });
    }
    let bad = url
        .chars()
        .take_while(|&c| !matches!(c, '/' | '?' | '#'))
        .any(|c| (c as u32) < 0x20 || c == '\u{7f}');
    if bad {
        return None;
    }

    // Fragment: first '#' (not inside an IPv6 bracket).
    let mut rest = url;
    let mut fragment: Option<String> = None;
    let mut in_brackets = false;
    let mut hash_pos = None;
    for (i, c) in url.char_indices() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            '#' if !in_brackets => {
                hash_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    if let Some(i) = hash_pos {
        fragment = Some(url[i + 1..].to_string());
        rest = &url[..i];
    }

    // Authority present: `scheme://...` or `//...`.
    let scheme_end = rest.find(':').filter(|&i| {
        let pre = &rest[..i];
        !pre.is_empty()
            && pre.bytes().next().unwrap().is_ascii_alphabetic()
            && pre[1..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
    });
    let has_authority = match scheme_end {
        Some(i) => rest[i + 1..].starts_with("//"),
        None => rest.starts_with("//"),
    };

    // Query: first '?' after the authority start (or in the relative part).
    let auth_start = if has_authority {
        scheme_end.map(|i| i + 3).unwrap_or(2)
    } else {
        0
    };
    let mut query: Option<String> = None;
    if let Some(i) = rest[auth_start..].find('?') {
        let qi = auth_start + i;
        query = Some(rest[qi + 1..].to_string());
        rest = &rest[..qi];
    }

    let mut parts = UrlParts::default();
    if let Some(i) = scheme_end {
        parts.scheme = Some(rest[..i].to_string());
    }

    if has_authority {
        let auth = &rest[auth_start..];
        if auth.is_empty() {
            return None; // PHP: parse_url("http://") === false
        }
        // host[:port][/path]
        let (hostport, path) = match auth.find('/') {
            Some(i) => (&auth[..i], Some(auth[i..].to_string())),
            None => (auth, None),
        };
        let (host, port) = if hostport.starts_with('[') {
            // IPv6 literal: host keeps the brackets, as PHP does.
            match hostport.find(']') {
                Some(j) => {
                    let after = &hostport[j + 1..];
                    let port = after.strip_prefix(':').map(|p| p.to_string());
                    (hostport[..=j].to_string(), port)
                }
                // PHP parses "[::1/" leniently (host "[:", port "1"): treat
                // the first ':' after '[' as the port separator.
                None => match hostport.find(':') {
                    Some(i) => (
                        hostport[..i].to_string(),
                        Some(hostport[i + 1..].to_string()),
                    ),
                    None => (hostport.to_string(), None),
                },
            }
        } else {
            match hostport.find(':') {
                Some(i) => (
                    hostport[..i].to_string(),
                    Some(hostport[i + 1..].to_string()),
                ),
                None => (hostport.to_string(), None),
            }
        };
        if let Some(port) = &port {
            if !port.is_empty() && !port.bytes().all(|b| b.is_ascii_digit()) {
                return None; // PHP: parse_url("http://x:abc/") === false
            }
        }
        parts.host = Some(host);
        parts.port = port.filter(|p| !p.is_empty());
        parts.path = path;
    } else if let Some(i) = scheme_end {
        // scheme without authority: rest after ':' is the (opaque) path.
        parts.path = Some(rest[i + 1..].to_string());
    } else if !rest.is_empty() {
        parts.path = Some(rest.to_string());
    }
    parts.query = query;
    parts.fragment = fragment;
    Some(parts)
}

/// `UrlHelper::build_url` (scheme://host[:port][path][?query][#fragment]); the
/// presence of the `port` key — even empty — emits a bare `:`.
fn build_url(p: &UrlParts) -> String {
    let mut out = format!(
        "{}://{}",
        p.scheme.as_deref().unwrap_or(""),
        p.host.as_deref().unwrap_or("")
    );
    if let Some(port) = &p.port {
        out.push(':');
        out.push_str(port);
    }
    if let Some(path) = &p.path {
        out.push_str(path);
    }
    if let Some(query) = &p.query {
        out.push('?');
        out.push_str(query);
    }
    if let Some(fragment) = &p.fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Port of `UrlHelper::validate` (non-extended). Returns the validated URL —
/// rebuilt from `parse_url` tokens, so the original scheme/host case and raw
/// (non-encoded) path characters are preserved — or `None`.
pub(crate) fn validate(url: &str) -> Option<String> {
    let url = crate::util::clean_str(url);
    let url = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url
    };
    let mut parts = parse_url(&url)?;
    let host = parts.host.as_deref().filter(|h| !h.is_empty())?;
    let scheme = parts.scheme.as_deref().unwrap_or("").to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // IDN host -> punycode (PHP: idn_to_ascii, UTS46 non-transitional).
    let host_out = if !host.is_ascii() {
        match url::Url::parse(&format!("http://{host}/")) {
            Ok(u) => u.host_str()?.to_string(),
            Err(_) => return None,
        }
    } else {
        host.to_string()
    };
    // filter_var(FILTER_VALIDATE_URL) host charset (after punycode).
    if !host_out
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return None;
    }
    // The filter_var gate runs on the path-encoded variant
    // (rawurldecode+rawurlencode per segment); a failed parse rejects.
    let mut filter_parts = parts.clone();
    if let Some(path) = &parts.path {
        filter_parts.path = Some(
            path.split('/')
                .map(|seg| rawurlencode(&rawurldecode(seg)))
                .collect::<Vec<_>>()
                .join("/"),
        );
    }
    let filter_url = build_url(&filter_parts);
    // (url::Url::parse also rejects out-of-range ports, like filter_var.)
    if url::Url::parse(&filter_url).is_err() {
        return None;
    }
    parts.host = Some(host_out);
    Some(build_url(&parts))
}

/// `rawurldecode` (PHP): decode %XX byte sequences, keep the rest.
fn rawurldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// libxml serialization quirk we must reproduce for byte-parity with PHP.
///
/// The reference sanitizer sets `href`/`src` to the value returned by
/// `rewrite_relative` — which keeps a literal space intact (verified against the
/// oracle: `rewrite_relative("…/en/", " https://…")` returns the space, not
/// `%20`). The `%20` only appears when libxml serializes the DOM: its
/// `saveHTML()` percent-encodes literal spaces in **URI attributes (`href` and
/// `src`) only** — `class`/`title`/`alt`/`data-*` and even `poster` keep literal
/// spaces. Our html5ever serializer keeps the space literal everywhere, so on a
/// feed whose href carries a stray space (WordPress emits `href=" https://…"`)
/// the output would diverge. Encode the space at the point the URL is finalized
/// so the serialized HTML matches PHP byte-for-byte. Only the space (0x20) is
/// affected; well-formed URLs have none, so this is a no-op in practice except
/// for such malformed inputs.
fn libxml_space_encode(url: &str) -> String {
    url.replace(' ', "%20")
}

/// Port of `UrlHelper::rewrite_relative`: convert a (possibly) relative URL
/// to an absolute one against `base_url`, with scheme/element exceptions, and
/// validate it. Returns `None` on failure. Mirrors the PHP algorithm —
/// including its deliberate *absence* of RFC 3986 dot-segment resolution.
pub(crate) fn rewrite_relative(
    base_url: &str,
    rel_url: &str,
    owner_element: &str,
    owner_attribute: &str,
    content_type: &str,
) -> Option<String> {
    if rel_url.is_empty() {
        return Some(base_url.to_string());
    }
    let mut rel = parse_url(rel_url)?;

    if rel.host.as_deref().filter(|h| !h.is_empty()).is_some()
        && rel.scheme.as_deref().filter(|s| !s.is_empty()).is_some()
    {
        return validate(rel_url);
    }
    if rel_url.starts_with("//") {
        return validate(&format!("https:{rel_url}"));
    }
    let scheme = rel.scheme.as_deref().unwrap_or("");
    // Extra schemes for <a href>.
    if EXTRA_HREF_SCHEMES.contains(&scheme) && owner_element == "a" && owner_attribute == "href" {
        return Some(rel_url.to_string());
    }
    // Extra schemes for links with a feed-specified content type (enclosures).
    if !content_type.is_empty()
        && EXTRA_SCHEMES_BY_CONTENT_TYPE
            .iter()
            .any(|(ct, ss)| *ct == content_type && ss.iter().any(|s| s == &scheme))
    {
        return Some(rel_url.to_string());
    }
    // Limited subset of inline base64-encoded images for <img src>.
    if scheme == "data"
        && rel
            .path
            .as_deref()
            .is_some_and(|p| data_uri_re().is_match(p))
        && owner_element == "img"
        && owner_attribute == "src"
    {
        return Some(rel_url.to_string());
    }

    // Relative: borrow scheme/host/port from the base, resolve the path as
    // PHP does (dirname() of the base path, "./" stripping, no dot-segments).
    let base = parse_url(base_url).unwrap_or_default();
    rel.scheme = Some(base.scheme.clone().unwrap_or_default());
    rel.host = Some(base.host.clone().unwrap_or_default());
    rel.port = Some(base.port.clone().unwrap_or_default());
    if let Some(path) = rel.path.as_deref().filter(|p| !p.is_empty()) {
        let mut base_path = base.path.clone().unwrap_or_default();
        if !base_path.ends_with('/') {
            base_path = with_trailing_slash(&php_dirname(&base_path));
        }
        if let Some(stripped) = path.strip_prefix("./") {
            rel.path = Some(format!("{base_path}{stripped}"));
        } else if !path.starts_with('/') {
            rel.path = Some(format!("{base_path}{path}"));
        }
    }
    validate(&build_url(&rel))
}

/// PHP `dirname()` for URL paths: "/feed" -> "/", "/feed/" -> "/",
/// "feed" -> ".", "" -> "", "/" -> "/".
fn php_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => {
            if path.starts_with('/') {
                "/".to_string()
            } else if path.is_empty() {
                String::new()
            } else {
                ".".to_string()
            }
        }
    }
}

/// `rawurlencode` (PHP): encode everything except `A-Za-z0-9_.~-`.
fn rawurlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Port of `UrlHelper::has_disallowed_ip` (no DNS resolution). Returns true
/// if the URL's host is a disallowed IP: localhost; reserved ranges
/// (0/8, 127/8, 169.254/16, 224/4, broadcast — any port); private ranges
/// (10/8, 172.16/12, 192.168/16 — non-standard ports only); IPv6 loopback /
/// unspecified / fe80::/10 (any port) and fc00::/7 ULA (non-standard ports).
/// IP literals are canonicalized the way PHP does (`filter_var(
/// FILTER_VALIDATE_IP)` first, then octal/hex/decimal `canonicalize_ipv4_
/// literal`) so `0177.0.0.1` / `0x7f.0.0.1` / `2130706433` all mean 127.0.0.1.
fn has_disallowed_ip(url: &str) -> bool {
    let parts = match parse_url(url) {
        Some(p) => p,
        None => return false, // PHP: parse_url false -> empty host -> false
    };
    let host = match parts.host {
        Some(h) if !h.is_empty() => h,
        _ => return false,
    };
    let host = host
        .to_ascii_lowercase()
        .trim_matches(|c| c == '[' || c == ']' || c == '.')
        .to_string();
    let port = parts.port.as_deref();
    let is_standard_port = port.map(|p| p == "80" || p == "443").unwrap_or(true);

    if host.is_empty() || host == "localhost" {
        return true;
    }

    let mut ips: Vec<std::net::IpAddr> = Vec::new();
    if let Some(ip) = strict_ip_literal(&host) {
        ips.push(ip);
    } else if let Some(ip) = canonicalize_ipv4_literal(&host) {
        ips.push(std::net::IpAddr::V4(ip));
    }
    // No DNS resolution in the sanitizer context (validate_resolved_ip=false);
    // with no IPs found PHP returns !$validate_resolved_ip == false.
    if ips.is_empty() {
        return false;
    }

    for ip in ips {
        match ip {
            std::net::IpAddr::V4(v4) => {
                if v4_disallowed(&v4, is_standard_port) {
                    return true;
                }
            }
            std::net::IpAddr::V6(v6) => {
                let o = v6.octets();
                // ::1 (loopback), :: (unspecified), fe80::/10 (link-local)
                if v6.is_loopback()
                    || v6.is_unspecified()
                    || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
                {
                    return true;
                }
                // Unwrap IPv4-mapped IPv6 (::ffff:a.b.c.d) and re-check as v4
                // (PHP: inet_pton/inet_ntop + "::ffff:" prefix).
                if o[0..10] == [0u8; 10] && o[10] == 0xff && o[11] == 0xff {
                    let v4 = std::net::Ipv4Addr::new(o[12], o[13], o[14], o[15]);
                    if v4_disallowed(&v4, is_standard_port) {
                        return true;
                    }
                    continue;
                }
                // IPv6 Unique Local Addresses (fc00::/7), non-standard ports.
                if !is_standard_port && (o[0] & 0xfe) == 0xfc {
                    return true;
                }
            }
        }
    }
    false
}

/// PHP `filter_var($host, FILTER_VALIDATE_IP)`: strict dotted-quad (no
/// leading zeros) or an IPv6 literal.
fn strict_ip_literal(host: &str) -> Option<std::net::IpAddr> {
    if host.contains('.') && !host.contains(':') {
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() == 4
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.len() <= 3
                    && p.bytes().all(|b| b.is_ascii_digit())
                    && (p.len() == 1 || !p.starts_with('0'))
                    && p.parse::<u8>().is_ok()
            })
        {
            let v4: [u8; 4] = parts
                .iter()
                .map(|p| p.parse::<u8>().unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .ok()?;
            return Some(std::net::IpAddr::V4(std::net::Ipv4Addr::from(v4)));
        }
        return None;
    }
    host.parse::<std::net::Ipv6Addr>()
        .map(std::net::IpAddr::V6)
        .ok()
}

/// `UrlHelper::canonicalize_ipv4_literal`: dotted IPv4 literals that may use
/// octal/hex/decimal syntax ("0177.0.0.1", "0x7f.0.0.1", "2130706433").
fn canonicalize_ipv4_literal(host: &str) -> Option<std::net::Ipv4Addr> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(0x[0-9a-fA-F]+|\d+)(\.(0x[0-9a-fA-F]+|\d+)){0,3}$").unwrap()
    });
    if !re.is_match(host) {
        return None;
    }
    let parts: Vec<&str> = host.split('.').collect();
    let n = parts.len();
    let mut val: u64 = 0;
    for (i, part) in parts.iter().enumerate() {
        let octet: u64 =
            if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).ok()?
            } else {
                part.parse::<u64>().ok()?
            };
        let max = if i == n - 1 {
            (1 << (8 * (5 - n))) - 1
        } else {
            255
        };
        if octet > max {
            return None;
        }
        if i == n - 1 {
            val = (val << (8 * (5 - n))) | octet;
        } else {
            val = (val << 8) | octet;
        }
    }
    Some(std::net::Ipv4Addr::from(val as u32))
}

/// v4 reserved/private checks (PHP FILTER_FLAG_NO_RES_RANGE / NO_PRIV_RANGE).
fn v4_disallowed(v4: &std::net::Ipv4Addr, is_standard_port: bool) -> bool {
    let o = v4.octets();
    // Reserved (any port): 0.0.0.0/8, 127/8, 169.254.0.0/16, 224.0.0.0/4,
    // 255.255.255.255.
    if o[0] == 0 || o[0] == 127 || (o[0] == 169 && o[1] == 254) || o[0] >= 224 {
        return true;
    }
    // Private (non-standard ports only): 10/8, 172.16/12, 192.168/16.
    if !is_standard_port
        && (o[0] == 10 || (o[0] == 172 && (8..=15).contains(&o[1])) || (o[0] == 192 && o[1] == 168))
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// srcset (RSSUtils::decode_srcset / encode_srcset)
// ---------------------------------------------------------------------------

/// Decode a `srcset` into `(url, size)` pairs.
fn decode_srcset(srcset: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in srcset.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (url, size) = match part.rfind(' ') {
            Some(i) => {
                let (u, s) = part.split_at(i);
                (u.trim(), s.trim())
            }
            None => (part, ""),
        };
        out.push((url.to_string(), size.to_string()));
    }
    out
}

/// Encode `(url, size)` pairs back to a `srcset` string: `trim(url) + ' ' +
/// trim(size)` joined by `,`.
fn encode_srcset(items: &[(String, String)]) -> String {
    items
        .iter()
        .map(|(u, s)| format!("{} {}", u.trim(), s.trim()))
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------------
// Iterative serializer
// ---------------------------------------------------------------------------

enum SerItem {
    Visit(NodeRef),
    Close(String),
}

/// Test-only accessor for the iterative serializer.
#[cfg(test)]
pub(crate) fn serialize_for_tests(node: &NodeRef, out: &mut String) {
    serialize_node(node, out)
}

/// Serialize a node (and descendants) to HTML, iteratively (heap stack).
/// Exposed for `diskcache::rewrite_urls` (same re-serialization path).
pub(crate) fn serialize_node(node: &NodeRef, out: &mut String) {
    let mut stack = vec![SerItem::Visit(node.clone())];
    while let Some(item) = stack.pop() {
        match item {
            SerItem::Close(name) => {
                out.push_str("</");
                out.push_str(&name);
                out.push('>');
            }
            SerItem::Visit(n) => match n.data() {
                NodeData::Element(el) => {
                    let name = el.name.local.to_string();
                    write_start_tag(el, &name, out);
                    if !VOID_ELEMENTS.contains(&name.as_str()) {
                        stack.push(SerItem::Close(name));
                        let children: Vec<NodeRef> = n.children().collect();
                        for c in children.into_iter().rev() {
                            stack.push(SerItem::Visit(c));
                        }
                    }
                }
                NodeData::Text(t) => escape_text(&t.borrow(), out),
                NodeData::Comment(c) => {
                    out.push_str("<!--");
                    out.push_str(&c.borrow());
                    out.push_str("-->");
                }
                _ => {}
            },
        }
    }
}

fn write_start_tag(el: &ElementData, name: &str, out: &mut String) {
    out.push('<');
    out.push_str(name);
    let attrs = el.attributes.borrow();
    for (exp_name, attr) in attrs.map.iter() {
        out.push(' ');
        if let Some(prefix) = &attr.prefix {
            out.push_str(prefix);
            out.push(':');
        }
        out.push_str(&exp_name.local);
        out.push('=');
        // libxml quote selection: a value containing `"` but not `'`
        // is single-quoted (with `"` raw); otherwise double-quoted with
        // `"` escaped as `&quot;` (`'` is never escaped).
        let value = &attr.value;
        let single = value.contains('"') && !value.contains('\'');
        out.push(if single { '\'' } else { '"' });
        escape_attr(value, out, !single);
        out.push(if single { '\'' } else { '"' });
    }
    out.push('>');
}

// libxml2's HTML named-entity table, as used by PHP `DOMDocument::saveHTML()`
// (the oracle's serializer), derived from the frozen PHP reference (243 entries:
// ISO-8859-1 + Latin-1 Supplement + Latin Ext-A + Greek + HTML4 "special" entities).
const HTML_NAMED_ENTITIES: &[(u32, &str)] = &[
    (160, "nbsp"),
    (161, "iexcl"),
    (162, "cent"),
    (163, "pound"),
    (164, "curren"),
    (165, "yen"),
    (166, "brvbar"),
    (167, "sect"),
    (168, "uml"),
    (169, "copy"),
    (170, "ordf"),
    (171, "laquo"),
    (172, "not"),
    (173, "shy"),
    (174, "reg"),
    (175, "macr"),
    (176, "deg"),
    (177, "plusmn"),
    (178, "sup2"),
    (179, "sup3"),
    (180, "acute"),
    (181, "micro"),
    (182, "para"),
    (183, "middot"),
    (184, "cedil"),
    (185, "sup1"),
    (186, "ordm"),
    (187, "raquo"),
    (188, "frac14"),
    (189, "frac12"),
    (190, "frac34"),
    (191, "iquest"),
    (192, "Agrave"),
    (193, "Aacute"),
    (194, "Acirc"),
    (195, "Atilde"),
    (196, "Auml"),
    (197, "Aring"),
    (198, "AElig"),
    (199, "Ccedil"),
    (200, "Egrave"),
    (201, "Eacute"),
    (202, "Ecirc"),
    (203, "Euml"),
    (204, "Igrave"),
    (205, "Iacute"),
    (206, "Icirc"),
    (207, "Iuml"),
    (208, "ETH"),
    (209, "Ntilde"),
    (210, "Ograve"),
    (211, "Oacute"),
    (212, "Ocirc"),
    (213, "Otilde"),
    (214, "Ouml"),
    (215, "times"),
    (216, "Oslash"),
    (217, "Ugrave"),
    (218, "Uacute"),
    (219, "Ucirc"),
    (220, "Uuml"),
    (221, "Yacute"),
    (222, "THORN"),
    (223, "szlig"),
    (224, "agrave"),
    (225, "aacute"),
    (226, "acirc"),
    (227, "atilde"),
    (228, "auml"),
    (229, "aring"),
    (230, "aelig"),
    (231, "ccedil"),
    (232, "egrave"),
    (233, "eacute"),
    (234, "ecirc"),
    (235, "euml"),
    (236, "igrave"),
    (237, "iacute"),
    (238, "icirc"),
    (239, "iuml"),
    (240, "eth"),
    (241, "ntilde"),
    (242, "ograve"),
    (243, "oacute"),
    (244, "ocirc"),
    (245, "otilde"),
    (246, "ouml"),
    (247, "divide"),
    (248, "oslash"),
    (249, "ugrave"),
    (250, "uacute"),
    (251, "ucirc"),
    (252, "uuml"),
    (253, "yacute"),
    (254, "thorn"),
    (255, "yuml"),
    (338, "OElig"),
    (339, "oelig"),
    (352, "Scaron"),
    (353, "scaron"),
    (376, "Yuml"),
    (402, "fnof"),
    (710, "circ"),
    (732, "tilde"),
    (913, "Alpha"),
    (914, "Beta"),
    (915, "Gamma"),
    (916, "Delta"),
    (917, "Epsilon"),
    (918, "Zeta"),
    (919, "Eta"),
    (920, "Theta"),
    (921, "Iota"),
    (922, "Kappa"),
    (923, "Lambda"),
    (924, "Mu"),
    (925, "Nu"),
    (926, "Xi"),
    (927, "Omicron"),
    (928, "Pi"),
    (929, "Rho"),
    (931, "Sigma"),
    (932, "Tau"),
    (933, "Upsilon"),
    (934, "Phi"),
    (935, "Chi"),
    (936, "Psi"),
    (937, "Omega"),
    (945, "alpha"),
    (946, "beta"),
    (947, "gamma"),
    (948, "delta"),
    (949, "epsilon"),
    (950, "zeta"),
    (951, "eta"),
    (952, "theta"),
    (953, "iota"),
    (954, "kappa"),
    (955, "lambda"),
    (956, "mu"),
    (957, "nu"),
    (958, "xi"),
    (959, "omicron"),
    (960, "pi"),
    (961, "rho"),
    (962, "sigmaf"),
    (963, "sigma"),
    (964, "tau"),
    (965, "upsilon"),
    (966, "phi"),
    (967, "chi"),
    (968, "psi"),
    (969, "omega"),
    (977, "thetasym"),
    (978, "upsih"),
    (982, "piv"),
    (8194, "ensp"),
    (8195, "emsp"),
    (8201, "thinsp"),
    (8204, "zwnj"),
    (8205, "zwj"),
    (8206, "lrm"),
    (8207, "rlm"),
    (8211, "ndash"),
    (8212, "mdash"),
    (8216, "lsquo"),
    (8217, "rsquo"),
    (8218, "sbquo"),
    (8220, "ldquo"),
    (8221, "rdquo"),
    (8222, "bdquo"),
    (8224, "dagger"),
    (8225, "Dagger"),
    (8226, "bull"),
    (8230, "hellip"),
    (8240, "permil"),
    (8242, "prime"),
    (8243, "Prime"),
    (8249, "lsaquo"),
    (8250, "rsaquo"),
    (8254, "oline"),
    (8260, "frasl"),
    (8364, "euro"),
    (8465, "image"),
    (8472, "weierp"),
    (8476, "real"),
    (8482, "trade"),
    (8501, "alefsym"),
    (8592, "larr"),
    (8593, "uarr"),
    (8594, "rarr"),
    (8595, "darr"),
    (8596, "harr"),
    (8629, "crarr"),
    (8656, "lArr"),
    (8657, "uArr"),
    (8658, "rArr"),
    (8659, "dArr"),
    (8660, "hArr"),
    (8704, "forall"),
    (8706, "part"),
    (8707, "exist"),
    (8709, "empty"),
    (8711, "nabla"),
    (8712, "isin"),
    (8713, "notin"),
    (8715, "ni"),
    (8719, "prod"),
    (8721, "sum"),
    (8722, "minus"),
    (8727, "lowast"),
    (8730, "radic"),
    (8733, "prop"),
    (8734, "infin"),
    (8736, "ang"),
    (8743, "and"),
    (8744, "or"),
    (8745, "cap"),
    (8746, "cup"),
    (8747, "int"),
    (8756, "there4"),
    (8764, "sim"),
    (8773, "cong"),
    (8776, "asymp"),
    (8800, "ne"),
    (8801, "equiv"),
    (8804, "le"),
    (8805, "ge"),
    (8834, "sub"),
    (8835, "sup"),
    (8836, "nsub"),
    (8838, "sube"),
    (8839, "supe"),
    (8853, "oplus"),
    (8855, "otimes"),
    (8869, "perp"),
    (8901, "sdot"),
    (8968, "lceil"),
    (8969, "rceil"),
    (8970, "lfloor"),
    (8971, "rfloor"),
    (9001, "lang"),
    (9002, "rang"),
];

/// `DOMDocument::saveHTML()` text-node serialization: `&`, `<`, `>` are
/// entity-escaped; non-ASCII characters become the HTML named entity (when
/// one exists in the table above) or a decimal numeric reference; `NUL`
/// becomes a space and other C0 controls are dropped (libxml behavior).
fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x80 {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '\0' => out.push(' '),
                '\t' | '\n' | '\r' => out.push(c),
                _ if cp < 0x20 => {}
                _ => out.push(c),
            }
        } else {
            push_html_char(c, out);
        }
    }
}

/// A single character >= 0x80 in libxml's HTML serializer form: the named
/// HTML entity when the table has one, else a decimal numeric reference.
fn push_html_char(c: char, out: &mut String) {
    let cp = c as u32;
    match HTML_NAMED_ENTITIES.binary_search_by(|&(p, _)| p.cmp(&cp)) {
        Ok(i) => {
            out.push('&');
            out.push_str(HTML_NAMED_ENTITIES[i].1);
            out.push(';');
        }
        Err(_) => out.push_str(&format!("&#{cp};")),
    }
}
fn escape_attr(s: &str, out: &mut String, escape_quote: bool) {
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x80 {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' if escape_quote => out.push_str("&quot;"),
                '\0' => out.push(' '),
                '\t' | '\n' | '\r' => out.push(c),
                _ if cp < 0x20 => {}
                _ => out.push(c),
            }
        } else {
            push_html_char(c, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn san(html: &str) -> String {
        sanitize_core(html, false, false, true, false, None, None, None)
    }

    #[test]
    fn libxml_implicit_p() {
        // Bare text → wrapped in `<p>` (libxml2 behavior; input is
        // trimmed first, like PHP's `trim($str)`).
        assert_eq!(san("plain text article"), "<p>plain text article</p>");
        assert_eq!(san("line one\nline two"), "<p>line one\nline two</p>");
        assert_eq!(san("  text  "), "<p>text</p>");
        // Leading comments dropped; text still opens `<p>`.
        assert_eq!(san("<!-- c -->text"), "<p>text</p>");
        assert_eq!(san("   <div>x</div>"), "<div>x</div>");
        // Inline elements first → no implicit `<p>`.
        assert_eq!(san("<b>a</b>text<b>c</b>"), "<b>a</b>text<b>c</b>");
        // Text first → `<p>` absorbs inline nodes until a block starts.
        assert_eq!(
            san("text <b>x</b> <div>d</div> tail"),
            "<p>text <b>x</b> </p><div>d</div> tail"
        );
        assert_eq!(
            san("text <i>i</i> <p>p2</p>"),
            "<p>text <i>i</i> </p><p>p2</p>"
        );
        // `<br>` does not close the implicit `<p>`.
        assert_eq!(san("text<br>more"), "<p>text<br>more</p>");
        // Already-blocked content is untouched.
        assert_eq!(san("<p>already</p>"), "<p>already</p>");
        assert_eq!(
            san("<div>a</div>  <div>b</div>"),
            "<div>a</div>  <div>b</div>"
        );
        assert_eq!(san("  <p>leading p</p>  "), "<p>leading p</p>");
    }

    #[test]
    fn rewrites_links_and_strips_attrs() {
        let out = san("<p><a href=\"https://a.com/x\" class=\"foo\">link</a></p>");
        assert!(out.contains("rel=\"noopener noreferrer\""), "got: {out}");
        assert!(out.contains("target=\"_blank\""), "got: {out}");
        assert!(!out.contains("class="), "class should be stripped: {out}");
    }

    #[test]
    fn decorates_images() {
        let out = san("<img src=\"https://a.com/x.jpg\" width=\"100\">");
        assert!(out.contains("referrerpolicy=\"no-referrer\""), "got: {out}");
        assert!(out.contains("loading=\"lazy\""), "got: {out}");
        assert!(!out.contains("width="), "width should be stripped: {out}");
    }

    #[test]
    fn drops_disallowed_elements_whole_subtree() {
        let out = san("<p>keep</p><script>alert(1)</script>");
        assert!(out.contains("<p>keep</p>"), "got: {out}");
        assert!(!out.contains("script"), "got: {out}");
    }

    #[test]
    fn strips_javascript_href() {
        let out = san("<a href=\"javascript:alert(1)\">x</a>");
        assert!(!out.contains("javascript:"), "got: {out}");
        assert!(out.contains("rel=\"noopener noreferrer\""), "got: {out}");
    }

    /// NOTE(port): the serializer re-encodes characters to HTML entities
    /// exactly like PHP `DOMDocument::saveHTML()` (libxml), so an entity in
    /// the input comes out as the *same* entity in the output (the earlier
    /// port decoded to a literal `’`, which the syndication feed diff caught).
    #[test]
    fn entity_roundtrip_matches_php() {
        let out = san("<p>a&rsquo;b &amp; c</p>");
        assert!(out.contains("a&rsquo;b &amp; c"), "got: {out}");
        assert!(!out.contains('’'), "got: {out}");
        // Non-entity-encodable non-ASCII → decimal numeric reference.
        let out2 = san("<p>中</p>");
        assert!(out2.contains("&#20013;"), "got: {out2}");
    }
    #[test]
    fn deep_nesting_does_not_recurse() {
        // 2k nested <div>s — must not blow the call stack. Far deeper than
        // any real HTML; pins the sanitizer's iterative (heap-stack)
        // traversal. Kept well below the point where html5ever's tree
        // builder degrades to ~O(depth²) (20k nesting takes ~17 s to
        // *parse*); our own passes over the tree are linear.
        let deep = format!("{}<div></div>", "<div>".repeat(2_000));
        let out = san(&deep);
        assert!(out.starts_with("<div>"), "got: {out}");
        assert!(out.ends_with("</div>"), "got: {out}");
    }
    #[test]
    fn has_disallowed_ip_checks() {
        assert!(has_disallowed_ip("http://127.0.0.1/x"));
        assert!(has_disallowed_ip("http://localhost/x"));
        assert!(has_disallowed_ip("http://10.0.0.1:8080/x"));
        assert!(!has_disallowed_ip("http://10.0.0.1/x"));
        assert!(!has_disallowed_ip("https://arstechnica.com/x"));
    }

    #[test]
    fn srcset_roundtrip() {
        let decoded = decode_srcset("a.jpg 1x, b.jpg 2x, c.jpg");
        assert_eq!(
            decoded,
            vec![
                ("a.jpg".into(), "1x".into()),
                ("b.jpg".into(), "2x".into()),
                ("c.jpg".into(), "".into())
            ]
        );
    }

    /// `UrlHelper::validate` — the `test_validate_*` cases from
    /// tt-rss/tests_functional/UrlHelperTest.php, ported 1:1.
    #[test]
    fn validate_cases() {
        // Ported from the PHP UrlHelper::validate expectations (no trailing
        // slash is added; scheme/host case is preserved, as in PHP).
        assert_eq!(
            validate("http://example.com"),
            Some("http://example.com".into())
        );
        assert_eq!(
            validate("https://example.com"),
            Some("https://example.com".into())
        );
        // protocol-relative is normalized to https:// (PHP `validate` fixes it).
        assert_eq!(
            validate("//example.com"),
            Some("https://example.com".into())
        );
        // Empty port is dropped on rebuild (PHP parse_url drops it).
        assert_eq!(
            validate("http://domain.invalid:/img.png"),
            Some("http://domain.invalid/img.png".into())
        );
        // Scheme/host case is preserved.
        assert_eq!(
            validate("HTTP://EXAMPLE.COM"),
            Some("HTTP://EXAMPLE.COM".into())
        );
        // Raw (non-encoded) path chars are preserved (filter gate uses the
        // encoded variant only).
        assert_eq!(
            validate("https://example.com/hello world"),
            Some("https://example.com/hello world".into())
        );
        // IDN host -> punycode.
        assert_eq!(
            validate("http://m\u{fc}nchen.example.com/"),
            Some("http://xn--mnchen-3ya.example.com/".into())
        );
        // test_validate_rejects_ftp / _mailto / _javascript / _empty / _no_host / _malformed
        assert_eq!(validate("ftp://ftp.example.com"), None);
        assert_eq!(validate("mailto:test@example.com"), None);
        assert_eq!(validate("javascript:void(0)"), None);
        assert_eq!(validate(""), None);
        assert_eq!(validate("about:blank"), None);
        assert_eq!(validate("not a url"), None);
        // Underscores in the host are rejected by the filter_var gate.
        assert_eq!(validate("http://ex_ample.com/"), None);
        // Non-numeric / out-of-range ports are rejected.
        assert_eq!(validate("http://x:abc/"), None);
        assert_eq!(validate("http://example.com:99999/"), None);
        // Port 0 is accepted (PHP filter_var allows it).
        assert_eq!(
            validate("http://example.com:0/"),
            Some("http://example.com:0/".into())
        );
    }

    #[test]
    fn rewrite_relative_cases() {
        // Ground-truth values from the PHP oracle (base http://domain.invalid/).
        let base = "http://domain.invalid/";
        assert_eq!(
            rewrite_relative(base, "img.png", "img", "src", ""),
            Some("http://domain.invalid/img.png".into())
        );
        assert_eq!(
            rewrite_relative(base, "#section", "a", "href", ""),
            Some("http://domain.invalid#section".into())
        );
        assert_eq!(
            rewrite_relative(base, "?q=1", "a", "href", ""),
            Some("http://domain.invalid?q=1".into())
        );
        assert_eq!(
            rewrite_relative(
                base,
                "data:text/html,<script>alert(1)</script>",
                "img",
                "src",
                ""
            ),
            Some("http://domain.invalid/text/html,alert(1)".into())
        );
        assert_eq!(
            rewrite_relative(base, "javascript :alert(1)", "a", "href", ""),
            Some("http://domain.invalid/javascript :alert(1)".into())
        );
        assert_eq!(
            rewrite_relative(base, "HTTP://EXAMPLE.COM", "a", "href", ""),
            Some("HTTP://EXAMPLE.COM".into())
        );
        assert_eq!(
            rewrite_relative(base, "vbscript:msgbox(1)", "a", "href", ""),
            Some("http://domain.invalid/msgbox(1)".into())
        );
        assert_eq!(
            rewrite_relative(base, "//example.com/page", "a", "href", ""),
            Some("https://example.com/page".into())
        );
        assert_eq!(
            rewrite_relative(base, "", "a", "href", ""),
            Some(base.to_string())
        );
        // mailto: on an img src is NOT an extra scheme (owner a/href only).
        assert_eq!(
            rewrite_relative(base, "mailto:x@y.z", "img", "src", ""),
            Some("http://domain.invalid/x@y.z".into())
        );
        // magnet: on an a href is preserved.
        assert_eq!(
            rewrite_relative(base, "magnet:?xt=1", "a", "href", ""),
            Some("magnet:?xt=1".into())
        );
        // data: image URI on an img src is preserved verbatim.
        assert_eq!(
            rewrite_relative(base, "data:image/png;base64,AAA", "img", "src", ""),
            Some("data:image/png;base64,AAA".into())
        );

        // Base with a port and a non-trailing-slash path: RFC 3986 dirname()
        // of the base path is appended (no dot-segment resolution).
        assert_eq!(
            rewrite_relative(
                "http://domain.invalid:8080/feed/",
                "article.html",
                "a",
                "href",
                ""
            ),
            Some("http://domain.invalid:8080/feed/article.html".into())
        );
        assert_eq!(
            rewrite_relative(
                "http://domain.invalid:8080/feed",
                "article.html",
                "a",
                "href",
                ""
            ),
            Some("http://domain.invalid:8080/article.html".into())
        );
        assert_eq!(
            rewrite_relative(base, "./x.html", "a", "href", ""),
            Some("http://domain.invalid/x.html".into())
        );
        // ../ is NOT resolved (PHP leaves dot-segments as-is).
        assert_eq!(
            rewrite_relative("http://domain.invalid/feed/", "../x.html", "a", "href", ""),
            Some("http://domain.invalid/feed/../x.html".into())
        );
    }
}
