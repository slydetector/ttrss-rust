//! Plugin storage — the port of `tt-rss/classes/PluginHost.php`'s data
//! layer (`load_data` / `save_data` / `set` / `set_array` / `get` /
//! `get_array` / `get_all` / `clear_data` / `profile_set` / `profile_get`)
//! onto [`crate::core::phpser`].
//!
//! One row per (owner_uid, plugin **class name**): `content` holds the
//! plugin's whole data map, PHP-serialized. PHP writes the `serialize()`
//! output raw into the `text` column under a UTF-8 client encoding, so the
//! stored form is always valid UTF-8 — this module asserts the same on
//! write (a non-UTF-8 serialized form is exactly what PHP itself cannot
//! store there).
//!
//! Every method is scoped to a [`HostCtx`]: PHP's per-host owner/profile
//! state (`$this->owner_uid`, `$_SESSION["profile"]`) lives there.
//!
//! Faithful quirks (PHP is the spec):
//!
//! - the owner gate is PHP's *truthiness* check — `None` **and** `0` make
//!   `save`/`clear` no-ops (`if ($this->owner_uid)`);
//! - `set`/`set_array`/`profile_set` do **not** load existing data first —
//!   a write on a fresh context (no prior `get` for that plugin) replaces
//!   the whole row; only the `get` side loads lazily;
//! - `get_all` returns the in-memory map without touching the DB
//!   (`$this->storage[$idx] ?? []` — no `load_data()` call);
//! - PHP's `load_data()` additionally gates on
//!   `Config::get_schema_version() > 100` (the table arrived in migration
//!   101); the port's schema is always ≥ 101, so that check is a constant.
//!
//! `Profile` scoping mirrors `profile_set`/`profile_get`: a truthy profile
//! id nests the value under an **int-keyed** slot (`storage[class][profile]
//! [name]`); a falsy one delegates to the plain `set`/`get`.


use sqlx::Error as SqlxError;
use sqlx::Row;

use super::host::HostCtx;
use super::plugin::Plugin;
use crate::core::phpser::{self, Key, Value};

/// Set an assoc-array key on a map value (the map is created if the slot
/// holds a non-map — unreachable in practice: PHP initializes with `??= []`).
fn map_put(v: &mut Value, key: &str, val: Value) {
    if !matches!(v, Value::Array(_)) {
        *v = Value::Array(Vec::new());
    }
    if let Value::Array(items) = v {
        if let Some(slot) = items
            .iter_mut()
            .find(|(k, _)| matches!(k, Key::Bytes(b) if b == key.as_bytes()))
        {
            slot.1 = val;
        } else {
            items.push((Key::Bytes(key.into()), val));
        }
    }
}

/// `map_put` into the `profile`-keyed sub-map (created as needed).
fn profile_map_put(v: &mut Value, profile: i64, key: &str, val: Value) {
    if !matches!(v, Value::Array(_)) {
        *v = Value::Array(Vec::new());
    }
    let Value::Array(items) = v else {
        return;
    };
    let slot = items
        .iter_mut()
        .find(|(k, _)| matches!(k, Key::Int(p) if *p == profile))
        .map(|(_, v)| v);
    match slot {
        Some(slot) => {
            if !matches!(slot, Value::Array(_)) {
                *slot = Value::Array(Vec::new());
            }
            if let Value::Array(inner) = slot {
                if let Some(e) = inner
                    .iter_mut()
                    .find(|(k, _)| matches!(k, Key::Bytes(b) if b == key.as_bytes()))
                {
                    e.1 = val;
                } else {
                    inner.push((Key::Bytes(key.into()), val));
                }
            }
        }
        None => {
            items.push((Key::Int(profile), Value::Array(vec![(Key::Bytes(key.into()), val)])));
        }
    }
}

/// `map_get` into the `profile`-keyed sub-map.
fn profile_map_get<'v>(v: &'v Value, profile: i64, key: &str) -> Option<&'v Value> {
    let Value::Array(items) = v else {
        return None;
    };
    let slot: &'v Value = items
        .iter()
        .find(|(k, _)| matches!(k, Key::Int(p) if *p == profile))
        .map(|(_, v)| v)?;
    let Value::Array(inner) = slot else {
        return None;
    };
    inner
        .iter()
        .find(|(k, _)| matches!(k, Key::Bytes(b) if b == key.as_bytes()))
        .map(|(_, v)| v)
}

impl<'a> HostCtx<'a> {
    // -- the lazy owner data map (PHP's `load_data` / `save_data`) ---------

    /// `PluginHost::load_data()` — load the owner's rows once, into
    /// `storage` (plugin class name → data map).
    async fn storage_load(&mut self) -> Result<(), SqlxError> {
        if !self.data_loaded {
            if let Some(uid) = self.owner_uid.filter(|u| *u != 0) {
                let rows = sqlx::query("SELECT name, content FROM ttrss_plugin_storage \
                     WHERE owner_uid = $1")
                    .bind(uid)
                    .fetch_all(self.pool)
                    .await?;
                for row in rows {
                    let name: String = row.try_get("name").unwrap();
                    let content: String = row.try_get("content").unwrap();
                    let v = phpser::deserialize(content.as_bytes())
                        .map_err(|e| {
                            SqlxError::Protocol(format!("plugin storage: {e}"))
                        })?;
                    self.storage.insert(name, v);
                }
            }
            self.data_loaded = true;
        }
        Ok(())
    }

    /// `PluginHost::save_data($plugin)` — upsert the owner's row for the
    /// plugin class name.
    async fn storage_save(&mut self, class: &str) -> Result<(), SqlxError> {
        let Some(uid) = self.owner_uid.filter(|u| *u != 0) else {
            return Ok(());
        };
        self.storage
            .entry(class.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let content = phpser::serialize(&self.storage[class].clone());
        // PHP stores the raw `serialize()` bytes in a `text` column under a
        // UTF-8 client encoding — the form is always valid UTF-8 (see the
        // module docs).
        let content = std::str::from_utf8(&content).map_err(|e| {
            SqlxError::Protocol(format!("plugin storage: non-UTF-8 form: {e}"))
        })?;

        let mut tx = self.pool.begin().await?;
        let exists: Option<i32> =
            sqlx::query_scalar("SELECT id FROM ttrss_plugin_storage WHERE owner_uid = $1 \
                                AND name = $2")
                .bind(uid)
                .bind(class)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_some() {
            sqlx::query("UPDATE ttrss_plugin_storage SET content = $1 \
                         WHERE owner_uid = $2 AND name = $3")
                .bind(content)
                .bind(uid)
                .bind(class)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("INSERT INTO ttrss_plugin_storage (name, owner_uid, content) \
                         VALUES ($1, $2, $3)")
                .bind(class)
                .bind(uid)
                .bind(content)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // -- the data API (PHP's `set` / `get` / `profile_*` / `clear_data`) ---

    /// `PluginHost::set($sender, $name, $value)`.
    pub async fn storage_set(
        &mut self,
        plugin: &dyn Plugin,
        name: &str,
        value: Value,
    ) -> Result<(), SqlxError> {
        let idx = plugin.class_name();
        self.storage
            .entry(idx.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        map_put(self.storage.get_mut(idx).unwrap(), name, value);
        self.storage_save(idx).await
    }

    /// `PluginHost::set_array($sender, $params)`.
    pub async fn storage_set_array(
        &mut self,
        plugin: &dyn Plugin,
        params: &[(String, Value)],
    ) -> Result<(), SqlxError> {
        let idx = plugin.class_name();
        self.storage
            .entry(idx.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        for (name, value) in params {
            map_put(self.storage.get_mut(idx).unwrap(), name, value.clone());
        }
        self.storage_save(idx).await
    }

    /// `PluginHost::get($sender, $name, $default_value)`.
    pub async fn storage_get(
        &mut self,
        plugin: &dyn Plugin,
        name: &str,
        default: Value,
    ) -> Result<Value, SqlxError> {
        self.storage_load().await?;
        let idx = plugin.class_name();
        Ok(self
            .storage
            .get(idx)
            .and_then(|v| v.get(name))
            .cloned()
            .unwrap_or(default))
    }

    /// `PluginHost::get_array($sender, $name, $default_value)` — the stored
    /// value unless it is not an array (PHP: `if (!is_array($tmp))`).
    pub async fn storage_get_array(
        &mut self,
        plugin: &dyn Plugin,
        name: &str,
        default: Value,
    ) -> Result<Value, SqlxError> {
        let tmp = self.storage_get(plugin, name, Value::Bool(false)).await?;
        Ok(if matches!(tmp, Value::Array(_)) {
            tmp
        } else {
            default
        })
    }

    /// `PluginHost::get_all($sender)` — the in-memory map, **without** a DB
    /// load (PHP's quirk; see the module docs).
    pub fn storage_get_all(&self, plugin: &dyn Plugin) -> Value {
        self.storage
            .get(plugin.class_name())
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()))
    }

    /// `PluginHost::profile_set($sender, $name, $value)` — the current
    /// preference profile, else the plain `set`.
    pub async fn storage_profile_set(
        &mut self,
        plugin: &dyn Plugin,
        name: &str,
        value: Value,
    ) -> Result<(), SqlxError> {
        let Some(profile) = self.profile.filter(|p| *p != 0) else {
            return self.storage_set(plugin, name, value).await;
        };
        let idx = plugin.class_name();
        self.storage
            .entry(idx.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        profile_map_put(self.storage.get_mut(idx).unwrap(), profile as i64, name, value);
        self.storage_save(idx).await
    }

    /// `PluginHost::profile_get($sender, $name, $default_value)` — the
    /// current preference profile, else the plain `get`.
    pub async fn storage_profile_get(
        &mut self,
        plugin: &dyn Plugin,
        name: &str,
        default: Value,
    ) -> Result<Value, SqlxError> {
        let Some(profile) = self.profile.filter(|p| *p != 0) else {
            return self.storage_get(plugin, name, default).await;
        };
        self.storage_load().await?;
        let idx = plugin.class_name();
        Ok(self
            .storage
            .get(idx)
            .and_then(|v| profile_map_get(v, profile as i64, name))
            .cloned()
            .unwrap_or(default))
    }

    /// `PluginHost::clear_data($sender)` — forget the in-memory map and
    /// delete the owner's row.
    pub async fn storage_clear(&mut self, plugin: &dyn Plugin) -> Result<(), SqlxError> {
        let Some(uid) = self.owner_uid.filter(|u| *u != 0) else {
            return Ok(());
        };
        let idx = plugin.class_name();
        self.storage.remove(idx);
        sqlx::query("DELETE FROM ttrss_plugin_storage WHERE name = $1 AND owner_uid = $2")
            .bind(idx)
            .bind(uid)
            .execute(self.pool)
            .await?;
        Ok(())
    }
}
