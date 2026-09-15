//! Password recovery — the public `op=forgotpass` handler (a faithful port of
//! `Handler_Public::forgotpass()`).
//!
//! Three branches over a minimal Dojo page (the `forgotpass.html.tpl` shell with
//! a `__CONTENT__` slot):
//! - `hash` present (the reset-link GET) → validate the 15-hour token and reset
//!   the password;
//! - no `method` (first GET) → the request form (login + email + an arithmetic
//!   CAPTCHA);
//! - `method = "do"` (the form POST) → validate + store a fresh `resetpass_token`.
//!
//! The outgoing mail is a no-op: the Rust server does not wire up the system MTA
//! (the PHP `Mailer`), so the reset e-mail is not delivered — but the token is
//! stored and the flow completes up to delivery, and the user still sees the new
//! password on the "Completed." page. This mirrors `Pref_System::sendTestEmail`.

use crate::core::config::Config;
use crate::core::phpser::Value;
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use sqlx::PgPool;

use crate::render;
use crate::session;
use crate::users;
use crate::util;
use crate::AppState;

/// The public `op=forgotpass` HTML shell.
const TEMPLATE: &str = include_str!("../templates/forgotpass.html.tpl");

/// The 15-hour reset-link validity window (PHP `time() - 15*60*60`).
const RESET_LINK_TTL: i64 = 15 * 60 * 60;

/// The `op=forgotpass` request params (extracted from the merged GET+POST
/// `$_REQUEST`; `login` may arrive via the reset link (GET) or the form (POST)).
pub struct Params<'a> {
    /// `$_REQUEST["hash"]` — the reset token (the reset-link GET).
    pub hash: Option<&'a str>,
    /// `clean($_REQUEST["login"])`.
    pub login: &'a str,
    /// `clean($_POST["email"])` (the form).
    pub email: Option<&'a str>,
    /// `clean($_POST["test"])` (the arithmetic-CAPTCHA answer).
    pub test: Option<&'a str>,
    /// `$_POST["csrf_token"]`.
    pub csrf_token: Option<&'a str>,
    /// `$_POST["method"]` — `"do"` on submit.
    pub method: Option<&'a str>,
}

/// `Handler_Public::forgotpass`: render the password-recovery page (or 403).
pub async fn forgotpass(state: &AppState, sess: &session::Sess, p: &Params<'_>) -> Response {
    // `Config::get(Config::DISABLE_LOGIN_FORM) || !str_contains(Config::PLUGINS, "auth_internal")`.
    if state.config.disable_login_form || !state.config.plugins.contains("auth_internal") {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from("Forbidden."))
            .expect("valid response");
    }

    // Ensure a session id exists so the CAPTCHA/CSRF persist across the GET and
    // POST (PHP `session_start()` creates one on first access), and set the cookie
    // so the browser remembers it.
    let sid = if sess.id().is_empty() {
        let id = crate::core::session::new_session_id();
        sess.replace(id.clone(), sess.data());
        id
    } else {
        sess.id()
    };

    // `if (empty($_SESSION['csrf_token'])) $_SESSION['csrf_token'] = bin2hex(get_random_bytes(16));`
    if sess.data().csrf_token().is_none() {
        sess.with_mut(|d| {
            d.set(
                "csrf_token",
                Value::Bytes(util::random_hex(16).into_bytes()),
            );
        });
    }

    let pool = state.db.pool();
    let now = util::now_unix();
    let hash = p.hash.map(util::clean_str);
    let login = util::clean_str(p.login);

    let content = if hash.as_deref().is_some_and(|h| !h.is_empty()) {
        // The reset link: `?op=forgotpass&hash=…&login=…`.
        let c = reset_link(pool, hash.as_deref().unwrap(), &login, now).await;
        format!("{c}<a href='index.php'>Return to Tiny Tiny RSS</a>")
    } else if p.method.is_none() {
        initial_form(sess)
    } else if p.method.is_some_and(|m| m == "do") {
        do_reset(state, sess, pool, p, now).await
    } else {
        // `method` present but not `"do"` (and no hash): PHP prints nothing.
        String::new()
    };

    let html = render_shell(&state.config, &content);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            header::SET_COOKIE,
            format!("{}={sid}; path=/", session::SESSION_NAME),
        )
        .body(Body::from(html))
        .expect("valid response")
}

// ── branch bodies ────────────────────────────────────────────────────────────

/// The first GET: the request form. Stores the arithmetic-CAPTCHA operands in
/// the session (so the POST can verify the answer) and renders the form.
fn initial_form(sess: &session::Sess) -> String {
    let v1 = rand_range(1, 10);
    let v2 = rand_range(1, 10);
    sess.with_mut(|d| {
        d.set("pwdreset:testvalue1", Value::Int(v1));
        d.set("pwdreset:testvalue2", Value::Int(v2));
    });
    let csrf = sess.data().csrf_token().unwrap_or("").to_string();

    let mut out = String::new();
    out.push_str(&notice_html(
        "You will need to provide valid account name and email. Password reset link will be sent to your email address.",
    ));
    out.push_str("<form method='POST' action='public.php'>");
    out.push_str("<input type='hidden' name='method' value='do'>");
    out.push_str("<input type='hidden' name='op' value='forgotpass'>");
    out.push_str(&format!(
        "<input type='hidden' name='csrf_token' value='{csrf}'>"
    ));
    out.push_str("<fieldset>");
    out.push_str("<label>Login:</label>");
    out.push_str(
        "<input dojoType='dijit.form.TextBox' type='text' name='login' value='' required>",
    );
    out.push_str("</fieldset>");
    out.push_str("<fieldset>");
    out.push_str("<label>Email:</label>");
    out.push_str(
        "<input dojoType='dijit.form.TextBox' type='email' name='email' value='' required>",
    );
    out.push_str("</fieldset>");
    out.push_str("<fieldset>");
    out.push_str(&format!("<label>How much is {v1} + {v2}:</label>"));
    out.push_str("<input dojoType='dijit.form.TextBox' type='text' name='test' value='' required>");
    out.push_str("</fieldset>");
    out.push_str("<hr/>");
    out.push_str("<fieldset>");
    out.push_str("<button dojoType='dijit.form.Button' type='submit' class='alt-danger'>Reset password</button>");
    out.push_str("<a href='index.php'>Return to Tiny Tiny RSS</a>");
    out.push_str("</fieldset>");
    out.push_str("</form>");
    out
}

/// The `method=do` POST: validate the CSRF token + arithmetic CAPTCHA + fields,
/// look up the user, then store a fresh `resetpass_token` (and "email" the link).
async fn do_reset(
    state: &AppState,
    sess: &session::Sess,
    pool: &PgPool,
    p: &Params<'_>,
    now: i64,
) -> String {
    let data = sess.data();
    let session_csrf = data.csrf_token().unwrap_or("");
    let t1 = data.get_int("pwdreset:testvalue1").unwrap_or(0);
    let t2 = data.get_int("pwdreset:testvalue2").unwrap_or(0);

    let login = util::clean_str(p.login);
    let email = p.email.map(util::clean_str);
    let test = p.test.map(util::clean_str);
    let csrf = p.csrf_token.map(util::clean_str);

    let csrf_ok = csrf
        .as_deref()
        .is_some_and(|c| !c.is_empty() && util::ct_eq(session_csrf, c));
    let test_ok = test.as_deref().and_then(|t| t.parse::<i64>().ok()) == Some(t1 + t2);
    let fields_ok = email.as_deref().is_some_and(|e| !e.is_empty()) && !login.is_empty();

    if !(csrf_ok && test_ok && fields_ok) {
        return format!(
            "{}{}",
            error_html("Some of the required form parameters are missing or incorrect."),
            go_back_form("alt-primary")
        );
    }

    // `prevent submitting this form multiple times`: regenerate the operands.
    sess.with_mut(|d| {
        d.set("pwdreset:testvalue1", Value::Int(rand_range(1, 1000)));
        d.set("pwdreset:testvalue2", Value::Int(rand_range(1, 1000)));
    });

    let email = email.as_deref().unwrap_or("");
    let user: Option<(i32,)> =
        sqlx::query_as("SELECT id FROM ttrss_users WHERE LOWER(login) = LOWER($1) AND email = $2")
            .bind(&login)
            .bind(email)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    match user {
        Some((uid,)) => {
            let token = util::sha1_hex(util::get_random_bytes(128));
            let link = format!(
                "{}/public.php?op=forgotpass&hash={}&login={}",
                state.config.self_url_rtrim(),
                token,
                render::urlencode(&login)
            );
            // `Mailer::mail(…)` — no-op: the MTA is not wired up (see module docs).
            tracing::info!(
                to = %email,
                link = %link,
                "forgotpass: reset e-mail not sent (MTA not wired up)"
            );
            let _ = sqlx::query("UPDATE ttrss_users SET resetpass_token = $1 WHERE id = $2")
                .bind(format!("{}:{}", now, token))
                .bind(uid)
                .execute(pool)
                .await;
            format!(
                "{}<a href='index.php'>Return to Tiny Tiny RSS</a>",
                notice_html("Password reset instructions are being sent to your email address.")
            )
        }
        None => format!(
            "{}{}",
            error_html("Sorry, login and email combination not found."),
            go_back_form("")
        ),
    }
}

/// The reset link: validate the `time():token` (within the 15-hour window), then
/// clear the token and reset the password (the new password is shown as a notice).
async fn reset_link(pool: &PgPool, hash: &str, login: &str, now: i64) -> String {
    let err = error_html("Some of the information provided is missing or incorrect.");
    let row: Option<(i32, Option<String>)> = sqlx::query_as(
        "SELECT id, resetpass_token FROM ttrss_users WHERE LOWER(login) = LOWER($1)",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let Some((uid, resetpass_token)) = row else {
        return err;
    };
    let Some(token) = resetpass_token else {
        return err;
    };
    let (timestamp, stored) = match token.split_once(':') {
        Some((ts, tok)) => (ts, tok),
        None => return err,
    };
    let ts: i64 = match timestamp.parse() {
        Ok(v) => v,
        Err(_) => return err,
    };

    if !timestamp.is_empty() && !stored.is_empty() && ts >= now - RESET_LINK_TTL && stored == hash {
        let _ = sqlx::query("UPDATE ttrss_users SET resetpass_token = NULL WHERE id = $1")
            .bind(uid)
            .execute(pool)
            .await;
        let msg = users::reset_password(pool, uid, "").await;
        format!("{}<p>Completed.</p>", notice_html(&msg))
    } else {
        err
    }
}

// ── small helpers ────────────────────────────────────────────────────────────

/// `Handler_Public::forgotpass` shell: the template with the dynamic values.
fn render_shell(config: &Config, content: &str) -> String {
    let doc = config.docroot.as_path();
    let scripts_ts = render::scripts_timestamp(doc);
    let theme_light = render::theme_path(doc, "light.css", "themes/light.css");
    let theme_dark = render::theme_path(doc, "night.css", "themes/night.css");
    TEMPLATE
        .replace("__SCRIPTS_TS__", &scripts_ts.to_string())
        .replace("__THEME_LIGHT__", &theme_light)
        .replace("__THEME_DARK__", &theme_dark)
        .replace("__CONTENT__", content)
}

/// `format_notice` — `<div class="alert alert-info">…</div>`.
fn notice_html(msg: &str) -> String {
    format!("<div class=\"alert alert-info\" id=\"\">{msg}</div>")
}

/// `format_error` — `<div class="alert alert-danger">…</div>`.
fn error_html(msg: &str) -> String {
    format!("<div class=\"alert alert-danger\" id=\"\">{msg}</div>")
}

/// The "Go back" GET form (the `alt-primary` class is only on the invalid-params
/// variant; the not-found variant has no class).
fn go_back_form(class: &str) -> String {
    if class.is_empty() {
        "<form method='GET' action='public.php'>\
         <input type='hidden' name='op' value='forgotpass'>\
         <button dojoType='dijit.form.Button' type='submit'>Go back</button>\
         </form>"
            .to_string()
    } else {
        format!(
            "<form method='GET' action='public.php'>\
             <input type='hidden' name='op' value='forgotpass'>\
             <button dojoType='dijit.form.Button' type='submit' class='{class}'>Go back</button>\
             </form>"
        )
    }
}

/// `random_int(1, n)`-style uniform integer in `[lo, hi]` (inclusive).
fn rand_range(lo: i64, hi: i64) -> i64 {
    use rand::Rng;
    rand::thread_rng().gen_range(lo..=hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_hex_matches_php() {
        // PHP: sha1("") = da39a3ee5e6b4b0d3255bfef95601890afd80709.
        assert_eq!(util::sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn ct_eq_basic() {
        assert!(util::ct_eq("abc", "abc"));
        assert!(!util::ct_eq("abc", "abd"));
        assert!(!util::ct_eq("abc", "abcd"));
        assert!(util::ct_eq("", ""));
    }

    #[test]
    fn go_back_form_variants() {
        assert!(go_back_form("alt-primary").contains("class='alt-primary'"));
        assert!(!go_back_form("").contains("class="));
        assert!(go_back_form("").contains("Go back"));
    }
}
