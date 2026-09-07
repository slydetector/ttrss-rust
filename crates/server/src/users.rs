//! `Pref_Users` backend read handlers.
//!
//! The `index` page is reproduced from the frozen PHP template
//! (`classes/Pref_Users.php`) with its exact whitespace: the harness keeps
//! whitespace *inside* a tag / before a text node (only whitespace strictly
//! between two tags is canonicalized away), so the button/menu labels carry
//! their template tabs. PHP's rule that the newline immediately after `?>`
//! is consumed is baked into the templates below.

use sqlx::Row;

use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util::htmlspecialchars;
use crate::AppState;

/// `UserHelper::ACCESS_LEVEL_*` → display name (the `$access_level_names` map
/// built in `backend.php`).
fn access_level_name(level: i32) -> &'static str {
    match level {
        -2 => "Disabled",
        -1 => "Read Only",
        0 => "User",
        5 => "Power User",
        10 => "Administrator",
        _ => "",
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
							Create user						</button>

						<button dojoType='dijit.form.Button' onclick='Users.removeSelected()'>
							Remove						</button>

						<button dojoType='dijit.form.Button' onclick='Users.resetSelected()'>
							Reset password						</button>

						
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

const INDEX_ROW: &str = r#"<tr data-row-id='@ID@' onclick='Users.edit(@ID@)' title="Click to edit">
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
							</div>"#;

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

    let mut out = String::new();
    out.push_str(INDEX_PRE);
    out.push_str(&htmlspecialchars(&user_search));
    out.push_str(INDEX_POST);

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
                .replace("@LEVEL@", access_level_name(access_level))
                .replace("@NUMFEEDS@", &num_feeds.to_string())
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

    out.push_str(INDEX_FOOTER);
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
        return crate::util::format_error("User not found", "");
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
        let icon_url = if state.config.feed_icon_dir.join(fid.to_string()).exists() {
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
