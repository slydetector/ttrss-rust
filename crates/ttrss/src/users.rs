//! `Pref_Users` backend read handlers.
//!
//! The `index` page is reproduced from the frozen PHP template
//! (`classes/Pref_Users.php`) with its exact whitespace: the harness keeps
//! whitespace *inside* a tag / before a text node (only whitespace strictly
//! between two tags is canonicalized away), so the button/menu labels carry
//! their template tabs. PHP's rule that the newline immediately after `?>`
//! is consumed is baked into the templates below.

use sqlx::AssertSqlSafe;
use sqlx::Row;

use crate::AppState;
use crate::core::user;
use crate::i18n::{__, t_sprintf};
use crate::session::Sess;
use crate::time::{TzCtx, make_local_datetime};
use crate::util::{
    checkbox, clean_int, clean_str, htmlspecialchars, make_password, param_int_array,
};
use serde_json::{Map, Value, json};

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
fn index_post_section() -> String {
    let (login, access, feeds, registered, last_login) = (
        __("Login"),
        __("Access level"),
        __("Subscribed feeds"),
        __("Registered"),
        __("Last login"),
    );
    format!(
        r#"[__pref_users_toolbar__]
						
					</div>
				</div>
				<div style='padding : 0px' dojoType='dijit.layout.ContentPane' region='center'>

					<table width='100%' class='users-list' id='users-list'>

						<tr>
							<th></th>
							<th><a href='#' onclick="Users.reload('login')">{login}</a></th>
							<th><a href='#' onclick="Users.reload('access_level')">{access}</a></th>
							<th><a href='#' onclick="Users.reload('num_feeds')">{feeds}</a></th>
							<th><a href='#' onclick="Users.reload('created')">{registered}</a></th>
							<th><a href='#' onclick="Users.reload('last_login')">{last_login}</a></th>
						</tr>"#
    )
}

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

    // `$user_search = clean($_REQUEST["search"] ?? "")` + the session
    // persistence: the reference stores the cleaned term when the `search`
    // key is *present* (even empty — that clears the filter) and re-reads
    // `$_SESSION["prefs_user_search"]` when it is absent.
    let user_search = match extra.get("search").and_then(|v| v.as_str()) {
        Some(raw) => {
            let cleaned = clean_str(raw);
            sess.with_mut(|d| {
                d.set(
                    "prefs_user_search",
                    crate::core::phpser::Value::Bytes(cleaned.as_bytes().to_vec()),
                );
            });
            cleaned
        }
        None => sess
            .data()
            .get_str("prefs_user_search")
            .unwrap_or("")
            .to_string(),
    };

    // `$sort = clean($_REQUEST["sort"] ?? "")` — the column headers sort
    // via `Users.reload(col)`; the reference whitelists the five column
    // names (its list repeats "created") and sorts everything but `login`
    // DESC. The whitelist makes the interpolated clause safe.
    let sort = extra
        .get("sort")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sort = clean_str(&sort);
    let sort = if sort.is_empty() || sort == "undefined" {
        "login".to_string()
    } else if matches!(
        sort.as_str(),
        "login" | "access_level" | "created" | "num_feeds" | "last_login"
    ) {
        sort
    } else {
        "login".to_string()
    };
    let order_by = if sort == "login" {
        "u.login".to_string()
    } else {
        format!("{sort} DESC")
    };

    let like = format!("%{}%", user_search);
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT u.id, u.login, u.access_level, COUNT(f.id) AS num_feeds, \
               to_char(u.created, 'YYYY-MM-DD HH24:MI:SS') AS created, \
               to_char(u.last_login, 'YYYY-MM-DD HH24:MI:SS') AS last_login \
         FROM ttrss_users u \
         LEFT JOIN ttrss_feeds f ON f.owner_uid = u.id \
         WHERE u.login LIKE $1 \
         GROUP BY u.id \
         ORDER BY {order_by}"
    )))
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
    out.push_str(&index_post_section().replace("[__pref_users_toolbar__]", &toolbar));

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
    let self_url = sess.self_url();
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
    let (registered_l, last_logged_l, feeds_l, stored_l) = (
        __("Registered"),
        __("Last logged in"),
        __("Subscribed feeds"),
        __("Stored articles"),
    );
    out.push_str(&format!(
        "<fieldset>\n<label>{registered_l}:</label>\n{0}\n</fieldset>\n\
         <fieldset>\n<label>{last_logged_l}:</label>\n{1}\n</fieldset>\n\
         <fieldset>\n<label>{feeds_l}:</label>\n{2}\n</fieldset>\n\
         <fieldset>\n<label>{stored_l}:</label>\n{3}\n</fieldset>\n\
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
            format!("{self_url}/public.php?op=feed_icon&id={fid}")
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
    // `T_sprintf("Changed password of user %s to %s", "<strong>…</strong>", …)`
    // — the `<strong>` tags are part of the args, as in the reference.
    let strong_login = format!("<strong>{login}</strong>");
    let strong_pwd = format!("<strong>{tmp_pwd}</strong>");
    t_sprintf(
        "Changed password of user %s to %s",
        &[&strong_login, &strong_pwd],
    )
}

// ---------------------------------------------------------------------------
// `update.php` verbs (docs/19 P0) — the `UserHelper` primitives plus the
// verb wrappers the CLI / first-boot bootstrap calls directly.
// ---------------------------------------------------------------------------

/// `UserHelper::map_access_level($level) != ACCESS_LEVEL_KEEP_CURRENT` — a
/// real, stored access level.
pub fn is_valid_access_level(level: i32) -> bool {
    matches!(
        level,
        user::ACCESS_LEVEL_DISABLED
            | user::ACCESS_LEVEL_READONLY
            | user::ACCESS_LEVEL_USER
            | user::ACCESS_LEVEL_POWERUSER
            | user::ACCESS_LEVEL_ADMIN
    )
}

/// `UserHelper::user_add` — add `$login`/`$password` at `$access_level`.
/// False when the login is blank, the password empty, the user already
/// exists, or the level is not a valid stored level. New passwords hash
/// SSHA-512 with a fresh salt (the `hash_password` default algo).
pub async fn user_add(
    pool: &sqlx::PgPool,
    login: &str,
    password: &str,
    access_level: i32,
) -> sqlx::Result<bool> {
    let login = clean_str(login);
    if login.is_empty() || password.is_empty() || !is_valid_access_level(access_level) {
        return Ok(false);
    }
    if find_user_by_login(pool, &login).await.is_some() {
        return Ok(false);
    }
    let salt = user::get_salt();
    let pwd_hash = user::hash_password(password, &salt, user::ALGO_SSHA512).unwrap_or_default();
    sqlx::query(
        "INSERT INTO ttrss_users (salt, login, pwd_hash, access_level, created)
         VALUES ($1, $2, $3, $4, NOW())",
    )
    .bind(&salt)
    .bind(login.to_lowercase())
    .bind(&pwd_hash)
    .bind(access_level)
    .execute(pool)
    .await?;
    Ok(true)
}

/// `UserHelper::user_modify` — set `$uid`'s password (when `$new_password`
/// is non-empty: fresh salt + SSHA-512) and/or `$access_level` (when not
/// `KEEP_CURRENT`). False when the user does not exist.
pub async fn user_modify(
    pool: &sqlx::PgPool,
    uid: i32,
    new_password: Option<&str>,
    access_level: i32,
) -> sqlx::Result<bool> {
    let row: Option<(String, String, i32)> =
        sqlx::query_as("SELECT pwd_hash, salt, access_level FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(pool)
            .await?;
    let Some((cur_hash, cur_salt, cur_level)) = row else {
        return Ok(false);
    };
    let (pwd_hash, salt) = match new_password {
        Some(p) if !p.is_empty() => {
            let salt = user::get_salt();
            (
                user::hash_password(p, &salt, user::ALGO_SSHA512).unwrap_or_default(),
                salt,
            )
        }
        _ => (cur_hash, cur_salt),
    };
    let level = if access_level == user::ACCESS_LEVEL_KEEP_CURRENT {
        cur_level
    } else {
        access_level
    };
    sqlx::query("UPDATE ttrss_users SET pwd_hash = $1, salt = $2, access_level = $3 WHERE id = $4")
        .bind(&pwd_hash)
        .bind(&salt)
        .bind(level)
        .bind(uid)
        .execute(pool)
        .await?;
    Ok(true)
}

/// `UserHelper::user_has_password` — verify `$password` against `$uid`'s
/// stored hash (false when the user does not exist). PHP delegates to
/// `Auth_Internal::check_password`, which also re-hashes an out-of-date algo
/// to `HASH_ALGOS[0]` on a match — the port's `auth::check_password` is the
/// same delegation target.
pub async fn user_has_password(
    pool: &sqlx::PgPool,
    uid: i32,
    password: &str,
) -> sqlx::Result<bool> {
    Ok(crate::auth::check_password(pool, uid, password).await)
}

/// `update.php` `user-exists` — does `$login` exist (case-insensitive)?
pub async fn user_exists(pool: &sqlx::PgPool, login: &str) -> sqlx::Result<bool> {
    Ok(find_user_by_login(pool, login).await.is_some())
}

/// `update.php` `user-check-password` — is `$password` `$login`'s?
pub async fn user_check_password(
    pool: &sqlx::PgPool,
    login: &str,
    password: &str,
) -> sqlx::Result<bool> {
    match find_user_by_login(pool, login).await {
        Some(uid) => user_has_password(pool, uid, password).await,
        None => Ok(false),
    }
}

/// `update.php` `user-set-password` — set `$login`'s password (false when
/// the user does not exist).
pub async fn user_set_password(
    pool: &sqlx::PgPool,
    login: &str,
    password: &str,
) -> sqlx::Result<bool> {
    match find_user_by_login(pool, login).await {
        Some(uid) => user_modify(pool, uid, Some(password), user::ACCESS_LEVEL_KEEP_CURRENT).await,
        None => Ok(false),
    }
}

/// `update.php` `user-set-access-level` — set `$login`'s level (false when
/// the user does not exist or the level is not a valid stored level).
pub async fn user_set_access_level(
    pool: &sqlx::PgPool,
    login: &str,
    access_level: i32,
) -> sqlx::Result<bool> {
    if !is_valid_access_level(access_level) {
        return Ok(false);
    }
    match find_user_by_login(pool, login).await {
        Some(uid) => user_modify(pool, uid, None, access_level).await,
        None => Ok(false),
    }
}

/// `update.php` `user-add` — `user_add` (the env-driven bootstrap parses the
/// `USER:PASS:LEVEL` env var; an empty level defaults to 0 *there*).
pub async fn user_add_cmd(
    pool: &sqlx::PgPool,
    login: &str,
    password: &str,
    access_level: i32,
) -> sqlx::Result<bool> {
    user_add(pool, login, password, access_level).await
}

/// `update.php` `user-enable-api` — set `$login`'s `ENABLE_API_ACCESS` pref
/// (false when the user does not exist).
pub async fn user_enable_api(db: &crate::core::db::Db, login: &str, enable: bool) -> bool {
    let Some(uid) = find_user_by_login(db.pool(), login).await else {
        return false;
    };
    crate::prefs::set(
        db,
        crate::prefs::ENABLE_API_ACCESS,
        if enable { "true" } else { "false" },
        uid as i64,
        None,
        false,
    )
    .await
}

/// `update.php` `user-remove` — delete `$login`'s user row (false when the
/// user does not exist).
pub async fn user_remove(pool: &sqlx::PgPool, login: &str) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM ttrss_users WHERE LOWER(login) = LOWER($1)")
        .bind(login)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
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
        if id != uid && id != 1 {
            let _ = sqlx::query("DELETE FROM ttrss_tags WHERE owner_uid = $1")
                .bind(id)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM ttrss_feeds WHERE owner_uid = $1")
                .bind(id)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM ttrss_users WHERE id = $1")
                .bind(id)
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
        return t_sprintf("User %s already exists.", &[&login]);
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
        t_sprintf("Added user %s with password %s", &[&login, &new_password])
    } else {
        t_sprintf("Could not create user %s", &[&login])
    }
}

/// `Pref_Users::resetPass` — reset a user's password to a random one.
pub async fn reset_pass(state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let id = clean_int(extra.get("id")) as i32;
    reset_password(pool, id, "").await
}
