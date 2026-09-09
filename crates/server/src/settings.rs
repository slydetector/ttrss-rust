//! `Pref_Prefs` backend read handlers (the settings/preferences UI in
//! `backend.php?op=Pref_Prefs`).

use serde_json::{json, Map, Value};
use sqlx::Row;
use ttrss_core::phpser::Value as PValue;

use crate::prefs;
use crate::session::Sess;
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
