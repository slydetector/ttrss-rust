//! `Pref_System` backend read handlers.

use sqlx::Row;

use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util::htmlspecialchars;
use crate::AppState;

/// `Pref_System::getscheduledtasks` — an HTML table of the scheduled tasks.
/// (The PHP `echo`s raw markup but keeps the JSON content-type, so this is a
/// `DispatchOutput::Html`.)
pub async fn getscheduledtasks(state: &AppState, sess: &Sess) -> String {
    let pool = state.db.pool();
    let tz = TzCtx::for_session(state, &sess.data()).await;

    let rows = sqlx::query(
        "SELECT task_name, last_cron_expression, \
               to_char(last_run, 'YYYY-MM-DD HH24:MI:SS') AS last_run, \
               last_duration, last_rc \
         FROM ttrss_scheduled_tasks \
         ORDER BY last_cron_expression, task_name",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut out = String::new();
    out.push_str(
        "<table width='100%' class='event-log'>\n<tr>\n\
         <th>Task name</th>\n<th>Schedule</th>\n<th>Last executed</th>\n\
         <th>Duration (seconds)</th>\n<th>Return code</th>\n</tr>\n",
    );
    for r in &rows {
        let task_name: String = r.get(0);
        let cron: String = r.get(1);
        let last_run: String = r.get(2);
        let last_duration: i32 = r.get(3);
        let last_rc: i32 = r.get(4);
        let row_style = if last_rc == 0 { "text-success" } else { "text-error" };
        out.push_str(&format!(
            "<tr>\n<td class=\"{row_style}\">{}</td>\n<td>{}</td>\n<td>{}</td>\n<td>{}</td>\n<td>{}</td>\n</tr>\n",
            htmlspecialchars(&task_name),
            htmlspecialchars(&cron),
            make_local_datetime(Some(&last_run), false, &tz),
            last_duration,
            last_rc,
        ));
    }
    out.push_str("</table>");
    out
}
