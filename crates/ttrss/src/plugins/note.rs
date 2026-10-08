//! The `note` plugin (`tt-rss/plugins/note/init.php`, M7.4) — article notes
//! in the **`ttrss_user_entries.note` column** (keyed `ref_id` + `owner_uid`
//! — not plugin storage).
//!
//! PHP's `init` registers only `HOOK_ARTICLE_BUTTON`; the PluginHandler
//! methods are `edit` (the dialog body — the textarea only when the article
//! has a `ttrss_user_entries` row, the Save/Cancel footer always) and
//! `setNote` (`UPDATE` + `json_encode([id, note])`). There is no clear
//! method — clearing a note is `setNote` with an empty string.
//!
//! The frontend (`note.js`, already embedded in the shell) POSTs
//! `note/edit` (the reply is raw dialog HTML) and `note/setnote` (the reply
//! is JSON `{id, note}`).
//!
//! The button row is rendered in `crate::headlines` (M4's static embedding
//! is the DOM-normalized oracle output); M7.21 makes hook-driven rendering
//! data-driven.

use async_trait::async_trait;
use serde_json::{Value, json};

use sqlx::Row;

use crate::controls::{cancel_dialog_tag, hidden_tag, pluginhandler_tags, submit_tag};
use crate::i18n::__;
use crate::pluginhost::HOOK_ARTICLE_BUTTON;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, MethodOutput, Plugin, PluginError};
use crate::util::clean_str;

/// PHP's `class Note extends Plugin`.
pub struct Note;

#[async_trait]
impl Plugin for Note {
    fn name(&self) -> &'static str {
        "note"
    }
    fn class_name(&self) -> &'static str {
        "Note"
    }
    fn about(&self) -> About {
        About::new("Adds support for setting article notes", "fox")
    }
    fn api_version(&self) -> i32 {
        2
    }
    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_ARTICLE_BUTTON);
    }

    fn get_js(&self) -> String {
        include_str!("../../../../tt-rss/plugins/note/note.js").to_string()
    }
    fn get_css(&self) -> String {
        include_str!("../../../../tt-rss/plugins/note/note.css").to_string()
    }

    /// `Note::hook_article_button` — the `note` icon in the article-row
    /// button row. (PHP's double-quoted literal keeps a source newline +
    /// indent between `onclick` and `style`; the DOM round-trip normalizes
    /// it, so the port emits the single-line form — same attributes, order,
    /// and visible text.)
    async fn hook_article_button(&self, _host: &mut HostCtx, line: &Value) -> String {
        let id = line["id"].as_i64().unwrap_or(0);
        let edit_note_title = __("Edit article note");
        format!(
            "<i class='material-icons' onclick=\"Plugins.Note.edit({id})\" \
             style='cursor : pointer' title=\"{edit_note_title}\">note</i>"
        )
    }

    /// PHP's `method_exists`/method dispatch is case-insensitive; the RPC
    /// layer lowercases `method` before dispatch.
    fn has_public_method(&self, method: &str) -> bool {
        method.eq_ignore_ascii_case("edit") || method.eq_ignore_ascii_case("setnote")
    }

    async fn invoke_method(
        &self,
        host: &mut HostCtx,
        method: &str,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        match method.to_ascii_lowercase().as_str() {
            "edit" => Ok(MethodOutput::Raw(edit(host, params).await?)),
            "setnote" => Ok(MethodOutput::Json(set_note(host, params).await?)),
            _ => Err(PluginError::UnknownMethod(method.to_string())),
        }
    }
}

/// `Note::edit` — the dialog body. The textarea only when the article has a
/// `ttrss_user_entries` row (the row appears when the article is first
/// read); `$_SESSION['uid']` scopes the read.
async fn edit(host: &mut HostCtx, params: &Value) -> Result<String, PluginError> {
    let id = clean_str(params.get("id").and_then(|v| v.as_str()).unwrap_or(""));
    let owner = host.owner_uid.unwrap_or(0);

    // `Some(None)` — the row exists but the note is NULL — still renders the
    // textarea, as PHP's `if ($row = fetch())` tests row presence, not the
    // note value. (A flat `Option<String>` would conflate the two.)
    let note: Option<Option<String>> = sqlx::query(
        "SELECT note FROM ttrss_user_entries WHERE ref_id = $1::integer AND owner_uid = $2",
    )
    .bind(&id)
    .bind(owner)
    .fetch_optional(host.pool())
    .await
    .map_err(|e| PluginError::Other(e.to_string()))?
    .map(|row| row.try_get::<Option<String>, _>(0).unwrap_or_default());

    let mut out = String::new();
    if let Some(note_val) = note {
        let note = note_val.unwrap_or_default();
        out.push_str(&hidden_tag("id", &id));
        // PHP: `pluginhandler_tags($this, "setnote")` — the form the dialog's
        // Save button posts to.
        out.push_str(&pluginhandler_tags("Note", "setnote"));
        // PHP interpolates the note verbatim (`<?= $note ?>`) — no
        // escaping. Ported as-is per the spec (the note is owner-scoped;
        // the unescaped echo is a reference flaw, surfaced at M7.4 landing).
        out.push_str(&format!(
            "<textarea dojoType='dijit.form.SimpleTextarea' \
             style='font-size : 12px; width : 98%; height: 100px;' \
             name='note'>{}</textarea>",
            note
        ));
    }
    // The footer always prints, even with no row.
    out.push_str("<footer class='text-center'>");
    out.push_str(&submit_tag(&__("Save")));
    out.push_str(&cancel_dialog_tag(&__("Cancel")));
    out.push_str("</footer>");
    Ok(out)
}

/// `Note::setNote` — `UPDATE ttrss_user_entries SET note = …` (no INSERT: a
/// note on an article with no `ttrss_user_entries` row is a no-op, as in
/// PHP). Echoes `json_encode([id, note])` — PHP's insertion order, id then
/// note (serde_json's BTreeMap happens to sort them the same).
async fn set_note(host: &mut HostCtx, params: &Value) -> Result<Value, PluginError> {
    let id = php_int_cast(&clean_str(
        params.get("id").and_then(|v| v.as_str()).unwrap_or(""),
    ));
    let note = clean_str(params.get("note").and_then(|v| v.as_str()).unwrap_or(""));
    let owner = host.owner_uid.unwrap_or(0);

    sqlx::query("UPDATE ttrss_user_entries SET note = $1 WHERE ref_id = $2 AND owner_uid = $3")
        .bind(&note)
        .bind(id)
        .bind(owner)
        .execute(host.pool())
        .await
        .map_err(|e| PluginError::Other(e.to_string()))?;

    Ok(json!({ "id": id, "note": note }))
}

/// PHP's `(int)` cast: the leading numeric run (sign + digits), else 0
/// (`(int)"12abc"` == 12, `(int)"abc"` == 0).
fn php_int_cast(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut end = 0;
    if b.first().is_some_and(|c| *c == b'-' || *c == b'+') {
        end = 1;
    }
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    s.get(..end)
        .and_then(|p| p.parse::<i32>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_int_cast_matches_php() {
        assert_eq!(php_int_cast("12"), 12);
        assert_eq!(php_int_cast("12abc"), 12);
        assert_eq!(php_int_cast("-5x"), -5);
        assert_eq!(php_int_cast("abc"), 0);
        assert_eq!(php_int_cast("1.5"), 1);
        assert_eq!(php_int_cast(""), 0);
    }
}
