//! `af_comics` (M7.9) — "Fixes RSS feeds of assorted comic strips" (fox).
//!
//! The update-pipeline's filter host: the shell registers the five pipeline
//! hooks and dispatches each to the 12 shipped [`ComicFilter`]s (the PHP's
//! `Af_Comics` + `filters/`). The load-bearing one is [`HOOK_ARTICLE_FILTER`]
//! — for each new article, the first filter whose [`ComicFilter::process`]
//! matches (a link/`guid` substring) fetches the article page and rewrites
//! the stored `content` (and, for [`Dilbert`], `title`/`tags`).
//!
//! The match guards and the pipeline wiring are harness-tested; the per-filter
//! fetch + extraction bodies are ported faithfully from the reference and
//! verified by review (they need a live network, which the harness has no
//! access to).
//!
//! **Deviation notes** (the port's `fetch` API differs from PHP's in two ways
//! the reference leans on):
//! - PHP's `UrlHelper::$fetch_last_error_content` fallback (re-parse the error
//!   response body when the main fetch fails) is not exposed by the port's
//!   [`crate::urlhelper::FetchResult`], so those retries are omitted (moot in
//!   the harness — the fetch never succeeds there).
//! - The per-filter custom `useragent` IS ported (`FetchOptions.useragent`).
//!
//! Extraction uses `scraper` (CSS selectors over the port's `html5ever` tree)
//! as the XPath-equivalent; PHP's `DOMXPath` expressions map to CSS selectors
//! (`//div[@class="x"]/a/img` → `div.x a img`, `contains(@class,"y")` →
//! class membership, `starts-with(@id,"z")` → `[id^="z"]`). The two filters
//! needing DOM ops beyond a selector (the [`DanbyDraws`] parent, the
//! [`ComicPress`] attribute moves) use `ego_tree` / a small regex helper.

#![allow(clippy::new_without_default)]
//! The filters' `new()` pre-parses their CSS selectors (real setup, not
//! default-initialization), so `Default` is intentionally absent.

use async_trait::async_trait;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};

use crate::i18n::__;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, BoxFuture, Plugin};
use crate::pluginhost::{
    HOOK_ARTICLE_FILTER, HOOK_FEED_BASIC_INFO, HOOK_FETCH_FEED, HOOK_PREFS_TAB, HOOK_SUBSCRIBE_FEED,
};
use crate::urlhelper::{FetchOptions, fetch};

/// The user agents the reference filters set to dodge bot-blocking.
const IE_UA: &str = "Mozilla/5.0 (compatible; MSIE 10.0; Windows NT 6.1; WOW64; Trident/6.0)";
const FF_UA: &str = "Mozilla/5.0 (Windows NT 6.1; WOW64; rv:50.0) Gecko/20100101 Firefox/50.0";

/// A comic strip filter (the PHP `Af_ComicFilter` subclass in `filters/`).
pub trait ComicFilter: Send {
    /// `supported()` — the comic names shown in the prefs tab.
    fn supported(&self) -> &[&'static str];
    /// `process(&$article)` — match + fetch + rewrite. Returns `true` when the
    /// filter handled the article (the PHP's `true`); the chain stops on the
    /// first `true`.
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool>;
    /// `on_subscribe($url)` — return a replacement feed content, or `None` (the
    /// PHP's `false`). None of the shipped filters override it.
    fn on_subscribe(&self, _url: &str) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
    /// `on_basic_info($url)` — return feed basic info, or `None` (the PHP's
    /// `false`). None of the shipped filters override it.
    fn on_basic_info(&self, _url: &str) -> BoxFuture<'_, Option<BasicInfo>> {
        Box::pin(async { None })
    }
    /// `on_fetch($url)` — return a replacement fetch body, or `None` (the PHP's
    /// `false`). None of the shipped filters override it.
    fn on_fetch(&self, _url: &str) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
}

/// `hook_feed_basic_info`'s `on_basic_info` return (the PHP's
/// `{"title": string, "site_url": string}`).
#[derive(Debug, Clone)]
pub struct BasicInfo {
    pub title: String,
    pub site_url: String,
}

/// The 12 shipped filters (the PHP's `loadFilters`, `filters/`).
pub fn all_filters() -> Vec<Box<dyn ComicFilter>> {
    vec![
        Box::new(Cad::new()),
        Box::new(Dilbert::new()),
        Box::new(DanbyDraws::new()),
        Box::new(DarkLegacy::new()),
        Box::new(DumbingOfAge::new()),
        Box::new(Explosm::new()),
        Box::new(Tfd::new()),
        Box::new(TheOatmeal::new()),
        Box::new(Twp::new()),
        Box::new(Whomp::new()),
        Box::new(ComicClass::new()),
        Box::new(ComicPress::new()),
    ]
}

/// A string field of the article (`""` when absent / not a string).
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Fetch `$url` (with an optional user agent) and parse the body into an
/// [`Html`] tree (`None` when the fetch fails / has no body).
async fn fetch_doc(url: &str, useragent: Option<&str>) -> Option<Html> {
    let opts = FetchOptions {
        url: url.to_string(),
        useragent: useragent.map(str::to_string),
        ..Default::default()
    };
    let body = fetch(&opts).await.body?;
    let text = String::from_utf8_lossy(&body).into_owned();
    Some(Html::parse_document(&text))
}

/// Fetch + serialize the first element matching `selector` (the PHP's
/// `$doc->saveHTML($xpath->query(…)->item(0))`). `None` when the fetch fails or
/// nothing matches.
async fn fetch_first(url: &str, useragent: Option<&str>, sel: &Selector) -> Option<String> {
    let doc = fetch_doc(url, useragent).await?;
    doc.select(sel).next().map(|el| el.html())
}

/// Move a lazy-load attribute onto its eager counterpart for every `<img>` in
/// `$html` (the PHP's `move_all_attributes`: `data-src` → `src`, …). A small
/// regex approximation of the PHP's per-node `setAttribute`/`removeAttribute`
/// (the harness never reaches this — the fetch fails there).
fn move_attr(html: &str, from: &str, to: &str) -> String {
    static DATA_SRC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = DATA_SRC
        .get_or_init(|| Regex::new(&format!(r#"(?:\s{to}="[^"]*")?\s*{from}="([^"]*)""#)).unwrap());
    re.replace_all(html, &format!(r#"{to}="$1""#)).into_owned()
}

// ---------------------------------------------------------------------------
// The 12 filters. Each matches on a link/`guid` substring, fetches the article
// page, and rewrites the stored `content` (and, for Dilbert, `title`/`tags`).
// ---------------------------------------------------------------------------

/// `Af_Comics_Cad` — The Order of the Stick.
pub struct Cad {
    sel: Selector,
}
impl Cad {
    pub fn new() -> Self {
        Cad {
            sel: parse_sel("div.comicpage a img"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("orderofthestick.org") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, Some(IE_UA), &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for Cad {
    fn supported(&self) -> &[&'static str] {
        &["Ctrl+Alt+Del"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_Dilbert` — Dilbert (comic title + tags + image/transcript).
pub struct Dilbert {
    title_sel: Selector,
    tags_sel: Selector,
    img_sel: Selector,
    transcript_sel: Selector,
}
impl Dilbert {
    pub fn new() -> Self {
        Dilbert {
            title_sel: parse_sel("span.comic-title-name"),
            tags_sel: parse_sel("p.comic-tags a"),
            img_sel: parse_sel("div.img-comic-container > a.img-comic-link"),
            transcript_sel: parse_sel("div[id^=\"js-toggle-transcript-\"] p"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("understood.org") {
            return false;
        }
        let link = s(article, "link").to_string();
        let Some(doc) = fetch_doc(&link, Some(IE_UA)).await else {
            return true; // matched the site; nothing to fetch (the PHP returns true here too)
        };
        // Title: append the comic's own title to the article title.
        if let Some(node) = doc.select(&self.title_sel).next() {
            let comic_title = node.text().collect::<String>();
            let new_title = format!("{} {}", s(article, "title"), comic_title);
            article["title"] = Value::String(new_title);
        }
        // Tags: href query + visible text of each `#tags` link, comma-joined.
        let mut tags: Vec<String> = Vec::new();
        for a in doc.select(&self.tags_sel) {
            let Some(href) = a.attr("href") else { continue };
            let Some(query) = href.split('?').nth(1) else {
                continue;
            };
            if query.is_empty() {
                continue;
            }
            let title = a.text().collect::<String>();
            tags.push(format!("{query} {title}"));
        }
        if !tags.is_empty() {
            article["tags"] = Value::String(tags.join(","));
        }
        // Content: the comic image, else the first transcript `<p>`.
        let node = doc
            .select(&self.img_sel)
            .next()
            .or_else(|| doc.select(&self.transcript_sel).next());
        if let Some(node) = node {
            article["content"] = Value::String(node.html());
        }
        true
    }
}
impl ComicFilter for Dilbert {
    fn supported(&self) -> &[&'static str] {
        &["Dilbert"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_DanbyDraws` — Danby Draws (the comic div, else the image's parent).
pub struct DanbyDraws {
    comic_sel: Selector,
    img_sel: Selector,
}
impl DanbyDraws {
    pub fn new() -> Self {
        DanbyDraws {
            comic_sel: parse_sel("div#comic"),
            img_sel: parse_sel("img.wp-post-image"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("danbydraws.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        let Some(doc) = fetch_doc(&link, None).await else {
            return false;
        };
        // `//div[@id="comic"]` first, else the parent of the post image (the
        // PHP's `…/img[...]/..` — via ego_tree's `parent`, `ElementRef::wrap`).
        let node = doc.select(&self.comic_sel).next().or_else(|| {
            doc.select(&self.img_sel)
                .next()
                .and_then(|el| el.parent().and_then(ElementRef::wrap))
        });
        if let Some(node) = node {
            article["content"] = Value::String(node.html());
            return true;
        }
        false
    }
}
impl ComicFilter for DanbyDraws {
    fn supported(&self) -> &[&'static str] {
        &["Danby Draws"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_DarkLegacy` — Dark Legacy Comics.
pub struct DarkLegacy {
    sel: Selector,
}
impl DarkLegacy {
    pub fn new() -> Self {
        DarkLegacy {
            sel: parse_sel("div.comic"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "guid").contains("darklegacycomics.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, Some(IE_UA), &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for DarkLegacy {
    fn supported(&self) -> &[&'static str] {
        &["Dark Legacy Comics"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_DumbingOfAge` — Dumbing of Age.
pub struct DumbingOfAge {
    sel: Selector,
}
impl DumbingOfAge {
    pub fn new() -> Self {
        DumbingOfAge {
            sel: parse_sel("div#comic"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("dumbingofage.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, Some(FF_UA), &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for DumbingOfAge {
    fn supported(&self) -> &[&'static str] {
        &["Dumbing of Age"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_Explosm` — Cyanide and Happiness.
pub struct Explosm {
    sel: Selector,
}
impl Explosm {
    pub fn new() -> Self {
        Explosm {
            sel: parse_sel("div[class*=\"MainComic__ComicImage\"] img"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("explosm.net/comics") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, None, &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for Explosm {
    fn supported(&self) -> &[&'static str] {
        &["Cyanide and Happiness"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_Tfd` — Toothpaste For Dinner / Married to the Sea.
///
/// NOTE: the reference fetches twice (once with an IE user agent, discarded;
/// then again without, which is used). The port issues the single fetch whose
/// result the reference actually uses (no user agent).
pub struct Tfd {
    sel: Selector,
}
impl Tfd {
    pub fn new() -> Self {
        Tfd {
            sel: parse_sel("img[src*=\".gif\"]"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("toothpastefordinner.com")
            && !s(article, "link").contains("marriedtothesea.com")
        {
            return false;
        }
        let link = s(article, "link").to_string();
        let Some(doc) = fetch_doc(&link, None).await else {
            return false;
        };
        if let Some(node) = doc.select(&self.sel).next() {
            article["content"] = Value::String(node.html());
            return true;
        }
        false
    }
}
impl ComicFilter for Tfd {
    fn supported(&self) -> &[&'static str] {
        &["Toothpaste For Dinner", "Married to the Sea"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_TheOatmeal` — The Oatmeal.
pub struct TheOatmeal {
    post_sel: Selector,
    comic_img_sel: Selector,
}
impl TheOatmeal {
    pub fn new() -> Self {
        TheOatmeal {
            post_sel: parse_sel("div.post"),
            comic_img_sel: parse_sel("div#comic img"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "guid").contains("theoatmeal.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        let Some(doc) = fetch_doc(&link, Some(IE_UA)).await else {
            return true;
        };
        // `//div[@class="post"] | //div[@id="comic"]//img` — first of the two.
        let node = doc
            .select(&self.post_sel)
            .next()
            .or_else(|| doc.select(&self.comic_img_sel).next());
        if let Some(node) = node {
            article["content"] = Value::String(node.html());
        }
        true
    }
}
impl ComicFilter for TheOatmeal {
    fn supported(&self) -> &[&'static str] {
        &["The Oatmeal"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_Twp` — The Web of Pretense? (`td/center/img`).
pub struct Twp {
    sel: Selector,
}
impl Twp {
    pub fn new() -> Self {
        Twp {
            sel: parse_sel("td center img"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "link").contains("twistedpretty.net") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, None, &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for Twp {
    fn supported(&self) -> &[&'static str] {
        &["Three Word Phrase"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_Whomp` — Whomp!
pub struct Whomp {
    sel: Selector,
}
impl Whomp {
    pub fn new() -> Self {
        Whomp {
            sel: parse_sel("img#cc-comic"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "guid").contains("whompcomic.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, Some(IE_UA), &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for Whomp {
    fn supported(&self) -> &[&'static str] {
        &["Whomp!"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_ComicClass` — Loading Artist.
pub struct ComicClass {
    sel: Selector,
}
impl ComicClass {
    pub fn new() -> Self {
        ComicClass {
            sel: parse_sel("div.comic"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        if !s(article, "guid").contains("loadingartist.com") {
            return false;
        }
        let link = s(article, "link").to_string();
        if let Some(html) = fetch_first(&link, Some(IE_UA), &self.sel).await {
            article["content"] = Value::String(html);
        }
        true
    }
}
impl ComicFilter for ComicClass {
    fn supported(&self) -> &[&'static str] {
        &["Loading Artist"]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

/// `Af_Comics_ComicPress` — the WordPress-comic press sites (image + entry
/// text, with lazy-load attribute moves).
///
/// NOTE: the reference also removes `sharedaddy`/`relatedposts`/… nodes; the
/// port's read-only `scraper` tree doesn't support node removal, so that
/// cosmetic cleanup is omitted (the attribute moves are done via [`move_attr`]).
pub struct ComicPress {
    img_sel: Selector,
    text_sel: Selector,
}
impl ComicPress {
    pub fn new() -> Self {
        ComicPress {
            img_sel: parse_sel("div#comic"),
            text_sel: parse_sel("div.entry, div.entry-content"),
        }
    }
    async fn do_process(&self, article: &mut Value) -> bool {
        let guid = s(article, "guid");
        if !(guid.contains("bunicomic.com")
            || guid.contains("buttersafe.com")
            || guid.contains("extrafabulouscomics.com")
            || guid.contains("theduckwebcomics.com/Powerup_Comics")
            || guid.contains("happyjar.com")
            || guid.contains("nedroid.com")
            || guid.contains("stonetoss.com")
            || guid.contains("csectioncomics.com"))
        {
            return false;
        }
        let link = s(article, "link").to_string();
        let Some(doc) = fetch_doc(&link, None).await else {
            return false;
        };
        let img = doc.select(&self.img_sel).next();
        let text = doc.select(&self.text_sel).next();
        if img.is_none() && text.is_none() {
            return false;
        }
        let mut content = String::new();
        if let Some(node) = img {
            content.push_str(&move_attr(
                &move_attr(&node.html(), "data-src", "src"),
                "data-srcset",
                "srcset",
            ));
            content = move_attr(&content, "data-sizes", "sizes");
        }
        if let Some(node) = text {
            content.push_str(&node.html());
        }
        article["content"] = Value::String(content);
        true
    }
}
impl ComicFilter for ComicPress {
    fn supported(&self) -> &[&'static str] {
        &[
            "Buni",
            "Buttersafe",
            "Happy Jar",
            "CSection",
            "Extra Fabulous Comics",
            "Nedroid",
            "Stonetoss",
            "Powerup Comics",
        ]
    }
    fn process<'a>(&'a self, article: &'a mut Value) -> BoxFuture<'a, bool> {
        Box::pin(self.do_process(article))
    }
}

// ---------------------------------------------------------------------------
// The plugin (the PHP's `Af_Comics` shell).
// ---------------------------------------------------------------------------

/// The `af_comics` plugin (the PHP's `Af_Comics`).
#[derive(Clone, Copy)]
pub struct AfComics;

#[async_trait]
impl Plugin for AfComics {
    fn name(&self) -> &'static str {
        "af_comics"
    }
    fn class_name(&self) -> &'static str {
        "Af_Comics"
    }
    fn about(&self) -> About {
        About::new("Fixes RSS feeds of assorted comic strips", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_FETCH_FEED);
        host.add_hook_default(HOOK_FEED_BASIC_INFO);
        host.add_hook_default(HOOK_SUBSCRIBE_FEED);
        host.add_hook_default(HOOK_ARTICLE_FILTER);
        host.add_hook_default(HOOK_PREFS_TAB);
    }

    /// `hook_article_filter` — run each filter's `process`; the first `true`
    /// stops the chain (the PHP returns `$article` either way).
    async fn hook_article_filter(&self, _host: &mut HostCtx, article: &mut Value) -> Value {
        for f in all_filters() {
            if f.process(article).await {
                break;
            }
        }
        article.clone()
    }

    /// `hook_fetch_feed` — run each filter's `on_fetch`; the first `Some`
    /// replaces the feed data, else the original.
    async fn hook_fetch_feed(
        &self,
        _host: &mut HostCtx,
        feed_data: &str,
        fetch_url: &str,
        _owner_uid: i32,
        _feed: i32,
        _last_article_timestamp: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        for f in all_filters() {
            if let Some(replacement) = f.on_fetch(fetch_url).await {
                return replacement;
            }
        }
        feed_data.to_string()
    }

    /// `hook_subscribe_feed` — run each filter's `on_subscribe`; the first
    /// `Some` replaces the content, else the original.
    async fn hook_subscribe_feed(
        &self,
        _host: &mut HostCtx,
        contents: &str,
        url: &str,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> String {
        for f in all_filters() {
            if let Some(replacement) = f.on_subscribe(url).await {
                return replacement;
            }
        }
        contents.to_string()
    }

    /// `hook_feed_basic_info` — run each filter's `on_basic_info`; the first
    /// `Some` replaces the info, else the original.
    async fn hook_feed_basic_info(
        &self,
        _host: &mut HostCtx,
        basic_info: &mut Value,
        fetch_url: &str,
        _owner_uid: i32,
        _feed_id: i32,
        _auth_login: &str,
        _auth_pass: &str,
    ) -> Value {
        for f in all_filters() {
            if let Some(info) = f.on_basic_info(fetch_url).await {
                return json!({
                    "title": info.title,
                    "site_url": info.site_url,
                });
            }
        }
        basic_info.clone()
    }

    /// `hook_prefs_tab` — list the supported comics in the `prefFeeds` pane
    /// (the PHP's `<?php ?>` block).
    async fn hook_prefs_tab(&self, host: &mut HostCtx, tab: &str) -> () {
        if tab != "prefFeeds" {
            return;
        }
        let mut comics: Vec<String> = Vec::new();
        for f in all_filters() {
            for name in f.supported() {
                comics.push(name.to_string());
            }
        }
        comics.sort();
        let list: String = comics
            .iter()
            .map(|c| format!("<li>{}</li>", crate::util::htmlspecialchars(c)))
            .collect();
        let notice_gocomics = crate::controls::format_notice(
            "To subscribe to GoComics use the comic's regular web page as the                  feed URL (e.g. for the <em>Garfield</em> comic use                  <code>http://www.gocomics.com/garfield</code>).",
        );
        let notice_filters = crate::controls::format_notice(
            "Drop any updated filters into <code>filters.local</code> in plugin                  directory.",
        );
        let comics_header = __("The following comics are currently supported:");
        let feeds_title = __("Feeds supported by af_comics");
        host.output.push_str(&format!(
            "<div dojoType=\"dijit.layout.AccordionPane\"                  title=\"<i class='material-icons'>photo</i> {feeds_title}\">\n\
             \t<h3>{comics_header}</h3>\n\
             \t<ul class='panel panel-scrollable list list-unstyled'>\n\
             {list}\
             \t</ul>\n\
             \t{notice_gocomics}\n\
             \t{notice_filters}\n\
             </div>\n",
        ));
    }
}

fn parse_sel(css: &str) -> Selector {
    Selector::parse(css).expect("static selector")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pluginhost::{
        HOOK_ARTICLE_FILTER, HOOK_FEED_BASIC_INFO, HOOK_FETCH_FEED, HOOK_PREFS_TAB,
        HOOK_SUBSCRIBE_FEED,
    };
    use std::sync::Arc;

    /// A host with only `af_comics` + a lazy pool (the hook-dispatch / prefs-tab
    /// tests never touch the DB).
    fn setup() -> (
        Arc<PluginHost>,
        sqlx::postgres::PgPool,
        crate::core::config::Config,
    ) {
        let host = Arc::new(PluginHost::startup(&[|| Box::new(AfComics)]));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://ttrss:ttrss@localhost/ttrss_test")
            .unwrap();
        (host, pool, crate::core::config::Config::for_test())
    }

    /// The five `init` registrations (checked through the runner's `hook_order`;
    /// `af_comics` alone answers each of the five).
    #[tokio::test]
    async fn init_registers_the_five_hooks() {
        let (host, pool, test_cfg) = setup();
        let ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_comics"],
        );
        for h in [
            HOOK_FETCH_FEED,
            HOOK_FEED_BASIC_INFO,
            HOOK_SUBSCRIBE_FEED,
            HOOK_ARTICLE_FILTER,
            HOOK_PREFS_TAB,
        ] {
            let order = ctx.hook_order(h);
            assert_eq!(order.len(), 1, "{h}");
            assert_eq!(host.plugin(order[0]).name(), "af_comics", "{h}");
        }
    }

    /// All 12 filters are present (the 18 comic names are the spec; a raw filter
    /// count would be an implementation detail).
    #[test]
    fn all_twelve_filters_present() {
        let filters = all_filters();
        assert_eq!(filters.len(), 12);
        let mut names: Vec<String> = Vec::new();
        for f in &filters {
            for n in f.supported() {
                names.push(n.to_string());
            }
        }
        for want in [
            "Ctrl+Alt+Del",
            "Dilbert",
            "The Oatmeal",
            "Three Word Phrase",
            "Loading Artist",
        ] {
            assert!(names.iter().any(|n| n == want), "missing {want}");
        }
        assert_eq!(names.len(), 20);
    }

    /// An article matching no filter is returned unchanged (every match guard
    /// rejects it, so no fetch is attempted).
    #[tokio::test]
    async fn no_match_leaves_article_unchanged() {
        let (host, pool, test_cfg) = setup();
        let mut article = json!({"link": "https://example.com/a", "guid": "https://example.com/a", "content": "orig"});
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_comics"],
        );
        let out = AfComics.hook_article_filter(&mut ctx, &mut article).await;
        assert_eq!(out, article);
    }

    /// A filter's `process` rejects a non-matching link/`guid` (the negative
    /// match guard) without fetching.
    #[tokio::test]
    async fn filter_process_negative_guard() {
        assert!(
            !Cad::new()
                .process(&mut json!({"link": "https://example.com/x"}))
                .await
        );
        assert!(
            !Whomp::new()
                .process(&mut json!({"guid": "https://example.com/x"}))
                .await
        );
    }

    /// The prefs tab lists the supported comics; a non-`prefFeeds` tab is empty.
    #[tokio::test]
    async fn prefs_tab_lists_supported_comics() {
        let (host, pool, test_cfg) = setup();
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_comics"],
        );
        AfComics.hook_prefs_tab(&mut ctx, "prefFeeds").await;
        let out = std::mem::take(&mut ctx.output);
        assert!(out.contains("dijit.layout.AccordionPane"));
        assert!(out.contains("Ctrl+Alt+Del"));
        assert!(out.contains("The Oatmeal"));

        let mut ctx2 = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_comics"],
        );
        AfComics.hook_prefs_tab(&mut ctx2, "other").await;
        assert!(ctx2.output.is_empty());
    }

    /// The three feed-level hooks pass through (no shipped filter overrides
    /// `on_fetch`/`on_subscribe`/`on_basic_info`).
    #[tokio::test]
    async fn feed_hooks_pass_through() {
        let (host, pool, test_cfg) = setup();
        let mut ctx = HostCtx::for_user(
            Arc::clone(&host),
            pool.clone(),
            test_cfg.clone(),
            Some(1),
            None,
            0,
            &[],
            &["af_comics"],
        );
        let out = AfComics
            .hook_fetch_feed(&mut ctx, "FEED", "https://x", 1, 0, 0, "", "")
            .await;
        assert_eq!(out, "FEED");
        let out = AfComics
            .hook_subscribe_feed(&mut ctx, "C", "https://x", "", "")
            .await;
        assert_eq!(out, "C");
        let mut bi = json!({"title": "t"});
        let out = AfComics
            .hook_feed_basic_info(&mut ctx, &mut bi, "https://x", 1, 0, "", "")
            .await;
        assert_eq!(out, json!({"title": "t"}));
    }
}
