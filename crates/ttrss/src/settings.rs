//! `Pref_Prefs` backend read handlers (the settings/preferences UI in
//! `backend.php?op=Pref_Prefs`).

use crate::core::phpser::Value as PValue;
use minijinja::Environment;
use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::auth;
use crate::controls;
use crate::otp;
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
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ttrss_user_prefs2 WHERE profile = $1")
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
    let new_salt = crate::core::user::get_salt();
    let new_password_hash =
        crate::core::user::hash_password(&new_password, &new_salt, crate::core::user::ALGO_SSHA512)
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
        extra
            .get("boolean_prefs")
            .and_then(Value::as_str)
            .unwrap_or(""),
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
                    let _ =
                        sqlx::query("UPDATE ttrss_users SET last_digest_sent = NULL WHERE id = $1")
                            .bind(uid)
                            .execute(pool)
                            .await;
                }
            }
            prefs::USER_CSS_THEME | prefs::USER_LANGUAGE => {
                if !need_reload
                    && prefs::resolve(name, &rows, profile).as_str() != value_str.as_str()
                {
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
            let _ = prefs::set(&state.db, name, &value_str, uid, profile, true).await;
        }
    }

    // Invalidate the per-process prefs cache so the next read reflects the
    // saved values (the PHP's `Prefs::set` updates the in-memory cache).
    crate::prefs::invalidate(&state.prefs_cache, uid, profile);

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
    crate::prefs::invalidate(&state.prefs_cache, uid, profile);
    "PREFS_NEED_RELOAD".to_string()
}

/// `Pref_Prefs::changePersonalData`: update the user's full name + e-mail.
///
/// NOTE(port): the "e-mail address change notification" e-mail is omitted (no
/// SMTP in the Rust port).
pub async fn change_personal_data(
    state: &AppState,
    sess: &Sess,
    extra: &Map<String, Value>,
) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let exists: Option<String> = sqlx::query_scalar("SELECT login FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    if exists.is_some() {
        let new_full = util::clean_str(&str_extra(extra, "full_name"));
        let new_email = util::clean_str(&str_extra(extra, "email"));
        let _ = sqlx::query("UPDATE ttrss_users SET full_name = $1, email = $2 WHERE id = $3")
            .bind(&new_full)
            .bind(&new_email)
            .bind(uid)
            .execute(pool)
            .await;
    }
    "Your personal data has been saved.".to_string()
}

/// `Pref_Prefs::changepassword`: validate the new password (cleaned, must
/// differ from the old, non-blank, confirm matches) then change it via the
/// auth module. Assumes `auth_internal` (the only module loaded out of the
/// gate).
pub async fn changepassword(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0) as i32;

    // `Config::FORBID_PASSWORD_CHANGES`: refuse password changes entirely.
    if state.config.forbid_password_changes {
        return format!("ERROR: {}", controls::format_error("Access forbidden."));
    }

    let old_pw = util::clean_str(&str_extra(extra, "old_password"));
    let new_unclean = str_extra(extra, "new_password");
    let new_pw = util::clean_str(&new_unclean);
    let con_pw = util::clean_str(&str_extra(extra, "confirm_password"));

    let err = |m: &str| format!("ERROR: {}", controls::format_error(m));
    // `clean()` altered the raw password (tags / surrounding whitespace).
    if new_unclean != new_pw {
        return err("New password contains disallowed characters.");
    }
    if old_pw == new_pw {
        return err("New password must be different from the old one.");
    }
    if old_pw.is_empty() {
        return err("Old password cannot be blank.");
    }
    if new_pw.is_empty() {
        return err("New password cannot be blank.");
    }
    if new_pw != con_pw {
        return err("Entered passwords do not match.");
    }

    match auth::change_password(pool, uid, &old_pw, &new_pw).await {
        Some(new_hash) => {
            // Keep the session valid across the change (`validate_session`
            // compares the stored hash to `$_SESSION["pwd_hash"]`).
            sess.with_mut(|s| s.set("pwd_hash", PValue::Bytes(new_hash.into_bytes())));
            controls::format_notice("Password has been changed.")
        }
        None => controls::format_notice("ERROR: Old password is incorrect."),
    }
}

/// `Pref_Prefs::otpenable`: verify the password + one-time code, then enable
/// OTP. Assumes `auth_internal`.
pub async fn otp_enable(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let password = util::clean_str(&str_extra(extra, "password"));
    let otp_code = util::clean_str(&str_extra(extra, "otp"));
    if auth::check_password(pool, uid as i32, &password).await {
        match otp::enable_otp(pool, uid, &otp_code, util::now_unix() as u64).await {
            Ok(true) => "OK".to_string(),
            _ => "ERROR:Incorrect one time password".to_string(),
        }
    } else {
        "ERROR:Incorrect password".to_string()
    }
}

/// `Pref_Prefs::otpdisable`: verify the password, then disable OTP. Assumes
/// `auth_internal`.
pub async fn otp_disable(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let password = util::clean_str(&str_extra(extra, "password"));
    if auth::check_password(pool, uid as i32, &password).await {
        let _ = otp::disable_otp(pool, uid).await;
        "OK".to_string()
    } else {
        "ERROR: Incorrect password".to_string()
    }
}

/// `Pref_Prefs::customizeCSS`: the user's custom stylesheet for the active
/// profile, with `<br/>` converted to newlines.
pub async fn customize_css(state: &AppState, sess: &Sess) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let value = prefs::resolve(prefs::USER_STYLESHEET, &rows, profile)
        .as_str()
        .to_string();
    json!({ "value": value.replace("<br/>", "\n") })
}

/// A string form field, or `""` when absent (PHP's `$_POST[name]` reads).
fn str_extra(extra: &Map<String, Value>, key: &str) -> String {
    extra
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// A raw request value as a string: strings as-is, other JSON values via their
/// JSON representation (PHP's `$_REQUEST[name]` is always a string).
fn raw_extra(extra: &Map<String, Value>, key: &str) -> String {
    match extra.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `RPC::setpref` — the generic frontend preference setter. `strip_tags` is off
/// only for `USER_STYLESHEET` (its CSS value may contain `<`/`>`).
pub async fn set_pref(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let key = util::clean_str(&str_extra(extra, "key"));
    let value = raw_extra(extra, "value");
    let _ = prefs::set(
        &state.db,
        &key,
        &value,
        uid,
        profile,
        key != prefs::USER_STYLESHEET,
    )
    .await;
    json!({ "param": key, "value": value })
}

/// `RPC::togglepref` — flip a (bool) pref and echo back the new value.
pub async fn toggle_pref(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let key = util::clean_str(&str_extra(extra, "key"));
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    let negated = !prefs::resolve(&key, &rows, profile).as_bool();
    let _ = prefs::set(
        &state.db,
        &key,
        if negated { "1" } else { "" },
        uid,
        profile,
        true,
    )
    .await;
    let rows = prefs::load_rows(&state.db, uid, profile).await;
    json!({ "param": key, "value": prefs::resolve(&key, &rows, profile).to_json() })
}

/// `RPC::setWidescreen` — set the `WIDESCREEN_MODE` pref.
pub async fn set_widescreen(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> Value {
    let uid = sess.data().uid().unwrap_or(0);
    let profile = sess.data().profile().map(|p| p as i32);
    let wide = util::clean_int(extra.get("wide"));
    let _ = prefs::set(
        &state.db,
        prefs::WIDESCREEN_MODE,
        &wide.to_string(),
        uid,
        profile,
        true,
    )
    .await;
    json!({ "wide": wide })
}

/// `RPC::log` — the client-JS error reporter: record a client-side error and
/// ack it. Echoes nothing (JSON `null`) when `msg` is empty.
pub async fn client_log(_state: &AppState, _sess: &Sess, extra: &Map<String, Value>) -> Value {
    let msg = util::clean_str(&str_extra(extra, "msg"));
    if msg.is_empty() {
        return Value::Null;
    }
    let file = util::basename(&util::clean_str(&str_extra(extra, "file")));
    let line = util::clean_int(extra.get("line"));
    let context = util::clean_str(&str_extra(extra, "context"));
    tracing::error!(
        target: "client-js",
        error = %msg,
        file = %file,
        line = line,
        context = %context,
        "client JS error"
    );
    json!({ "message": "HOST_ERROR_LOGGED" })
}
