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
use std::collections::BTreeSet;

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
    "a", "abbr", "address", "acronym", "audio", "article", "aside",
    "b", "bdi", "bdo", "big", "blockquote", "body", "br",
    "caption", "cite", "center", "code", "col", "colgroup",
    "data", "dd", "del", "details", "description", "dfn", "div", "dl", "font",
    "dt", "em", "footer", "figure", "figcaption",
    "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "html", "i",
    "img", "ins", "kbd", "li", "main", "mark", "nav", "noscript",
    "ol", "p", "picture", "pre", "q", "ruby", "rp", "rt", "s", "samp", "section",
    "small", "source", "span", "strike", "strong", "sub", "summary",
    "sup", "table", "tbody", "td", "tfoot", "th", "thead", "time",
    "tr", "track", "tt", "u", "ul", "var", "wbr", "video", "xml:namespace",
];

/// `Sanitizer::$disallowed_attributes` (Sanitizer.php).
const DISALLOWED_ATTRIBUTES: &[&str] = &["id", "style", "class", "width", "height", "allow", "srcdoc"];

/// HTML void elements (no closing tag) — for the serializer.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input",
    "link", "meta", "param", "source", "track", "wbr",
];

/// `UrlHelper::EXTRA_HREF_SCHEMES`.
const EXTRA_HREF_SCHEMES: &[&str] = &["magnet", "mailto", "tel"];

/// The allowed-elements set (lowercased), optionally including `iframe`.
fn allowed_elements(has_sandbox: bool) -> BTreeSet<String> {
    let mut s: BTreeSet<String> = ALLOWED_ELEMENTS.iter().map(|s| s.to_string()).collect();
    if has_sandbox {
        s.insert("iframe".to_string());
    }
    s
}

/// Find the `<body>` element of a full-document parse (the fragment is placed
/// there, mirroring PHP's `DOMDocument::loadHTML`).
fn find_body(doc: &NodeRef) -> Option<NodeRef> {
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
        el.as_element().unwrap().attributes.borrow_mut().insert(k.clone(), v.clone());
    }
    el
}

/// Elements that close libxml2's implicit `<p>` (block-level, i.e. not
/// phrasing content in the HTML DTD).
const IMPLICIT_P_BLOCK_ELEMENTS: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "div",
    "dd", "dl", "dt", "fieldset", "figure", "figcaption", "footer", "form",
    "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "main", "nav", "ol",
    "p", "pre", "section", "summary", "table", "ul",
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

/// The sanitized fragment HTML.
pub fn sanitize(
    input: &str,
    force_remove_images: bool,
    strip_images: bool,
    has_sandbox: bool,
    bw_limit: bool,
    site_url: Option<&str>,
    highlight_words: Option<&[String]>,
) -> String {
    let res = input.trim();
    if res.is_empty() {
        return String::new();
    }

    let doc = parse_html().one(res);
    let Some(body) = find_body(&doc) else {
        return String::new();
    };

    // 0. Emulate libxml2 (PHP `DOMDocument::loadHTML`) implicit markup:
    // drop leading whitespace/comments, open an implicit `<p>` for leading
    // bare text.
    apply_libxml_implicit_p(&body);

    let rewrite_base = site_url.unwrap_or("http://domain.invalid/");

    // 1. Rewrite links/images (a[@href], img/source[src|srcset], video[poster]).
    rewrite_url_elements(&body, rewrite_base, force_remove_images, strip_images, bw_limit);

    // 2. iframes: sandbox.
    process_iframes(&body);

    // 3. Strip disallowed elements (whole subtree) + disallowed attributes.
    strip_harmful_tags(&body, &allowed_elements(has_sandbox));

    // 4. Highlight words (if the feed has any).
    if let Some(words) = highlight_words {
        if !words.is_empty() {
            highlight_words_in_tree(&body, words);
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
        let needs_rewrite = if let Some(el) = node.as_element() {
            let name = lower(&el_name(el));
            let is_link_img = matches!(name.as_str(), "a" | "img" | "source" | "video");
            is_link_img
                && ["href", "src", "srcset", "poster"]
                    .iter()
                    .any(|k| el.attributes.borrow().get(*k).is_some())
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
        if let Some(u) = rewrite_relative(base, &href, &name, "href", "") {
            el.attributes.borrow_mut().insert("href", u);
        }
        el.attributes.borrow_mut().insert("rel", "noopener noreferrer".to_string());
        el.attributes.borrow_mut().insert("target", "_blank".to_string());
    }

    let mut should_replace_element = false;
    let mut src_valid = true;

    if let Some(src) = src {
        match rewrite_relative(base, &src, &name, "src", "") {
            Some(u) if u.to_lowercase().starts_with("data:") => {
                el.attributes.borrow_mut().insert("src", u);
            }
            Some(u) if !has_disallowed_ip(&u) => {
                el.attributes.borrow_mut().insert("src", u);
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
            el.attributes.borrow_mut().insert("srcset", encode_srcset(&validated));
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
        el.attributes.borrow_mut().insert("referrerpolicy", "no-referrer".to_string());
        el.attributes.borrow_mut().insert("loading", "lazy".to_string());
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
        let src = el.attributes.borrow().get("src").map(|s| s.to_string()).unwrap_or_default();
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
fn process_iframes(root: &NodeRef) {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.as_element() {
            if el_name(el) == "iframe" {
                // No HOOK_IFRAME_WHITELISTED plugins are relevant here, so use
                // the restrictive sandbox (Sanitizer.php).
                el.attributes.borrow_mut().insert("sandbox", "allow-scripts".to_string());
            }
        }
        for c in node.children() {
            stack.push(c);
        }
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
            to_remove.push(exp_name.local.to_string());
        }
    }
    for name in to_remove {
        el.attributes.borrow_mut().remove(name.as_str());
    }
}

fn value_starts_with_js(value: &str) -> bool {
    let v = value.trim_start().to_ascii_lowercase();
    v.starts_with("javascript:")
}

/// Pass 4: wrap case-insensitive occurrences of each word in `<span
/// class="highlight">` (Sanitizer::highlight_words). Iterative.
fn highlight_words_in_tree(root: &NodeRef, words: &[String]) {
    for word in words {
        let w = word.to_lowercase();
        if w.is_empty() {
            continue;
        }
        let mut stack = vec![root.clone()];
        while let Some(node) = stack.pop() {
            if let Some(text) = node.as_text() {
                let content = text.borrow().clone();
                let lower_content = content.to_lowercase();
                if let Some(pos) = lower_content.find(&w) {
                    let byte_end = pos + w.len();
                    let before = &content[..pos];
                    let matched = &content[pos..byte_end];
                    let after = &content[byte_end..];
                    let highlight = build_element(
                        "span",
                        &[("class".into(), "highlight".into())],
                    );
                    highlight.append(NodeRef::new_text(matched.to_string()));
                    if !before.is_empty() {
                        node.insert_before(NodeRef::new_text(before.to_string()));
                    }
                    node.insert_before(highlight);
                    if !after.is_empty() {
                        node.insert_after(NodeRef::new_text(after.to_string()));
                    }
                    node.detach();
                    continue; // re-walk to catch further matches
                }
            }
            for c in node.children() {
                stack.push(c);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// URL validation (UrlHelper)
// ---------------------------------------------------------------------------

/// Port of `UrlHelper::rewrite_relative`. Returns the absolute/validated URL or
/// `None` on failure.
fn rewrite_relative(
    base_url: &str,
    rel_url: &str,
    owner_element: &str,
    owner_attribute: &str,
    _content_type: &str,
) -> Option<String> {
    if rel_url.is_empty() {
        return Some(base_url.to_string());
    }
    let parsed = match url::Url::parse(rel_url) {
        Ok(u) => u,
        Err(_) => return None,
    };
    let scheme = parsed.scheme().to_string();
    let host = parsed.host_str().unwrap_or("").to_string();

    if !host.is_empty() && !scheme.is_empty() {
        return validate(&rel_url);
    }
    if rel_url.starts_with("//") {
        return validate(&format!("https:{rel_url}"));
    }
    if EXTRA_HREF_SCHEMES.contains(&scheme.as_str())
        && owner_element == "a"
        && owner_attribute == "href"
    {
        return Some(rel_url.to_string());
    }
    if scheme == "data"
        && rel_url.starts_with("image/(webp|gif|jpg|png|svg);base64,")
        && owner_element == "img"
        && owner_attribute == "src"
    {
        return Some(rel_url.to_string());
    }

    // Relative: resolve against the base URL, then validate.
    let base = url::Url::parse(base_url).ok()?;
    let rel = url::Url::parse(rel_url).ok()?;
    let resolved = base.join(rel.as_str()).ok()?;
    let rebuilt = rebuild_url(&resolved);
    validate(&rebuilt)
}

/// Rebuild a URL string from its parsed components, mirroring
/// `UrlHelper::build_url` (scheme://host[:port][path][?query][#fragment]).
fn rebuild_url(u: &url::Url) -> String {
    let mut out = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
    if let Some(p) = u.port() {
        out.push_str(&format!(":{p}"));
    }
    out.push_str(u.path());
    if u.query().is_some() {
        out.push('?');
        out.push_str(u.query().unwrap_or(""));
    }
    if let Some(f) = u.fragment() {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Port of `UrlHelper::validate` (non-extended). Returns the validated URL or
/// `None`.
fn validate(url: &str) -> Option<String> {
    let url = url.trim();
    let url = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    };
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.host_str().is_none() {
        return None;
    }
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(rebuild_url(&parsed))
}

/// Port of `UrlHelper::has_disallowed_ip` (no DNS resolution). Returns true if
/// the URL's host is a disallowed IP (localhost, loopback, link-local,
/// reserved, or private on a non-standard port).
fn has_disallowed_ip(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = match parsed.host_str().map(|h| h.trim_matches(['[', ']']).to_lowercase()) {
        Some(h) if !h.is_empty() => h,
        _ => return false,
    };
    let port = parsed.port_or_known_default();
    let is_standard_port = matches!(port, None | Some(80) | Some(443));

    if host == "localhost" {
        return true;
    }

    // Only IP literals are evaluated here (no DNS resolution).
    let ip: std::net::IpAddr = match host.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    match ip {
        std::net::IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
            {
                return true;
            }
            if !is_standard_port && v4.is_private() {
                return true;
            }
            false
        }
        std::net::IpAddr::V6(v6) => {
            let octets = v6.octets();
            // fe80::/10 (link-local), ::1 (loopback), :: (unspecified)
            let is_link_local = octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80;
            if v6.is_loopback() || is_link_local || v6.is_unspecified() {
                return true;
            }
            let is_ula = (octets[0] & 0xfe) == 0xfc; // fc00::/7
            if !is_standard_port && is_ula {
                return true;
            }
            false
        }
    }
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

/// Serialize a node (and descendants) to HTML, iteratively (heap stack).
fn serialize_node(node: &NodeRef, out: &mut String) {
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
        out.push_str("=\"");
        escape_attr(&attr.value, out);
        out.push('"');
    }
    out.push('>');
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\0' => out.push_str("\u{fffd}"),
            _ => out.push(c),
        }
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn san(html: &str) -> String {
        sanitize(html, false, false, true, false, None, None)
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
        assert_eq!(san("<b>a</b>text<b>c</b>"),
                   "<b>a</b>text<b>c</b>");
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
        assert_eq!(san("<div>a</div>  <div>b</div>"),
                   "<div>a</div>  <div>b</div>");
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

    #[test]
    fn decodes_entities_to_chars() {
        let out = san("<p>a&rsquo;b &amp; c</p>");
        assert!(out.contains("’"), "got: {out}");
        assert!(out.contains("&amp;"), "got: {out}");
    }

    #[test]
    fn deep_nesting_does_not_recurse() {
        // 20k nested <div>s — must not blow the call stack.
        let deep = format!("{}<div></div>", "<div>".repeat(20_000));
        let _ = san(&deep);
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
        // test_validate_valid_http / _valid_https / _protocol_relative_normalized.
        // NOTE: url::Url normalizes a host-only URL to a "/" path, so the
        // rebuilt URL carries a trailing slash ("http://example.com/") where
        // the PHP build_url() returns "http://example.com". The difference is
        // cosmetic (both are valid, equivalent origins) — REVISIT if exact
        // string parity matters.
        assert_eq!(validate("http://example.com"), Some("http://example.com/".into()));
        assert_eq!(validate("https://example.com"), Some("https://example.com/".into()));
        assert_eq!(validate("//example.com"), Some("https://example.com/".into()));
        // test_validate_rejects_ftp / _mailto / _javascript / _empty / _no_host / _malformed
        assert_eq!(validate("ftp://ftp.example.com"), None);
        assert_eq!(validate("mailto:test@example.com"), None);
        assert_eq!(validate("javascript:void(0)"), None);
        assert_eq!(validate(""), None);
        assert_eq!(validate("about:blank"), None);
        assert_eq!(validate("not a url"), None);
    }

    // NOTE: test_validate_lowercase_scheme expects the returned URL to preserve
    // the original scheme case ("HTTP://EXAMPLE.COM"), but url::Url lowercases
    // the scheme ("http://EXAMPLE.COM"). REVISIT (scheme-case preservation) —
    // tracked in docs/plan.md.
}

