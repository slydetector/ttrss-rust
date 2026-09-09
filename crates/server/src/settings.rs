//! `Pref_Prefs` backend read handlers (the settings/preferences UI in
//! `backend.php?op=Pref_Prefs`).

use minijinja::Environment;
use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::phpser::Value as PValue;

use crate::prefs;
use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util;
use crate::AppState;

/// `Pref_Prefs::getProfiles`: the "Default profile" entry plus the user's
/// stored profiles (ordered by title), each with `active` (the session's
/// profile) and `initialized` (has any `ttrss_user_prefs2` rows).
pub async fn get_profiles(state: &AppState, sess: &Sess) -> Value {
    let pool = state.db.pool();
    let data = sess.data();
    let uid = data.uid().unwrap_or(0);
    let active_profile = data.profile().unwrap_or(0);

    let mut rv: Vec<Value> = Vec::new();
    rv.push(json!({
        "title": "Default profile",
        "id": 0,
        "initialized": true,
        "active": active_profile == 0,
    }));

    let rows = sqlx::query(
        "SELECT id, title, owner_uid FROM ttrss_settings_profiles \
         WHERE owner_uid = $1 ORDER BY title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for row in &rows {
        let id: i32 = row.get(0);
        let title: String = row.get(1);
        let owner_uid: i32 = row.get(2);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ttrss_user_prefs2 WHERE profile = $1",
        )
        .bind(id as i64)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
        rv.push(json!({
            "id": id,
            "title": title,
            "owner_uid": owner_uid,
            "active": active_profile == id as i64,
            "initialized": count > 0,
        }));
    }

    Value::Array(rv)
}

/// `Pref_Prefs::activateprofile`: set the session's active profile to `id` if
/// it exists and is owned, otherwise clear it (fall back to the default).
pub async fn activate_profile(state: &AppState, sess: &Sess, extra: &Map<String, Value>) {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let id = util::clean_int(extra.get("id")) as i32;
    let found: Option<i32> = sqlx::query_scalar(
        "SELECT id FROM ttrss_settings_profiles WHERE owner_uid = $1 AND id = $2",
    )
    .bind(uid)
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    sess.with_mut(|s| match found {
        Some(pid) => s.set("profile", PValue::Int(pid as i64)),
        None => s.remove("profile"),
    });
}

/// `Pref_Prefs::addprofile`: create a profile with the given (cleaned) title,
/// unless an owned profile with that title already exists (idempotent).
pub async fn add_profile(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let title = util::clean_str(extra.get("title").and_then(Value::as_str).unwrap_or(""));
    if title.is_empty() {
        return Ok(());
    }
    let exists: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ttrss_settings_profiles WHERE owner_uid = $1 AND title = $2",
    )
    .bind(uid)
    .bind(&title)
    .fetch_one(pool)
    .await?;
    if exists == 0 {
        sqlx::query("INSERT INTO ttrss_settings_profiles (title, owner_uid) VALUES ($1, $2)")
            .bind(&title)
            .bind(uid)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// `Pref_Prefs::saveprofile`: rename an owned profile (`id` + cleaned `value`).
pub async fn save_profile(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let id = util::clean_int(extra.get("id")) as i32;
    let title = util::clean_str(extra.get("value").and_then(Value::as_str).unwrap_or(""));
    if !title.is_empty() && id != 0 {
        sqlx::query(
            "UPDATE ttrss_settings_profiles SET title = $1 WHERE owner_uid = $2 AND id = $3",
        )
        .bind(&title)
        .bind(uid)
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// `Pref_Prefs::remprofiles`: delete the selected owned profiles (never the
/// active one); their pref rows cascade-delete.
pub async fn remove_profiles(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let active_profile = sess.data().profile().unwrap_or(0);
    let ids = util::param_int_array(extra, "ids");
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "DELETE FROM ttrss_settings_profiles \
         WHERE owner_uid = $1 AND id = ANY($2) AND id <> $3",
    )
    .bind(uid)
    .bind(&ids)
    .bind(active_profile as i32)
    .execute(pool)
    .await?;
    Ok(())
}

/// `Pref_Prefs::cloneprofile`: create a new profile (`new_title`) and copy the
/// source profile's pref overrides into it. Cloning from the default profile
/// (0) copies only the non-blacklisted default prefs; cloning from a named
/// profile copies all of its prefs.
pub async fn clone_profile(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<(), sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);

    let old_raw = extra
        .get("old_profile")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !util::ctype_digit(old_raw) {
        return Ok(());
    }
    let old_profile_id: i32 = old_raw.parse().unwrap_or(0);

    let new_title = util::clean_str(extra.get("new_title").and_then(Value::as_str).unwrap_or(""));
    if new_title.is_empty() {
        return Ok(());
    }

    let new_profile_id: i32 = sqlx::query_scalar(
        "INSERT INTO ttrss_settings_profiles (title, owner_uid) VALUES ($1, $2) RETURNING id",
    )
    .bind(&new_title)
    .bind(uid)
    .fetch_one(pool)
    .await?;

    let mut q = sqlx::QueryBuilder::new(
        "INSERT INTO ttrss_user_prefs2 (owner_uid, pref_name, profile, value) \
         SELECT ",
    );
    q.push_bind(uid);
    q.push(", pref_name, ");
    q.push_bind(new_profile_id);
    q.push(", value FROM ttrss_user_prefs2 WHERE owner_uid = ");
    q.push_bind(uid);
    if old_profile_id == 0 {
        q.push(" AND profile IS NULL AND pref_name NOT IN (");
        for (i, name) in prefs::PROFILE_BLACKLIST.iter().enumerate() {
            if i > 0 {
                q.push(", ");
            }
            q.push_bind(name);
        }
        q.push(")");
    } else {
        q.push(" AND profile = ");
        q.push_bind(old_profile_id);
    }
    q.build().execute(pool).await?;
    Ok(())
}

/// `Pref_Prefs::previewDigest`: preview of the headline digest (days=1,
/// limit=16), rendered via `digest::prepare_headlines_digest`.
pub async fn preview_digest(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0) as i32;
    crate::digest::prepare_headlines_digest(state, uid, 1, 16).await
}

/// `Pref_Prefs::appPasswordList` (the private renderer shared by
/// `generateAppPassword` and `deleteAppPasswords`): the user's app-password
/// table (ordered by title) rendered as the Dojo UI the SPA injects into
/// `app_passwords_holder`. Titles are html-escaped; dates go through
/// `make_local_datetime` (a `NULL` `last_used` renders as "Never").
pub async fn app_password_list(state: &AppState, sess: &Sess) -> Result<String, sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0) as i32;
    let ctx = TzCtx::for_session(state, &sess.data()).await;
    let rows = sqlx::query(
        "SELECT id, title, created::text AS created, last_used::text AS last_used \
         FROM ttrss_app_passwords WHERE owner_uid = $1 ORDER BY title",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;

    let mut passwords: Vec<Value> = Vec::new();
    for row in &rows {
        let id: i32 = row.get(0);
        let title: String = row.get(1);
        let created: String = row.get(2);
        let last_used: Option<String> = row.get(3);
        passwords.push(json!({
            "id": id,
            "title": util::htmlspecialchars(&title),
            "created": make_local_datetime(Some(&created), false, &ctx),
            "last_used": make_local_datetime(last_used.as_deref(), false, &ctx),
        }));
    }

    let mut map: Map<String, Value> = Map::new();
    map.insert("passwords".into(), Value::Array(passwords));
    app_password_list_env()
        .get_template("app_password_list")
        .expect("app_password_list template")
        .render(map)
        .map_err(sqlx::Error::config)
}

/// `Pref_Prefs::generateAppPassword`: insert a new app-password row (a random
/// 16-char password, hashed SSHA-512 with a fresh salt) and return the warning
/// div (showing the plaintext password) plus the refreshed list.
pub async fn generate_app_password(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<String, sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0) as i32;
    let title = util::clean_str(extra.get("title").and_then(Value::as_str).unwrap_or(""));
    let new_password = util::make_password(16);
    let new_salt = ttrss_core::user::get_salt();
    let new_password_hash = ttrss_core::user::hash_password(
        &new_password,
        &new_salt,
        ttrss_core::user::ALGO_SSHA512,
    )
    .expect("hash_password");

    sqlx::query(
        "INSERT INTO ttrss_app_passwords (title, owner_uid, pwd_hash, service, created) \
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(&title)
    .bind(uid)
    .bind(format!("{new_password_hash}:{new_salt}"))
    .bind("_api") // Auth_Base::AUTH_SERVICE_API
    .execute(pool)
    .await?;

    let list = app_password_list(state, sess).await?;
    let msg = format!(
        "<div class=\"alert\" id=\"\">Generated password <strong>{new_password}</strong> \
         for {title}. Please remember it for future reference.</div>"
    );
    Ok(format!("{msg}{list}"))
}

/// `Pref_Prefs::deleteAppPasswords`: delete the selected owned app-password
/// rows, then return the refreshed list.
pub async fn delete_app_passwords(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> Result<String, sqlx::Error> {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0) as i32;
    let ids = util::param_int_array(extra, "ids");
    if !ids.is_empty() {
        sqlx::query("DELETE FROM ttrss_app_passwords WHERE owner_uid = $1 AND id = ANY($2)")
            .bind(uid)
            .bind(&ids)
            .execute(pool)
            .await?;
    }
    app_password_list(state, sess).await
}

/// The minijinja environment holding `app_password_list` (parsed once).
fn app_password_list_env() -> &'static Environment<'static> {
    static ENV: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_template(
            "app_password_list",
            include_str!("../templates/app_password_list.html.jinja"),
        )
        .expect("valid app_password_list template");
        env
    })
}

/// `Pref_Prefs::saveconfig`: persist every valid pref in the submitted form
/// (the prefs form posts `op=Pref_Prefs&method=saveconfig`), applying the
/// per-pref quirks — boolean defaults (`boolean_prefs`), the digest-time reset
/// of `last_digest_sent`, the theme/language reload flag, and blacklisted-tag
/// normalization. Returns the message the SPA shows (`PREFS_NEED_RELOAD` or
/// the "saved" text); the SPA reloads on the former and toasts the latter.
pub async fn saveconfig(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);

    // `$_POST[boolean_prefs]`: comma-separated boolean pref names, each
    // defaulting to "false" when absent from the form (`??=`).
    let boolean_names: Vec<String> = util::clean_str(
        extra.get("boolean_prefs").and_then(Value::as_str).unwrap_or(""),
    )
    .split(',')
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect();

    // The effective POST map (the form fields + the boolean defaults).
    let mut post: Map<String, Value> = extra.clone();
    for name in &boolean_names {
        post.entry(name.clone())
            .or_insert_with(|| Value::String("false".into()));
    }

    // Current values (for the reload / digest-time comparisons). `load_rows`
    // loads both the named-profile and default-profile overrides so the
    // profile-blacklisted comparisons resolve correctly.
    let rows = prefs::load_rows(&state.db, uid, profile).await;

    let mut need_reload = false;
    for (name, value) in &post {
        let mut value_str = value.as_str().unwrap_or("").to_string();

        match name.as_str() {
            prefs::DIGEST_PREFERRED_TIME => {
                if prefs::resolve(prefs::DIGEST_PREFERRED_TIME, &rows, profile).as_str()
                    != value_str.as_str()
                {
                    let _ = sqlx::query("UPDATE ttrss_users SET last_digest_sent = NULL WHERE id = $1")
                        .bind(uid)
                        .execute(pool)
                        .await;
                }
            }
            prefs::USER_CSS_THEME | prefs::USER_LANGUAGE => {
                if !need_reload && prefs::resolve(name, &rows, profile).as_str() != value_str.as_str() {
                    need_reload = true;
                }
            }
            prefs::BLACKLISTED_TAGS => {
                let parts: Vec<String> = value_str.split(',').map(|s| s.to_string()).collect();
                value_str = util::normalize_categories(&parts).join(", ");
            }
            _ => {}
        }

        if prefs::default_for(name).is_some() {
            let _ = prefs::set(&state.db, name, &value_str, uid, profile).await;
        }
    }

    if need_reload {
        "PREFS_NEED_RELOAD".to_string()
    } else {
        "The configuration was saved.".to_string()
    }
}

/// `Pref_Prefs::resetconfig`: discard every override the user set for the
/// active profile (`Prefs::reset`) and tell the SPA to reload.
pub async fn resetconfig(state: &AppState, sess: &Sess) -> String {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    prefs::reset(&state.db, uid, profile).await;
    "PREFS_NEED_RELOAD".to_string()
}


