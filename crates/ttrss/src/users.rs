//! `Pref_Users` backend read handlers.
//!
//! The `index` page is reproduced from the frozen PHP template
//! (`classes/Pref_Users.php`) with its exact whitespace: the harness keeps
//! whitespace *inside* a tag / before a text node (only whitespace strictly
//! between two tags is canonicalized away), so the button/menu labels carry
//! their template tabs. PHP's rule that the newline immediately after `?>`
//! is consumed is baked into the templates below.

use sqlx::Row;
use sqlx::AssertSqlSafe;

use crate::core::user;
use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util::{
    checkbox, clean_int, clean_str, htmlspecialchars, make_password, param_int_array,
};
use crate::AppState;
use serde_json::{json, Map, Value};
use crate::i18n::__;

/// `UserHelper::ACCESS_LEVEL_*` → display name (the `$access_level_names` map
/// built in `backend.php`).
fn access_level_name(level: i32) -> String {
    match level {
        -2 => __("Disabled"),
        -1 => __("Read Only"),
        0 => __("User"),
        5 => __("Power User"),
        10 => __("Administrator"),
        _ => String::new(),
    }
}

const INDEX_PRE: &str = r#"<div dojoType='dijit.layout.BorderContainer' gutters='false'>
				<div style='padding : 0px' dojoType='dijit.layout.ContentPane' region='top'>
					<div dojoType='fox.Toolbar'>

						<div style='float : right'>
							<form dojoType="dijit.form.Form" onsubmit="Users.reload(); return false;">
								<input dojoType='dijit.form.TextBox' id='user_search' size='20' type='search'
									value=""#;

const INDEX_POST: &str = r#"">
								<button dojoType='dijit.form.Button' type='submit'>
									Search								</button>
							</form>
						</div>

						<div dojoType='fox.form.DropDownButton'>
							<span>Select</span>
							<div dojoType='dijit.Menu' style='display: none'>
								<div onclick="Tables.select('users-list', true)"
									dojoType='dijit.MenuItem'>All</div>
								<div onclick="Tables.select('users-list', false)"
									dojoType='dijit.MenuItem'>None</div>
								</div>
							</div>

						<button dojoType='dijit.form.Button' onclick='Users.add()'>
						@CREATE_USER@						</button>

						<button dojoType='dijit.form.Button' onclick='Users.removeSelected()'>
						@REMOVE@						</button>

						<button dojoType='dijit.form.Button' onclick='Users.resetSelected()'>
						@RESET_PW@						</button>
						"#;
// M7.22 item 7 — the `HOOK_PREFS_TAB_SECTION` ("prefUsersToolbar") site:
// PHP prints the chain's output here, inside the toolbar after the
// "Reset password" button. The placeholder is spliced in `index`.
const INDEX_POST_SECTION: &str = r#"[__pref_users_toolbar__]
						
					</div>
				</div>
				<div style='padding : 0px' dojoType='dijit.layout.ContentPane' region='center'>

					<table width='100%' class='users-list' id='users-list'>

						<tr>
							<th></th>
							<th><a href='#' onclick="Users.reload('login')">Login</a></th>
							<th><a href='#' onclick="Users.reload('access_level')">Access level</a></th>
							<th><a href='#' onclick="Users.reload('num_feeds')">Subscribed feeds</a></th>
							<th><a href='#' onclick="Users.reload('created')">Registered</a></th>
							<th><a href='#' onclick="Users.reload('last_login')">Last login</a></th>
						</tr>"#;

const INDEX_ROW: &str = r#"<tr data-row-id='@ID@' onclick='Users.edit(@ID@)' title="@CLICK_EDIT@">
									<td class='checkbox'>
										<input onclick='Tables.onRowChecked(this); event.stopPropagation();'
										dojoType='dijit.form.CheckBox' type='checkbox'>
									</td>

									<td width='30%'>
										<i class='material-icons'>person</i>
										<strong>@LOGIN@</strong>
									</td>
									<td>@LEVEL@</td>
									<td>@NUMFEEDS@</td>
									<td class='text-muted'>@CREATED@</td>
									<td class='text-muted'>@LASTLOGIN@</td>
								</tr>"#;

const INDEX_FOOTER: &str = r#"</table>
				</div>
							#[__pref_users_tabs__]</div>"#;

/// `Pref_Users::index` — the user-management page (toolbar + user list table).
pub async fn index(
    state: &AppState,
    sess: &Sess,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let pool = state.db.pool();
    let tz = TzCtx::for_session(state, &sess.data()).await;

    let user_search = extra
        .get("search")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let like = format!("%{}%", user_search);
    let rows = sqlx::query(
        "SELECT u.id, u.login, u.access_level, COUNT(f.id) AS num_feeds, \
               to_char(u.created, 'YYYY-MM-DD HH24:MI:SS') AS created, \
               to_char(u.last_login, 'YYYY-MM-DD HH24:MI:SS') AS last_login \
         FROM ttrss_users u \
         LEFT JOIN ttrss_feeds f ON f.owner_uid = u.id \
         WHERE u.login LIKE $1 \
         GROUP BY u.id \
         ORDER BY u.login",
    )
    .bind(like)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // M7.22 item 7 — the `Pref_Users::index` plugin chains: the toolbar
    // section and the tab (after the table, before the page closes).
    let toolbar = crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB_SECTION,
        "prefUsersToolbar",
    )
    .await;
    let tabs = crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB,
        "prefUsers",
    )
    .await;

    let mut out = String::new();
    let create_user = __("Create user");
    let remove_lbl = __("Remove");
    let reset_pw = __("Reset password");
    let click_edit = __("Click to edit");
    out.push_str(INDEX_PRE);
    out.push_str(&htmlspecialchars(&user_search));
    out.push_str(
        &INDEX_POST
            .replace("@CREATE_USER@", &create_user)
            .replace("@REMOVE@", &remove_lbl)
            .replace("@RESET_PW@", &reset_pw),
    );
    // The `prefUsersToolbar` hook site (`HOOK_PREFS_TAB_SECTION`) sits at the
    // start of INDEX_POST_SECTION (after the "Reset password" button); splice
    // the hook chain's output in there.
    out.push_str(&INDEX_POST_SECTION.replace("[__pref_users_toolbar__]", &toolbar));

    for r in &rows {
        let id: i32 = r.get(0);
        let login: String = r.get(1);
        let access_level: i32 = r.get(2);
        let num_feeds: i64 = r.get(3);
        let created: Option<String> = r.get(4);
        let last_login: Option<String> = r.get(5);
        let id_s = id.to_string();
        out.push_str(
            &INDEX_ROW
                .replace("@ID@", &id_s)
                .replace("@LOGIN@", &htmlspecialchars(&login))
                .replace("@LEVEL@", &access_level_name(access_level))
                .replace("@NUMFEEDS@", &num_feeds.to_string())
                .replace("@CLICK_EDIT@", &click_edit)
                .replace(
                    "@CREATED@",
                    &make_local_datetime(created.as_deref(), false, &tz),
                )
                .replace(
                    "@LASTLOGIN@",
                    &make_local_datetime(last_login.as_deref(), false, &tz),
                ),
        );
    }

    // The `prefUsers` tab hook site (`HOOK_PREFS_TAB`) sits in INDEX_FOOTER
    // (after the table); splice the hook chain's output in there.
    out.push_str(&INDEX_FOOTER.replace("#[__pref_users_tabs__]", &tabs));
    out
}

/// `Pref_Users::userdetails` — the per-user details panel (or "User not
/// found"). Reads `$_REQUEST["id"]` (the corpus passes `user`, so id is 0).
pub async fn user_details(
    state: &AppState,
    sess: &Sess,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let pool = state.db.pool();
    let tz = TzCtx::for_session(state, &sess.data()).await;

    let id = crate::util::clean_int(extra.get("id")) as i32;

    let user = sqlx::query(
        "SELECT u.login, u.access_level, \
               SUBSTRING_FOR_DATE(u.created, 1, 16) AS created, \
               SUBSTRING_FOR_DATE(u.last_login, 1, 16) AS last_login, \
               (SELECT COUNT(ue.int_id) FROM ttrss_user_entries ue \
                WHERE ue.owner_uid = u.id) AS stored_articles \
         FROM ttrss_users u WHERE u.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    let Some(user) = user else {
        return crate::util::format_error(&__("User not found"), "");
    };

    let created: Option<String> = user.get(2);
    let last_login: Option<String> = user.get(3);
    let stored_articles: i64 = user.get(4);

    let feeds = sqlx::query(
        "SELECT id, title, site_url FROM ttrss_feeds WHERE owner_uid = $1 ORDER BY LOWER(title)",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!(
        "<fieldset>\n<label>Registered:</label>\n{}\n</fieldset>\n\
         <fieldset>\n<label>Last logged in:</label>\n{}\n</fieldset>\n\
         <fieldset>\n<label>Subscribed feeds:</label>\n{}\n</fieldset>\n\
         <fieldset>\n<label>Stored articles:</label>\n{}\n</fieldset>\n\
         <ul class=\"panel panel-scrollable list list-unstyled\">",
        make_local_datetime(created.as_deref(), false, &tz),
        make_local_datetime(last_login.as_deref(), false, &tz),
        feeds.len(),
        stored_articles,
    ));

    for f in &feeds {
        let fid: i32 = f.get(0);
        let title: String = f.get(1);
        let site_url: Option<String> = f.get(2);
        let icon_url = if state.config.feed_icon_dir().join(fid.to_string()).exists() {
            format!(
                "{}/public.php?op=feed_icon&id={}",
                state.config.self_url_rtrim(),
                fid
            )
        } else {
            "images/blank_icon.gif".to_string()
        };
        out.push_str(&format!(
            "<li>\n<img class=\"icon\" src=\"{}\">\n\
             <a target=\"_blank\" href=\"{}\">{}</a>\n</li>",
            htmlspecialchars(&icon_url),
            htmlspecialchars(site_url.as_deref().unwrap_or("")),
            htmlspecialchars(&title),
        ));
    }
    out.push_str("</ul>");
    out
}

/// `$access_level_names` (built in `backend.php`) as a JSON object. PHP
/// `json_encode` of the non-sequential int-keyed array yields an object with
/// string keys (order preserved, but functionally a lookup map).
fn access_level_names_json() -> Value {
    json!({
        "-2": __("Disabled"),
        "-1": __("Read Only"),
        "0": __("User"),
        "5": __("Power User"),
        "10": __("Administrator"),
    })
}

/// `UserHelper::find_user_by_login`: the user id matching `$login` case-
/// insensitively (or `None`).
async fn find_user_by_login(pool: &sqlx::PgPool, login: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT id FROM ttrss_users WHERE LOWER(login) = LOWER($1) LIMIT 1")
        .bind(login)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// `UserHelper::reset_password` — set user `$uid`'s password to `$new_password`
/// (or a fresh random one) and return the message. `$format_output` is `false`
/// on the admin paths (a plain `print`, not `print_notice`).
pub(crate) async fn reset_password(pool: &sqlx::PgPool, uid: i32, new_password: &str) -> String {
    let row: Option<String> = sqlx::query_scalar("SELECT login FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(login) = row else {
        return __("User not found");
    };
    let new_salt = user::get_salt();
    let tmp_pwd = if new_password.is_empty() {
        make_password(12)
    } else {
        new_password.to_string()
    };
    let pwd_hash = user::hash_password(&tmp_pwd, &new_salt, user::ALGO_SSHA512).unwrap_or_default();
    let _ = sqlx::query("UPDATE ttrss_users SET pwd_hash = $1, salt = $2 WHERE id = $3")
        .bind(&pwd_hash)
        .bind(&new_salt)
        .bind(uid)
        .execute(pool)
        .await;
    format!("Changed password of user <strong>{login}</strong> to <strong>{tmp_pwd}</strong>")
}

/// `Pref_Users::edit` — the per-user edit dialog data (or `None` if absent).
pub async fn edit(state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> Option<Value> {
    let pool = state.db.pool();
    let id = clean_int(extra.get("id")) as i32;
    let row: Option<(i32, String, i32, String, String, bool)> = sqlx::query_as(
        "SELECT id, login, access_level, email, full_name, otp_enabled \
         FROM ttrss_users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let (id, login, access_level, email, full_name, otp_enabled) = row?;
    Some(json!({
        "user": {
            "id": id,
            "login": login,
            "access_level": access_level,
            "email": email,
            "full_name": full_name,
            "otp_enabled": otp_enabled,
        },
        "access_level_names": access_level_names_json(),
    }))
}

/// `Pref_Users::editSave` — update a user's login/access level/email/OTP and,
/// if a password was posted, reset it. Returns the password-reset message
/// (empty when no reset happened).
pub async fn edit_save(state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let id = clean_int(extra.get("id")) as i32;
    let password = clean_str(extra.get("password").and_then(Value::as_str).unwrap_or(""));
    let user_exists: Option<i32> = sqlx::query_scalar("SELECT id FROM ttrss_users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    if user_exists.is_some() {
        let mut login = clean_str(extra.get("login").and_then(Value::as_str).unwrap_or(""));
        if id == 1 {
            login = "admin".into();
        }
        if login.is_empty() {
            return String::new();
        }
        let access_level = clean_int(extra.get("access_level")) as i32;
        let email = clean_str(extra.get("email").and_then(Value::as_str).unwrap_or(""));
        let otp_enabled = checkbox(extra, "otp_enabled");
        // `schema_version >= 143 && !otp_enabled` → force a fresh OTP secret.
        let otp_secret_clause = if otp_enabled {
            ""
        } else {
            ", otp_secret = NULL"
        };
        let _ = sqlx::query(AssertSqlSafe(format!(
            "UPDATE ttrss_users SET login = $1, access_level = $2, email = $3, \
             otp_enabled = $4{otp_secret_clause} WHERE id = $5"
        )))
        .bind(login.to_lowercase())
        .bind(access_level)
        .bind(email)
        .bind(otp_enabled)
        .bind(id)
        .execute(pool)
        .await;
    }
    if password.is_empty() {
        return String::new();
    }
    reset_password(pool, id, &password).await
}

/// `Pref_Users::remove` — delete the given users (never the current user or
/// admin id 1). Related rows cascade via their `owner_uid`/`feed_id` FKs.
pub async fn remove(state: &AppState, sess: &Sess, extra: &Map<String, Value>) {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    for id in param_int_array(extra, "ids") {
        if id as i64 != uid && id != 1 {
            let _ = sqlx::query("DELETE FROM ttrss_tags WHERE owner_uid = $1")
                .bind(id as i64)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM ttrss_feeds WHERE owner_uid = $1")
                .bind(id as i64)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM ttrss_users WHERE id = $1")
                .bind(id as i64)
                .execute(pool)
                .await;
        }
    }
}

/// `Pref_Users::add` — create a new user with a random password; returns the
/// result message.
pub async fn add(state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let login = clean_str(extra.get("login").and_then(Value::as_str).unwrap_or(""));
    if login.is_empty() {
        return String::new();
    }
    // `UserHelper::find_user_by_login`: `LOWER(login) = LOWER(?)`.
    if find_user_by_login(pool, &login).await.is_some() {
        return format!("User {login} already exists.");
    }
    let new_password = make_password(12);
    let salt = user::get_salt();
    let pwd_hash =
        user::hash_password(&new_password, &salt, user::ALGO_SSHA512).unwrap_or_default();
    let _ = sqlx::query(
        "INSERT INTO ttrss_users (login, pwd_hash, salt, access_level, created) \
         VALUES ($1, $2, $3, 0, NOW())",
    )
    .bind(login.to_lowercase())
    .bind(&pwd_hash)
    .bind(&salt)
    .execute(pool)
    .await;
    // Re-check (the PHP re-runs `find_user_by_login` to confirm the insert).
    if find_user_by_login(pool, &login).await.is_some() {
        format!("Added user {login} with password {new_password}")
    } else {
        format!("Could not create user {login}")
    }
}

/// `Pref_Users::resetPass` — reset a user's password to a random one.
pub async fn reset_pass(state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let id = clean_int(extra.get("id")) as i32;
    reset_password(pool, id, "").await
}
