//! `Pref_Prefs` backend read handlers (the settings/preferences UI in
//! `backend.php?op=Pref_Prefs`).

use serde_json::{json, Value};
use sqlx::Row;

use crate::session::Sess;
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
