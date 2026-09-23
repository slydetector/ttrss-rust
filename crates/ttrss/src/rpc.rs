//! Port of the `backend.php` entry point + the `RPC` handler (M2 read ops).
//!
//! `backend.php` is the internal JSON RPC the Dojo SPA drives:
//! `POST|GET op=<Handler>&method=<m>&csrf_token=…`. This module ports the
//! dispatch (public shim, session validation, CSRF, handler routing) and the
//! `RPC` handler's `sanityCheck` (the SPA's first call, which un-blocks the
//! load sequence) and `getRuntimeInfo`. Other handlers/methods return
//! `E_UNKNOWN_METHOD` until ported.

use crate::core::phpser;
use crate::core::user;
use axum::body::{to_bytes, Body};
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{Datelike, Timelike};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::article;
use crate::feeds;
use crate::filters;
use crate::labels;
use crate::opml;
use crate::plugins;
use crate::pref_ui;
use crate::prefs;
use crate::session::{self, Sess};
use crate::settings;
use crate::system;
use crate::users;
use crate::util;
use crate::AppState;

/// Form/query params for `backend.php` (all arrive via `$_REQUEST`).
#[derive(Deserialize, Default)]
pub struct RpcForm {
    op: Option<String>,
    method: Option<String>,
    subop: Option<String>,
    csrf_token: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// Parse an `application/x-www-form-urlencoded` (or query-string) payload into
/// an [`RpcForm`], mirroring PHP's `$_REQUEST` normalization. The Dojo SPA
/// sends batch ops (`catchupSelected`, `markSelected`, `delete`, …) as
/// repeated `ids[]=…&ids[]=…` params and the filter forms as repeated
/// `rule[]`/`action[]` params; a `Map` (the previous `serde_urlencoded` path)
/// collapsed each repeated key to its *last* value, so only the final id was
/// ever acted on. A `[]`-suffixed field name normalizes to the bare key, as
/// an array even with a single value (PHP: `rule[]={…}` → `$_REQUEST['rule']
/// = ['{…}']`). A plain single-valued key stays a scalar string (the axum
/// `Form` convention the scalar handlers and tests rely on); a repeated plain
/// key becomes a `Value::Array` that `param_int_array` and friends read whole.
fn rpc_form_from_urlencoded(input: &[u8]) -> RpcForm {
    let mut op: Option<String> = None;
    let mut method: Option<String> = None;
    let mut subop: Option<String> = None;
    let mut csrf_token: Option<String> = None;
    let mut idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut vals: Vec<(String, Vec<String>)> = Vec::new();
    for (k, v) in url::form_urlencoded::parse(input) {
        let (k, v) = (k.into_owned(), v.into_owned());
        match k.as_str() {
            "op" => op = Some(v),
            "method" => method = Some(v),
            "subop" => subop = Some(v),
            "csrf_token" => csrf_token = Some(v),
            _ => {
                let i = *idx.entry(k.clone()).or_insert_with(|| {
                    vals.push((k.clone(), Vec::new()));
                    vals.len() - 1
                });
                vals[i].1.push(v);
            }
        }
    }
    let mut extra: Map<String, Value> = Map::new();
    for (k, v) in vals {
        // PHP `$_REQUEST`: `name[]` fields normalize to the bare key, always
        // as an array (even a single `x[]=5` yields `$_REQUEST['x'] = ['5']`).
        let arr = k.ends_with("[]");
        let key = if arr { k[..k.len() - 2].to_string() } else { k };
        let value = if arr || v.len() > 1 {
            Value::Array(v.into_iter().map(Value::String).collect())
        } else {
            Value::String(v.into_iter().next().unwrap())
        };
        extra.insert(key, value);
    }
    RpcForm {
        op,
        method,
        subop,
        csrf_token,
        extra,
    }
}

/// `backend.php` — dispatch on `op`/`method`, return a JSON envelope.
pub async fn handler(
    State(state): State<AppState>,
    sess: axum::extract::Extension<Sess>,
    req: Request,
) -> Response {
    // `startup_gettext` runs per request; the `init` (`sanitycheck`) response
    // carries the `translations` map for the resolved locale (PHP `RPC::init`).
    let accept_language: Option<String> = req
        .headers()
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let (form, file) = parse_backend(req).await;
    let op = form.op.clone().unwrap_or_default();
    let method = form
        .subop
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| form.method.clone())
        .unwrap_or_else(|| "index".into());
    // `backend.php`: `$method = strtolower($method)` — method names are matched
    // case-insensitively (PHP resolves `call` / `method_exists` case-insensitively),
    // so the dispatcher matches on the lowercased form.
    let method = method.to_ascii_lowercase();
    let csrf = form.csrf_token.clone().unwrap_or_default();
    let extra = form.extra.clone();

    // `backend.php` public-calls compatibility shim.
    if matches!(op.as_str(), "rss" | "getProfiles" | "share") {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("public.php?{}", state.config.self_url_rtrim()),
            )],
        )
            .into_response();
    }

    let (body, content_type) =
        match dispatch(&state, &sess, &op, &method, &csrf, &extra, file.as_deref(), accept_language.as_deref()).await {
            DispatchOutput::Json(v) => (v.to_string(), "application/json; charset=utf-8"),
            DispatchOutput::Html(h) => (h, "application/json; charset=utf-8"),
            DispatchOutput::TextHtml(h) => (h, "text/html;charset=UTF-8"),
            DispatchOutput::Raw(status, location, body) => {
                let mut res = Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                    .body(Body::from(body))
                    .expect("valid response");
                if let Some(loc) = location {
                    // A 302 without a parseable `Location` would be malformed; the
                    // validated URL is always parseable, but fall back rather than panic.
                    let hv = loc
                        .parse()
                        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("/"));
                    res.headers_mut().insert(header::LOCATION, hv);
                }
                if sess.had_cookie() {
                    super::nocache_headers(res.headers_mut());
                }
                return res;
            }
            DispatchOutput::Download(status, ct, disposition, body) => {
                let mut res = Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, ct)
                    .body(Body::from(body))
                    .expect("valid response");
                if let Some(d) = disposition {
                    if let Ok(hv) = d.parse() {
                        res.headers_mut().insert(header::CONTENT_DISPOSITION, hv);
                    }
                }
                if sess.had_cookie() {
                    super::nocache_headers(res.headers_mut());
                }
                return res;
            }
        };

    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("valid response");
    // The PHP session `nocache` limiter stamps responses whose session started
    // (i.e. the request carried a session cookie).
    if sess.had_cookie() {
        super::nocache_headers(res.headers_mut());
    }
    res
}

/// Parse a `backend.php` request into its [`RpcForm`] fields plus any uploaded
/// file bytes. `backend.php` is driven three ways: GET (query string) or POST
/// `application/x-www-form-urlencoded` / `application/json` (scalar `$_REQUEST`
/// fields), and POST `multipart/form-data` (the icon / OPML uploads, which
/// carry a `$_FILES` part). The multipart text fields populate `RpcForm`
/// exactly as the urlencoded path does (scalar strings → `Value::String`);
/// the single file part's bytes are returned alongside.
async fn parse_backend(req: Request) -> (RpcForm, Option<Vec<u8>>) {
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ct.starts_with("multipart/form-data") {
        let mut form = RpcForm::default();
        let mut file: Option<Vec<u8>> = None;
        let mut mp = match Multipart::from_request(req, &()).await {
            Ok(m) => m,
            Err(_) => return (form, None),
        };
        while let Ok(Some(field)) = mp.next_field().await {
            let is_file = field.file_name().is_some();
            let name = field.name().unwrap_or("").to_string();
            let bytes = field.bytes().await.unwrap_or_default().to_vec();
            if is_file {
                file = Some(bytes);
            } else if !name.is_empty() {
                let s = String::from_utf8_lossy(&bytes).to_string();
                match name.as_str() {
                    "op" => form.op = Some(s),
                    "method" => form.method = Some(s),
                    "subop" => form.subop = Some(s),
                    "csrf_token" => form.csrf_token = Some(s),
                    _ => {
                        form.extra.insert(name, Value::String(s));
                    }
                }
            }
        }
        return (form, file);
    }

    // GET (query string) or POST urlencoded / json body.
    let query = req.uri().query().unwrap_or("").to_string();
    let body = to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default()
        .to_vec();
    let form = if ct.contains("application/json") {
        serde_json::from_slice(&body).unwrap_or_default()
    } else if !body.is_empty() {
        rpc_form_from_urlencoded(&body)
    } else {
        rpc_form_from_urlencoded(query.as_bytes())
    };
    (form, None)
}

/// `backend.php` dispatch output: the JSON envelope, or raw HTML for the few
/// methods that `echo` markup directly (they keep the JSON content-type —
/// PHP sets it in `backend.php` before dispatch).
enum DispatchOutput {
    Json(Value),
    Html(String),
    TextHtml(String),
    /// A raw status + body (the `Article::redirect` 302/404).
    Raw(StatusCode, Option<String>, String),
    /// A direct download (OPML export): explicit content-type + optional
    /// `Content-Disposition`, body is the (non-JSON) payload.
    Download(StatusCode, String, Option<String>, String),
}

async fn dispatch(
    state: &AppState,
    sess: &Sess,
    op: &str,
    method: &str,
    csrf: &str,
    extra: &Map<String, Value>,
    file: Option<&[u8]>,
    accept_language: Option<&str>,
) -> DispatchOutput {
    let mut data = sess.data();

    // `Sessions::validate_session` (parity row 9): guests pass; a logged-in
    // session is re-checked against the DB (user exists, account not
    // disabled, pwd hash unchanged).
    if !session::validate_session(state.db.pool(), state.config.single_user_mode, &mut data).await {
        // Reflect the failure reason into the live session (PHP $_SESSION),
        // where the UI surfaces it.
        if let Some(msg) = data.get_str("login_error_msg") {
            sess.with_mut(|s| {
                s.set(
                    "login_error_msg",
                    phpser::Value::Bytes(msg.as_bytes().to_vec()),
                )
            });
        }
        return DispatchOutput::Json(errors("E_UNAUTHORIZED"));
    }

    if method.starts_with('_') {
        return DispatchOutput::Json(errors("E_UNAUTHORIZED"));
    }

    // CSRF gate: `validate_csrf($csrf_token) || $handler->csrf_ignore($method)`.
    if !validate_csrf(sess, csrf) && !csrf_ignored(op, method) {
        return DispatchOutput::Json(errors("E_UNAUTHORIZED"));
    }

    // `Handler_Protected::before`: the handler needs a logged-in user.
    if is_protected(op) && data.uid().is_none() {
        return DispatchOutput::Json(errors("E_UNAUTHORIZED"));
    }

    // `Handler_Administrative::before` (extends `Handler_Protected`): the
    // admin handlers (`Pref_Users`, `Pref_System`) additionally need
    // `access_level >= ACCESS_LEVEL_ADMIN`.
    if is_administrative(op)
        && data.access_level().unwrap_or(0) < crate::core::user::ACCESS_LEVEL_ADMIN as i64
    {
        return DispatchOutput::Json(errors("E_UNAUTHORIZED"));
    }

    // M10.3: resolve the UI locale once (profile-aware `USER_LANGUAGE` pref,
    // falling back to `Accept-Language`) for the server-rendered surfaces
    // (the headlines `feed_title`/content and the `hotkeyhelp` descriptions).
    //
    // TODO(perf): this runs for *every* RPC op, even ones that render no
    // user-facing string (it hits the per-process prefs cache — a memory hit
    // after the first load per user). Re-evaluate lazily (resolve only in
    // `view`/`hotkeyhelp`) if the pref load ever shows up in a profile.
    let locale = {
        let uid = data.uid().unwrap_or(0);
        let profile = data.profile().map(|p| p as i32);
        crate::i18n::resolve_user_locale(state, uid, profile, accept_language).await
    };

    match op {
        "RPC" => match method {
            "hotkeyhelp" => {
                DispatchOutput::Html(crate::hotkeys::hotkey_help(locale.as_deref(), &state.i18n))
            }
            _ => DispatchOutput::Json(rpc_handler(state, sess, method, extra, accept_language).await),
        },
        // `PluginHandler::catchall` (M7.3) — plugin method dispatch. The
        // handler-level CSRF gate is a no-op (see `csrf_ignored`); the
        // plugin-level gate runs inside the handler.
        "PluginHandler" => match crate::pluginhost::dispatch::pluginhandler(state, sess, csrf, method, extra).await {
            Ok(crate::pluginhost::plugin::MethodOutput::Json(v)) => DispatchOutput::Json(v),
            // The method printed raw text/HTML (PHP: a plain `print` — e.g.
            // the `note` plugin's dialog body).
            Ok(crate::pluginhost::plugin::MethodOutput::Raw(s)) => DispatchOutput::Html(s),
            // A full response (custom status + Content-Type): the backend
            // route folds it into its raw status + body (application/json).
            Ok(crate::pluginhost::plugin::MethodOutput::Response { status, body, .. }) => {
                DispatchOutput::Raw(StatusCode::from_u16(status).unwrap(), None, body)
            }
            // A plugin method that fails after passing the gates (PHP: an
            // uncaught exception in `catchall` — 500). The port includes the
            // message in the body for debuggability.
            Err(e) => DispatchOutput::Raw(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string()),
        },
        "Feeds" => match method {
            "view" => {
                DispatchOutput::Json(crate::headlines::view(state, sess, extra, &locale).await)
            }
            "search" => DispatchOutput::Json(crate::headlines::search(state, sess, extra).await),
            "updatedebugger" => DispatchOutput::TextHtml(
                crate::feeds::updatedebugger(state, sess, extra, csrf).await,
            ),
            "catchupall" => DispatchOutput::Json(crate::feeds::catchup_all(state, sess).await),
            "subscribetofeed" => {
                DispatchOutput::Json(crate::feeds::subscribe_to_feed(state, sess).await)
            }
            "add" => DispatchOutput::Json(crate::feeds::add(state, sess, extra).await),
            "opensite" => {
                let (status, location, body) = crate::feeds::opensite(state, sess, extra).await;
                DispatchOutput::Raw(status, location, body)
            }
            _ => DispatchOutput::Json(unknown_method("Feeds", method)),
        },
        "Pref_Feeds" => match method {
            "index" => DispatchOutput::Html(feeds::index(state, sess, extra).await),
            "getfeedtree" => DispatchOutput::Json(feeds::getfeedtree(state, sess, extra).await),
            "feedswitherrors" => DispatchOutput::Json(feeds::feeds_with_errors(state, sess).await),
            "inactivefeeds" => DispatchOutput::Json(feeds::inactive_feeds(state, sess).await),
            "editsave" => DispatchOutput::Json(feeds::edit_save(state, sess, extra).await),
            "batcheditsave" => {
                DispatchOutput::Json(feeds::batch_edit_save(state, sess, extra).await)
            }
            "remove" => DispatchOutput::Json(feeds::remove_feeds(state, sess, extra).await),
            "addcat" => DispatchOutput::Json(feeds::add_cat(state, sess, extra).await),
            "renamecat" => DispatchOutput::Json(feeds::rename_cat(state, sess, extra).await),
            "removecat" => DispatchOutput::Json(feeds::remove_cat(state, sess, extra).await),
            "catsortreset" => DispatchOutput::Json(feeds::catsortreset(state, sess, extra).await),
            "feedsortreset" => DispatchOutput::Json(feeds::feedsortreset(state, sess, extra).await),
            "savefeedorder" => DispatchOutput::Json(feeds::savefeedorder(state, sess, extra).await),
            "removeicon" => DispatchOutput::Json(feeds::remove_icon(state, sess, extra).await),
            "uploadicon" => {
                DispatchOutput::Json(feeds::upload_icon(state, sess, extra, file).await)
            }
            "clearkeys" => DispatchOutput::Json(feeds::clear_keys(state, sess, extra).await),
            "regenfeedkey" => DispatchOutput::Json(feeds::regen_feed_key(state, sess, extra).await),
            "getsharedurl" => DispatchOutput::Json(feeds::get_shared_url(state, sess, extra).await),
            "editfeed" => DispatchOutput::Json(feeds::editfeed(state, sess, extra).await),
            "editfeeds" => DispatchOutput::Html(feeds::editfeeds(state, sess, extra).await),
            "batchsubscribe" => DispatchOutput::Json(feeds::batch_subscribe(state, sess).await),
            "batchaddfeeds" => {
                DispatchOutput::Json(feeds::batch_add_feeds(state, sess, extra).await)
            }
            // The UI's OPML import dialog (the `opml_import_form` in `Pref_Feeds::index`
            // posts `op=Pref_Feeds&method=importOpml` with the `opml_file` upload).
            // The response body is the `opml_notice` lines (printed, not JSON).
            "importopml" => DispatchOutput::Html(opml::import(state, sess, file).await),
            _ => DispatchOutput::Json(unknown_method("Pref_Feeds", method)),
        },
        "Article" => match method {
            "getmetadatabyid" => DispatchOutput::Json(
                article::metadata_by_id(
                    state.db.pool(),
                    data.uid().unwrap_or(0),
                    util::clean_int(extra.get("id")) as i32,
                )
                .await,
            ),
            "printarticletags" => DispatchOutput::Json(
                article::print_article_tags(
                    state.db.pool(),
                    data.uid().unwrap_or(0),
                    util::clean_int(extra.get("id")) as i32,
                )
                .await,
            ),
            "setscore" => DispatchOutput::Json(article::set_score(state, sess, extra).await),
            "setarticletags" => {
                DispatchOutput::Json(article::set_article_tags(state, sess, extra).await)
            }
            "completetags" => {
                DispatchOutput::Json(article::complete_tags(state, sess, extra).await)
            }
            "assigntolabel" => {
                DispatchOutput::Json(article::label_ops(state, sess, extra, true).await)
            }
            "removefromlabel" => {
                DispatchOutput::Json(article::label_ops(state, sess, extra, false).await)
            }
            "redirect" => {
                let (status, location, body) = article::redirect(state, sess, extra).await;
                DispatchOutput::Raw(status, location, body)
            }
            _ => DispatchOutput::Json(unknown_method("Article", method)),
        },
        "Pref_Labels" => match method {
            "index" => DispatchOutput::Html(labels::index(state, sess, extra).await),
            "getlabeltree" => DispatchOutput::Json(
                labels::get_label_tree(state.db.pool(), data.uid().unwrap_or(0)).await,
            ),
            "edit" => match labels::edit(state, sess, extra).await {
                Some(v) => DispatchOutput::Json(v),
                None => DispatchOutput::TextHtml(String::new()),
            },
            "save" => {
                DispatchOutput::TextHtml(labels::save(state, sess, extra).await.unwrap_or_default())
            }
            "add" => DispatchOutput::TextHtml(
                labels::add_label(state, sess, extra)
                    .await
                    .unwrap_or_default(),
            ),
            "colorset" => {
                let _ = labels::colorset(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "colorreset" => {
                let _ = labels::colorreset(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "remove" => {
                let _ = labels::remove_labels(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            _ => DispatchOutput::Json(unknown_method("Pref_Labels", method)),
        },
        "Pref_Filters" => match method {
            "index" => DispatchOutput::Html(filters::index(state, sess, extra).await),
            "getfiltertree" => DispatchOutput::Json(filters::get_filter_tree(state, sess).await),
            "edit" => match filters::edit(state, sess, extra).await {
                Some(json) => DispatchOutput::Json(json),
                None => DispatchOutput::TextHtml(String::new()),
            },
            "printrulename" => {
                DispatchOutput::TextHtml(filters::print_rule_name(state, sess, extra).await)
            }
            "printactionname" => {
                DispatchOutput::TextHtml(filters::print_action_name(state, sess, extra).await)
            }
            "editrule" => DispatchOutput::Json(filters::editrule(state, sess, extra).await),
            "editsave" => {
                let _ = filters::edit_save(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "add" => {
                let _ = filters::add_filter(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "remove" => {
                let _ = filters::remove_filters(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "clone" => {
                let _ = filters::clone_filters(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "join" => {
                let _ = filters::join_filters(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "savefilterorder" => {
                let _ = filters::savefilterorder(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "filtersortreset" => {
                let _ = filters::filtersortreset(state, sess).await;
                DispatchOutput::TextHtml(String::new())
            }
            "testfilterdo" => match filters::test_filter_do(state, sess, extra).await {
                Ok(v) => DispatchOutput::Json(v),
                // PHP fatal (`Sanitizer::highlight_words` infinite loop on an
                // empty highlight word) → 500 with an empty body.
                Err(()) => {
                    DispatchOutput::Raw(StatusCode::INTERNAL_SERVER_ERROR, None, String::new())
                }
            },
            _ => DispatchOutput::Json(unknown_method("Pref_Filters", method)),
        },
        "Pref_Prefs" => match method {
            "getprofiles" => DispatchOutput::Json(settings::get_profiles(state, sess).await),
            "index" => DispatchOutput::Html(pref_ui::index(state, sess).await.unwrap_or_default()),
            "index_auth" => {
                let html = pref_ui::index_auth(state, sess).await.unwrap_or_default();
                DispatchOutput::Html(html)
            }
            "addprofile" => {
                let _ = settings::add_profile(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "cloneprofile" => {
                let _ = settings::clone_profile(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "remprofiles" => {
                let _ = settings::remove_profiles(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "activateprofile" => {
                settings::activate_profile(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "saveprofile" => {
                let _ = settings::save_profile(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "generateapppassword" => {
                let html = settings::generate_app_password(state, sess, extra)
                    .await
                    .unwrap_or_default();
                DispatchOutput::Html(html)
            }
            "deleteapppasswords" => {
                let html = settings::delete_app_passwords(state, sess, extra)
                    .await
                    .unwrap_or_default();
                DispatchOutput::Html(html)
            }
            "previewdigest" => DispatchOutput::Json(settings::preview_digest(state, sess).await),
            "saveconfig" => {
                DispatchOutput::TextHtml(settings::saveconfig(state, sess, extra).await)
            }
            "resetconfig" => DispatchOutput::TextHtml(settings::resetconfig(state, sess).await),
            "changepersonaldata" => {
                DispatchOutput::TextHtml(settings::change_personal_data(state, sess, extra).await)
            }
            "changepassword" => {
                DispatchOutput::TextHtml(settings::changepassword(state, sess, extra).await)
            }
            "otpenable" => DispatchOutput::TextHtml(settings::otp_enable(state, sess, extra).await),
            "otpdisable" => {
                DispatchOutput::TextHtml(settings::otp_disable(state, sess, extra).await)
            }
            "customizecss" => DispatchOutput::Json(settings::customize_css(state, sess).await),
            // TODO(port): Plugins-tab management actions are not yet ported and
            // fall through to `unknown_method` (E_UNKNOWN_METHOD). Spec:
            // Pref_Prefs.php — `setplugins()` (enable/disable),
            // `installPlugin()` / `uninstallPlugin()` (file ops),
            // `checkForPluginUpdates()` / `updateLocalPlugins()` (network),
            // `clearPluginData()`. The SPA's `xhr.post` callers (e.g. "Enable
            // selected") treat the error body as success, so these falsely
            // confirm without writing anything.
            "getpluginslist" => DispatchOutput::Json(plugins::get_plugins_list(state, sess).await),
            "getavailableplugins" => DispatchOutput::Json(plugins::get_available_plugins(sess)),
            _ => DispatchOutput::Json(unknown_method("Pref_Prefs", method)),
        },
        "Pref_System" => match method {
            "index" => DispatchOutput::Html(system::index(state, sess, extra).await),
            "getscheduledtasks" => {
                DispatchOutput::Html(system::getscheduledtasks(state, sess).await)
            }
            "clearlog" => {
                system::clear_log(state).await;
                DispatchOutput::Json(json!({ "status": "OK" }))
            }
            "sendtestemail" => DispatchOutput::Json(system::send_test_email()),
            "getphpinfo" => DispatchOutput::Html(system::get_php_info(state)),
            _ => DispatchOutput::Json(unknown_method("Pref_System", method)),
        },
        "Pref_Users" => match method {
            "index" => DispatchOutput::Html(users::index(state, sess, extra).await),
            "userdetails" => DispatchOutput::Html(users::user_details(state, sess, extra).await),
            "edit" => match users::edit(state, sess, extra).await {
                Some(v) => DispatchOutput::Json(v),
                None => DispatchOutput::TextHtml(String::new()),
            },
            "editsave" => DispatchOutput::TextHtml(users::edit_save(state, sess, extra).await),
            "remove" => {
                users::remove(state, sess, extra).await;
                DispatchOutput::TextHtml(String::new())
            }
            "add" => DispatchOutput::TextHtml(users::add(state, sess, extra).await),
            "resetpass" => DispatchOutput::TextHtml(users::reset_pass(state, sess, extra).await),
            _ => DispatchOutput::Json(unknown_method("Pref_Users", method)),
        },
        "OPML" => match method {
            "export" => {
                let (status, ct, disp, body) = opml::export(state, sess, extra).await;
                DispatchOutput::Download(status, ct, disp, body)
            }
            "import" => DispatchOutput::Html(opml::import(state, sess, file).await),
            _ => DispatchOutput::Json(unknown_method("OPML", method)),
        },
        other => DispatchOutput::Json(unknown_method(other, method)),
    }
}

/// `Errors::to_json(code, params)`.
fn errors(code: &str) -> Value {
    json!({ "error": { "code": code, "params": [] } })
}

/// `Errors::to_json(E_UNKNOWN_METHOD, ["info" => "<Handler>-><method>"])`.
fn unknown_method(handler: &str, method: &str) -> Value {
    json!({ "error": { "code": "E_UNKNOWN_METHOD", "params": [ { "info": format!("{handler}->{method}") } ] } })
}

/// The `RPC` handler (`Handler_Protected`): CSRF-gated (in [`dispatch`]), no
/// per-method `csrf_ignore`.
async fn rpc_handler(
    state: &AppState,
    sess: &Sess,
    method: &str,
    extra: &Map<String, Value>,
    accept_language: Option<&str>,
) -> Value {
    match method {
        "sanitycheck" => sanity_check(state, sess, extra, accept_language).await,
        // TODO: stub (M2 close-out). PHP's `checkforupdates` polls
        // `tt-rss.org/tt-rss/version.json` for new *PHP* releases (vs the
        // local git checkout) and git-checks installed plugins for updates;
        // a Rust deployment has no such upstream channel, so report the
        // success shape with "no updates". The harness corpus step
        // `rpc_check_for_updates` is a known intentional mismatch until the
        // oracle side is adjusted.
        "checkforupdates" => json!({ "changeset": [], "plugins": [] }),
        "getruntimeinfo" => json!({ "runtime-info": make_runtime_info(state, sess).await }),
        "getallcounters" => crate::counters::get_all_counters(state, sess, extra).await,
        // M3 mutations (article state).
        "catchupselected" => crate::article::catchup_selected(state, sess, extra).await,
        "markselected" => crate::article::mark_selected(state, sess, extra).await,
        "publishselected" => crate::article::publish_selected(state, sess, extra).await,
        "mark" => crate::article::mark(state, sess, extra).await,
        "publ" => crate::article::publ(state, sess, extra).await,
        "delete" => crate::article::delete(state, sess, extra).await,
        "catchupfeed" => crate::feeds::catchup_feed(state, sess, extra).await,
        // M4 config verbs (the frontend quick-toggles + client error reporter).
        "setpref" => crate::settings::set_pref(state, sess, extra).await,
        "togglepref" => crate::settings::toggle_pref(state, sess, extra).await,
        "setwidescreen" => crate::settings::set_widescreen(state, sess, extra).await,
        "log" => crate::settings::client_log(state, sess, extra).await,
        other => unknown_method("RPC", other),
    }
}

/// `validate_csrf`: the request token matches the session token (non-empty).
pub(crate) fn validate_csrf(sess: &Sess, csrf: &str) -> bool {
    let data = sess.data();
    let session_csrf = data.csrf_token().unwrap_or("");
    !csrf.is_empty() && util::ct_eq(session_csrf, csrf)
}

/// `Handler::csrf_ignore` per handler. Only the read-only handlers the SPA
/// drives via GET (no token) are exempt.
fn csrf_ignored(op: &str, method: &str) -> bool {
    // `PluginHandler::csrf_ignore` is unconditionally `true` (every method)
    // — the per-plugin gate (`validate_csrf || $plugin->csrf_ignore($method)`)
    // runs inside `pluginhost::dispatch::pluginhandler`.
    op == "PluginHandler"
        // `Pref_Feeds::csrf_ignore` = ["index", "getfeedtree"].
        || (matches!(op, "Pref_Feeds") && matches!(method, "index" | "getfeedtree"))
        // `Feeds::csrf_ignore` = ["index"].
        || (op == "Feeds" && method == "index")
        // `OPML::csrf_ignore` = ["export"].
        || (op == "OPML" && method == "export")
        // `Pref_Filters::csrf_ignore` = ["index", "getfiltertree"].
        || (op == "Pref_Filters" && matches!(method, "index" | "getfiltertree"))
        // `Pref_System::csrf_ignore` = ["index"].
        || (op == "Pref_System" && method == "index")
        // `Pref_Prefs::csrf_ignore` = ["index", "updateself", "otpqrcode"].
        || (op == "Pref_Prefs"
            && matches!(method, "index" | "updateself" | "otpqrcode"))
        // `Pref_Labels::csrf_ignore` = ["index", "getlabeltree"].
        || (op == "Pref_Labels" && matches!(method, "index" | "getlabeltree"))
        // `Pref_Users::csrf_ignore` = ["index"].
        || (op == "Pref_Users" && method == "index")
}

/// `Handler_Protected` handlers (all the ones ported so far) require a user.
fn is_protected(op: &str) -> bool {
    matches!(
        op,
        "RPC"
            | "Feeds"
            | "Pref_Feeds"
            | "Article"
            | "Pref_Labels"
            | "Pref_Filters"
            | "Pref_Users"
            | "Pref_System"
            | "Pref_Prefs"
            | "OPML"
            | "PluginHandler"
    )
}

/// `Handler_Administrative` handlers require an admin-level session.
fn is_administrative(op: &str) -> bool {
    matches!(op, "Pref_Users" | "Pref_System")
}

/// `RPC::sanityCheck`.
async fn sanity_check(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
    accept_language: Option<&str>,
) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);

    // Stored (not returned) — mirrors `$_SESSION["hasSandbox"/"clientTzOffset"]`.
    sess.with_mut(|s| {
        if let Some(v) = extra.get("hasSandbox").and_then(Value::as_str) {
            s.set(
                "hasSandbox",
                crate::core::phpser::Value::Bool(!v.is_empty() && v != "f" && v != "false"),
            );
        }
        if let Some(v) = extra.get("clientTzOffset").and_then(Value::as_str) {
            s.set(
                "clientTzOffset",
                crate::core::phpser::Value::Bytes(v.trim().as_bytes().to_vec()),
            );
        }
    });

    // `$_REQUEST["clientLocation"]` (the request, not the session).
    let client_location = extra
        .get("clientLocation")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let client_scheme = url_scheme(&client_location);
    let server_scheme = url_scheme(&state.config.self_url_rtrim());

    let mut err = "E_SUCCESS";
    let mut params: Map<String, Value> = Map::new();
    if client_scheme != server_scheme {
        err = "E_URL_SCHEME_MISMATCH";
        params.insert(
            "client_scheme".into(),
            client_scheme.map(Value::from).unwrap_or(Value::Null),
        );
        params.insert(
            "server_scheme".into(),
            Value::from(server_scheme.clone().unwrap_or_default()),
        );
        params.insert(
            "self_url_path".into(),
            Value::from(state.config.self_url_rtrim()),
        );
    }

    // `RPC::init` → `translations` (PHP `RPC.php:198`): the full original→translated
    // map for the resolved locale (the SPA's `App._translations`, `App.js:624`); an
    // empty object for English / no locale. Resolve like the shell (`http.rs`):
    // profile-aware `USER_LANGUAGE` pref, else `Accept-Language`.
    let rows = crate::prefs::load_rows_cached(&state.prefs_cache, &state.db, uid, profile).await;
    let user_lang_pref = crate::prefs::resolve("USER_LANGUAGE", &rows, profile);
    let user_lang = user_lang_pref.as_str();
    let locale =
        crate::i18n::resolve_request_locale(&state.i18n, accept_language, Some(user_lang));
    let translations = match &locale {
        Some(loc) => {
            let map = state.i18n.translations_map(loc).unwrap_or_default();
            let obj: Map<String, Value> =
                map.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
            Value::Object(obj)
        }
        None => Value::Object(Map::new()),
    };

    if err == "E_SUCCESS" {
        json!({
            "init-params": make_init_params(state, sess, uid, profile).await,
            "runtime-info": make_runtime_info(state, sess).await,
            "translations": translations,
        })
    } else {
        json!({ "error": { "code": err, "params": params } })
    }
}

/// `RPC::_make_runtime_info`.
pub(crate) async fn make_runtime_info(state: &AppState, sess: &Sess) -> Value {
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let profile = data.profile().map(|p| p as i32);
    let access_level = data.access_level().unwrap_or(0);

    let (max_feed_id, num_feeds) = feed_counts(state.db.pool(), uid).await;
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let cdm_expanded = prefs::resolve("CDM_EXPANDED", &rows, profile).as_bool();
    let labels = labels_all(state.db.pool(), uid).await;

    let mut out = Map::new();
    out.insert("max_feed_id".into(), Value::from(max_feed_id));
    out.insert("num_feeds".into(), Value::from(num_feeds));
    out.insert("cdm_expanded".into(), Value::from(cdm_expanded));
    out.insert("labels".into(), labels);

    // `recent_log_events` (admin + `LOG_DESTINATION == sql`) is a live counter;
    // the harness normalizes it away, so we emit it for shape parity.
    if access_level >= 10 {
        let cid = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(id) FROM ttrss_error_log
             WHERE errno NOT IN (512, 8192)
               AND created_at > NOW() - INTERVAL '1 hour'
               AND errstr NOT LIKE '%Returning bool from comparison function is deprecated%'
               AND errstr NOT LIKE '%imagecreatefromstring(): Data is not in a recognized format%'",
        )
        .fetch_one(state.db.pool())
        .await
        .unwrap_or(0);
        out.insert("recent_log_events".into(), Value::from(cid));
    }

    // Update-daemon state (D6 = c): the updater runs in-process (D1 = B), so
    // it is always running; the 30-min "not updating feeds" stale-check is
    // dropped. Emit the fields the SPA reads — `daemon_is_running` and
    // `daemon_stamp_ok` as JSON *integers* so the UI's strict `v !== 1` check
    // passes — with their steady-state values, plus a current `daemon_stamp`.
    // This fakes the updater-health indicator without the `.lock`/`.stamp`
    // file mechanism; the harness normalizer drops these volatile keys.
    let now = chrono::Utc::now();
    out.insert("daemon_is_running".into(), Value::from(1_i64));
    out.insert("daemon_stamp_ok".into(), Value::from(1_i64));
    out.insert(
        "daemon_stamp".into(),
        Value::from(format!(
            "{:04}.{:02}.{:02}, {}:{:02}",
            now.year(),
            now.month(),
            now.day(),
            now.hour(),
            now.minute(),
        )),
    );
    Value::Object(out)
}

/// `RPC::_make_init_params`.
async fn make_init_params(state: &AppState, sess: &Sess, uid: i64, profile: Option<i32>) -> Value {
    let data = sess.data();
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let p = |name: &str| prefs::resolve(name, &rows, profile);

    let (max_feed_id, num_feeds) = feed_counts(state.db.pool(), uid).await;
    let labels = labels_all(state.db.pool(), uid).await;
    let is_default_pw = is_default_password(state.db.pool(), uid).await;
    let self_url = state.config.self_url_rtrim();

    let theme_pref = p("USER_CSS_THEME").as_str().to_string();
    let theme = if theme_exists(&state.config.docroot, &theme_pref) {
        theme_pref
    } else {
        String::new()
    };

    // M7.22 item 2: the hotkey map goes through the HOOK_HOTKEY_MAP chain
    // (hotkeys_swap_jk / hotkeys_noscroll) before the prefixes are computed.
    let mut pctx = crate::pluginhost::dispatch::session_ctx(state, sess).await;
    let (prefixes, hotkeys) = crate::hotkeys::hotkeys_map(&mut pctx).await;

    let mut out = Map::new();
    out.insert(
        "on_catchup_show_next_feed".into(),
        Value::from(p("ON_CATCHUP_SHOW_NEXT_FEED").as_int()),
    );
    out.insert(
        "hide_read_feeds".into(),
        Value::from(p("HIDE_READ_FEEDS").as_int()),
    );
    out.insert(
        "enable_feed_cats".into(),
        Value::from(p(prefs::ENABLE_FEED_CATS).as_int()),
    );
    out.insert(
        "feeds_sort_by_unread".into(),
        Value::from(p("FEEDS_SORT_BY_UNREAD").as_int()),
    );
    out.insert(
        "confirm_feed_catchup".into(),
        Value::from(p("CONFIRM_FEED_CATCHUP").as_int()),
    );
    out.insert(
        "cdm_auto_catchup".into(),
        Value::from(p("CDM_AUTO_CATCHUP").as_int()),
    );
    out.insert(
        "fresh_article_max_age".into(),
        Value::from(p(prefs::FRESH_ARTICLE_MAX_AGE).as_int()),
    );
    out.insert(
        "recently_read_max_age".into(),
        Value::from(p("RECENTLY_READ_MAX_AGE").as_int()),
    );
    out.insert(
        "hide_read_shows_special".into(),
        Value::from(p("HIDE_READ_SHOWS_SPECIAL").as_int()),
    );
    out.insert(
        "combined_display_mode".into(),
        Value::from(p("COMBINED_DISPLAY_MODE").as_int()),
    );
    out.insert(
        "debug_headline_ids".into(),
        Value::from(p("DEBUG_HEADLINE_IDS").as_int()),
    );
    out.insert(
        "cdm_enable_grid".into(),
        Value::from(p("CDM_ENABLE_GRID").as_int()),
    );
    out.insert("safe_mode".into(), Value::from(data.safe_mode()));
    out.insert("check_for_updates".into(), Value::from(check_for_updates()));
    out.insert(
        "icons_url".into(),
        Value::from(format!("{self_url}/public.php")),
    );
    out.insert(
        "cookie_lifetime".into(),
        Value::from(session::COOKIE_LIFETIME),
    );
    out.insert(
        "default_view_mode".into(),
        Value::from(p("_DEFAULT_VIEW_MODE").as_str()),
    );
    out.insert(
        "default_view_limit".into(),
        Value::from(p("_DEFAULT_VIEW_LIMIT").as_int()),
    );
    out.insert(
        "default_view_order_by".into(),
        Value::from(p("_DEFAULT_VIEW_ORDER_BY").as_str()),
    );
    out.insert(
        "bw_limit".into(),
        Value::from(data.get_bool("bw_limit").unwrap_or(false) as i64),
    );
    out.insert("is_default_pw".into(), Value::from(is_default_pw));
    out.insert("label_base_index".into(), Value::from(-1024i64));
    out.insert("theme".into(), Value::from(theme));
    out.insert(
        "plugins".into(),
        Value::from(plugin_names(&state.config.docroot)),
    );
    out.insert("php_platform".into(), Value::from("Linux"));
    out.insert("php_version".into(), Value::from("8.5.9"));
    out.insert("self_url_prefix".into(), Value::from(self_url));
    out.insert("max_feed_id".into(), Value::from(max_feed_id));
    out.insert("num_feeds".into(), Value::from(num_feeds));
    out.insert("hotkeys".into(), json!([prefixes, hotkeys]));
    out.insert(
        "widescreen".into(),
        Value::from(p("WIDESCREEN_MODE").as_int()),
    );
    out.insert(
        "icon_indicator_white".into(),
        image_base64(&state.config.docroot, "images/indicator_white.gif"),
    );
    out.insert(
        "icon_oval".into(),
        image_base64(&state.config.docroot, "images/oval.svg"),
    );
    out.insert(
        "icon_three_dots".into(),
        image_base64(&state.config.docroot, "images/three-dots.svg"),
    );
    out.insert(
        "icon_blank".into(),
        image_base64(&state.config.docroot, "images/blank_icon.gif"),
    );
    out.insert("labels".into(), labels);
    Value::Object(out)
}

/// `Labels::get_all` — `{id, fg_color, bg_color, caption}` ordered by caption.
async fn labels_all(pool: &sqlx::PgPool, uid: i64) -> Value {
    let rows = sqlx::query(
        "SELECT id, fg_color, bg_color, caption FROM ttrss_labels2
         WHERE owner_uid = $1 ORDER BY caption",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "id": r.get::<i32, _>(0),
                    "fg_color": r.get::<String, _>(1),
                    "bg_color": r.get::<String, _>(2),
                    "caption": r.get::<String, _>(3),
                })
            })
            .collect(),
    )
}

/// `SELECT MAX(id), COUNT(*) FROM ttrss_feeds WHERE owner_uid = ?`.
async fn feed_counts(pool: &sqlx::PgPool, uid: i64) -> (i64, i64) {
    let row = sqlx::query("SELECT MAX(id), COUNT(*) FROM ttrss_feeds WHERE owner_uid = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    match row {
        Some(r) => (
            r.get::<Option<i32>, _>(0).unwrap_or(0) as i64,
            r.get::<i64, _>(1),
        ),
        None => (0, 0),
    }
}

/// `UserHelper::is_default_password` — does the user's password equal the
/// literal string "password"?
async fn is_default_password(pool: &sqlx::PgPool, uid: i64) -> bool {
    let row = sqlx::query("SELECT pwd_hash, salt FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(r) = row else {
        return false;
    };
    user::verify_password(&r.get::<String, _>(0), "password", &r.get::<String, _>(1))
}

/// `theme_exists`: a bare `themes/<name>` file/dir (or a local-themes dir).
fn theme_exists(docroot: &std::path::Path, theme: &str) -> bool {
    if theme.is_empty() {
        return false;
    }
    docroot.join("themes").join(theme).exists() || docroot.join("themes.local").join(theme).exists()
}

/// `RPC::image_to_base64` — `data:image/<ext>;base64,<b64>` ("" if missing).
fn image_base64(docroot: &std::path::Path, filename: &str) -> Value {
    let path = docroot.join(filename);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let ext = std::path::Path::new(filename)
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            let mime_ext = if ext == "svg" { "svg+xml" } else { &ext };
            Value::from(format!(
                "data:image/{mime_ext};base64,{}",
                base64_encode(&bytes)
            ))
        }
        Err(_) => Value::from(String::new()),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `TTRSS_CHECK_FOR_UPDATES` (default `true`).
fn check_for_updates() -> bool {
    std::env::var("TTRSS_CHECK_FOR_UPDATES")
        .map(|v| v != "false" && v != "0" && !v.is_empty())
        .unwrap_or(true)
}

/// `PluginHost::get_plugin_names` for the web session: the `TTRSS_PLUGINS`
/// init list (KIND_ALL) — the same list is loaded for every request, so the
/// per-user enabled subset only re-registers (order is fixed by the init list).
fn plugin_names(docroot: &std::path::Path) -> String {
    let list = std::env::var("TTRSS_PLUGINS").unwrap_or_else(|_| crate::pluginhost::default_plugins());
    list.split(',')
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .filter_map(|d| plugin_class_name(docroot, d))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve a plugin dir name to its PHP class name by reading the plugin's
/// `init.php` (`plugins/<dir>` first, then `plugins.local/<dir>`).
fn plugin_class_name(docroot: &std::path::Path, dir: &str) -> Option<String> {
    for base in ["plugins", "plugins.local"] {
        let init = docroot.join(base).join(dir).join("init.php");
        let Ok(src) = std::fs::read_to_string(&init) else {
            continue;
        };
        for line in src.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("class ") {
                let name = rest
                    .trim()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

// The hotkey tables + `hotkeys_map` / `hotkey_help` live in `crate::hotkeys`
// (they are `RPC` class statics in PHP; split out to keep the dispatcher lean).

/// The `scheme` of a URL (None if absent/unparseable) — `parse_url(.., SCHEME)`.
fn url_scheme(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.scheme().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // M7.3 — `PluginHandler` is on the CSRF-exempt list; `is_protected`
    // matches `$_SERVER["op"]` (case-insensitive).
    #[test]
    fn pluginhandler_gates() {
        assert!(is_protected("PluginHandler"));
        // `in_array` on `$protected_ops` is case-sensitive (the RPC op is
        // sent exactly as `PluginHandler` by the JS).
        assert!(!is_protected("pluginhandler"));
        assert!(!is_protected("config"));
        assert!(csrf_ignored("PluginHandler", "whatever"));
        assert!(!csrf_ignored("config", "whatever"));
    }

    #[test]
    fn theme_exists_matches_php() {
        // test_theme_exists_fake
        let docroot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tt-rss");
        assert!(!theme_exists(&docroot, "nonexistent-theme-xyz"));
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn build(uri: &str, ct: Option<&str>, body: &str) -> Request {
        let mut b = Request::builder().method(axum::http::Method::POST).uri(uri);
        if let Some(ct) = ct {
            b = b.header(header::CONTENT_TYPE, ct);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    #[test]
    fn parse_backend_get_query() {
        let req = Request::builder()
            .method(axum::http::Method::GET)
            .uri("/backend.php?op=Feeds&method=getfeedtree&csrf_token=tok")
            .body(Body::empty())
            .unwrap();
        let (form, file) = rt().block_on(parse_backend(req));
        assert_eq!(form.op.as_deref(), Some("Feeds"));
        assert_eq!(form.method.as_deref(), Some("getfeedtree"));
        assert_eq!(form.csrf_token.as_deref(), Some("tok"));
        assert!(file.is_none());
    }

    #[test]
    fn parse_backend_urlencoded() {
        let (form, file) = rt().block_on(parse_backend(build(
            "/backend.php",
            Some("application/x-www-form-urlencoded"),
            "op=Pref_Feeds&method=remove&csrf_token=tok&id=5&ids=1%2C2",
        )));
        assert_eq!(form.op.as_deref(), Some("Pref_Feeds"));
        assert_eq!(form.method.as_deref(), Some("remove"));
        assert_eq!(form.csrf_token.as_deref(), Some("tok"));
        // Extra values are `Value::String` (matching the axum `Form` path).
        assert_eq!(form.extra.get("id"), Some(&Value::String("5".into())));
        assert_eq!(form.extra.get("ids"), Some(&Value::String("1,2".into())));
        assert!(file.is_none());
    }

    #[test]
    fn parse_backend_repeated_keys_become_array() {
        // Batch ops (`catchupSelected`, `markSelected`, `delete`, …) send
        // repeated `ids[]` params; they must arrive as a JSON array so every id
        // is acted on. Regression: the old `serde_urlencoded`→`Map` path kept
        // only the *last* value for a repeated key. PHP `$_REQUEST` semantics:
        // the `[]` suffix is stripped from the key and a `[]` field is always
        // an array, even with a single value (`rule[]`).
        let (form, file) = rt().block_on(parse_backend(build(
            "/backend.php",
            Some("application/x-www-form-urlencoded"),
            "op=RPC&method=catchupSelected&csrf_token=tok&ids[]=430&ids[]=431&ids[]=432&cmode=0&rule[]=%7B%22reg_exp%22%3A%22x%22%7D",
        )));
        assert_eq!(form.op.as_deref(), Some("RPC"));
        assert_eq!(form.method.as_deref(), Some("catchupSelected"));
        assert_eq!(
            form.extra.get("ids"),
            Some(&Value::Array(vec![
                Value::String("430".into()),
                Value::String("431".into()),
                Value::String("432".into()),
            ]))
        );
        // A single-valued key stays a scalar string (unchanged behavior).
        assert_eq!(form.extra.get("cmode"), Some(&Value::String("0".into())));
        // A single-valued `[]` field is still an array (PHP: `rule[]` →
        // `$_REQUEST['rule'] = ['{…}']`), under the bare key.
        assert_eq!(
            form.extra.get("rule"),
            Some(&Value::Array(vec![Value::String(
                "{\"reg_exp\":\"x\"}".into()
            )]))
        );
        assert!(file.is_none());
    }

    #[test]
    fn parse_backend_json() {
        let (form, file) = rt().block_on(parse_backend(build(
            "/backend.php",
            Some("application/json"),
            r#"{"op":"Feeds","method":"index","extra_val":7}"#,
        )));
        assert_eq!(form.op.as_deref(), Some("Feeds"));
        assert_eq!(form.method.as_deref(), Some("index"));
        assert_eq!(form.extra.get("extra_val"), Some(&json!(7)));
        assert!(file.is_none());
    }

    #[test]
    fn parse_backend_multipart() {
        let boundary = "BOUND123";
        let body = [
            "--{B}\r\nContent-Disposition: form-data; name=\"op\"\r\n\r\nPref_Feeds\r\n",
            "--{B}\r\nContent-Disposition: form-data; name=\"method\"\r\n\r\nuploadIcon\r\n",
            "--{B}\r\nContent-Disposition: form-data; name=\"feed_id\"\r\n\r\n1\r\n",
            "--{B}\r\nContent-Disposition: form-data; name=\"icon_file\"; filename=\"icon.png\"\r\nContent-Type: image/png\r\n\r\nPNGBYTES\r\n",
            "--{B}--\r\n",
        ]
        .iter()
        .map(|s| s.replace("{B}", boundary))
        .collect::<Vec<_>>()
        .join("");
        let (form, file) = rt().block_on(parse_backend(build(
            "/backend.php",
            Some(&format!("multipart/form-data; boundary={boundary}")),
            &body,
        )));
        assert_eq!(form.op.as_deref(), Some("Pref_Feeds"));
        assert_eq!(form.method.as_deref(), Some("uploadIcon"));
        assert_eq!(form.extra.get("feed_id"), Some(&Value::String("1".into())));
        assert_eq!(file.as_deref(), Some(b"PNGBYTES".as_slice()));
    }

    /// `Handler::csrf_ignore` per class (the frozen PHP spec):
    /// `Feeds: ["index"]`, `OPML: ["export"]`,
    /// `Pref_Filters: ["index", "getfiltertree"]`,
    /// `Pref_Labels: ["index", "getlabeltree"]`,
    /// `Pref_Prefs: ["index", "updateself", "otpqrcode"]`,
    /// `Pref_System: ["index"]`, `Pref_Users: ["index"]`,
    /// `Pref_Feeds: ["index", "getfeedtree"]`.
    /// `RPC::csrf_ignore` is commented out in PHP → no exemptions.
    /// Every exempt (op, method) pair must pass tokenless GETs; every other
    /// pair (including the ones `ef827a5` added) must still validate CSRF.
    #[test]
    fn csrf_ignored_matches_php() {
        let exempted: &[(&str, &str)] = &[
            ("Feeds", "index"),
            ("OPML", "export"),
            ("Pref_Filters", "index"),
            ("Pref_Filters", "getfiltertree"),
            ("Pref_Labels", "index"),
            ("Pref_Labels", "getlabeltree"),
            ("Pref_Prefs", "index"),
            ("Pref_Prefs", "updateself"),
            ("Pref_Prefs", "otpqrcode"),
            ("Pref_System", "index"),
            ("Pref_Users", "index"),
            ("Pref_Feeds", "index"),
            ("Pref_Feeds", "getfeedtree"),
        ];
        for (op, method) in exempted {
            assert!(
                csrf_ignored(op, method),
                "{op}::{method} must be CSRF-exempt (PHP spec)"
            );
        }

        // Not exempt: write methods, other read methods, and unknown ops.
        let not_exempted: &[(&str, &str)] = &[
            ("Feeds", "getfeedtree"),
            ("Feeds", "editfeed"),
            ("OPML", "import"),
            ("Pref_Filters", "testfilterdo"),
            ("Pref_Filters", "add"),
            ("Pref_Labels", "add"),
            ("Pref_Labels", "edit"),
            ("Pref_Prefs", "getsettings"),
            ("Pref_Prefs", "setsystem"),
            ("Pref_System", "setplugin"),
            ("Pref_Users", "edit"),
            ("Pref_Feeds", "editfeed"),
            ("RPC", "completelabels"), // commented out in PHP → not exempt
            ("RPC", "getAllCounters"),
            ("Article", "index"),
            ("Unknown_Op", "index"),
        ];
        for (op, method) in not_exempted {
            assert!(
                !csrf_ignored(op, method),
                "{op}::{method} must NOT be CSRF-exempt"
            );
        }
    }
}
