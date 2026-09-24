//! `Pref_System` backend read handlers.

use serde_json::{json, Map, Value};
use sqlx::Row;
use sqlx::AssertSqlSafe;

use crate::session::Sess;
use crate::time::{make_local_datetime, TzCtx};
use crate::util::{clean_int, htmlspecialchars};
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
        let row_style = if last_rc == 0 {
            "text-success"
        } else {
            "text-error"
        };
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

/// `Pref_System::LOG_PAGE_LIMIT` — rows per event-log page.
const LOG_PAGE_LIMIT: i64 = 15;

/// `Logger::ERROR_NAMES` — a PHP errno → its symbolic name.
fn error_name(errno: i32) -> &'static str {
    match errno {
        1 => "E_ERROR",
        2 => "E_WARNING",
        4 => "E_PARSE",
        8 => "E_NOTICE",
        16 => "E_CORE_ERROR",
        32 => "E_CORE_WARNING",
        64 => "E_COMPILE_ERROR",
        128 => "E_COMPILE_WARNING",
        256 => "E_USER_ERROR",
        512 => "E_USER_WARNING",
        1024 => "E_USER_NOTICE",
        2048 => "E_STRICT",
        4096 => "E_RECOVERABLE_ERROR",
        8192 => "E_DEPRECATED",
        16384 => "E_USER_DEPRECATED",
        32767 => "E_ALL",
        _ => "UNKNOWN",
    }
}

/// `Pref_System::_log_viewer`'s severity → errno filter. `None` = no filter
/// (all rows). The values are hardcoded constants (safe to inline in SQL).
fn severity_errno_filter(severity: i64) -> Option<&'static [i32]> {
    match severity {
        256 => Some(&[1, 256, 4, 64]),
        512 => Some(&[1, 256, 4, 64, 2, 512, 8192, 16384]),
        _ => None,
    }
}

/// `Pref_System::_log_viewer($page, $severity)` — the paged, severity-filtered
/// error-log table + its toolbar. The port assumes `LOG_DESTINATION == sql` (the
/// default) — the same assumption as `recent_log_events`.
async fn log_viewer(state: &AppState, sess: &Sess, page: i64, severity: i64) -> String {
    let pool = state.db.pool();
    let tz = TzCtx::for_session(state, &sess.data()).await;

    let where_sql = match severity_errno_filter(severity) {
        Some(errnos) => format!(
            "errno IN ({})",
            errnos
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => "true".to_string(),
    };

    let total: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT COUNT(id) FROM ttrss_error_log WHERE {where_sql}"
    )))
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let total_pages = total / LOG_PAGE_LIMIT;
    let offset = LOG_PAGE_LIMIT * page;

    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT errno, errstr, filename, lineno, \
                 to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') AS created_at, login, context \
         FROM ttrss_error_log \
           LEFT JOIN ttrss_users ON (owner_uid = ttrss_users.id) \
         WHERE {where_sql} \
         ORDER BY ttrss_error_log.id DESC \
         LIMIT {LOG_PAGE_LIMIT} OFFSET {offset}"
    )))
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let p1 = (page + 1).to_string();
    let p2 = (total_pages + 1).to_string();
    let page_label = crate::i18n::t_sprintf("Page %d of %d", &[&p1, &p2]);
    let prev_disabled = if page <= 0 { "disabled" } else { "" };
    let next_disabled = if page >= total_pages { "disabled" } else { "" };

    let sev_values = [
        (256.to_string(), "Errors".to_string()),
        (512.to_string(), "Warnings".to_string()),
        (1024.to_string(), "Everything".to_string()),
    ];
    let severity_select = crate::controls::select_hash(
        "severity",
        &severity.to_string(),
        &sev_values,
        &[("onchange", "Helpers.EventLog.refresh()".to_string())],
    );

    let mut out = String::new();
    out.push_str("<div dojoType='dijit.layout.BorderContainer' gutters='false'>\n");
    out.push_str("\t\t<div region='top' dojoType='fox.Toolbar'>\n");
    out.push_str(&format!(
        "\t\t\t<button dojoType='dijit.form.Button' onclick='Helpers.EventLog.refresh()'>Refresh</button>\n\
         \t\t\t<button dojoType='dijit.form.Button' {prev_disabled} onclick='Helpers.EventLog.prevPage()'>&lt;&lt;</button>\n\
         \t\t\t<button dojoType='dijit.form.Button' disabled>{page_label}</button>\n\
         \t\t\t<button dojoType='dijit.form.Button' {next_disabled} onclick='Helpers.EventLog.nextPage()'>&gt;&gt;</button>\n\
         \t\t\t<button dojoType='dijit.form.Button' onclick='Helpers.EventLog.clear()'>Clear</button>\n\
         \t\t\t<div class='pull-right'>\n\
         \t\t\t\t<label>Severity:</label>\n\
         \t\t\t\t{severity_select}\n\
         \t\t\t</div>\n\
         \t\t</div>\n"
    ));
    out.push_str(
        "\t\t<div style=\"padding : 0px\" dojoType=\"dijit.layout.ContentPane\" region=\"center\">\n\
         \t\t\t<table width='100%' class='event-log'>\n\
         \t\t\t\t<tr>\n\
         \t\t\t\t\t<th width='5%'>Error</th>\n\
         \t\t\t\t\t<th>Filename</th>\n\
         \t\t\t\t\t<th>Message</th>\n\
         \t\t\t\t\t<th width='5%'>User</th>\n\
         \t\t\t\t\t<th width='5%'>Date</th>\n\
         \t\t\t\t</tr>\n",
    );
    for r in &rows {
        let errno: i32 = r.get(0);
        let errstr: String = r.get(1);
        let filename: String = r.get(2);
        let lineno: i32 = r.get(3);
        let created_at: String = r.get(4);
        let login: Option<String> = r.get(5);
        let context: String = r.get(6);
        let message = format!(
            "{}\n{}",
            htmlspecialchars(&errstr),
            htmlspecialchars(&context)
        );
        out.push_str(&format!(
            "\t\t\t\t<tr>\n\
             \t\t\t\t\t<td class='errno'>{name} ({errno})</td>\n\
             \t\t\t\t\t<td class='filename'>{filename}:{lineno}</td>\n\
             \t\t\t\t\t<td class='errstr'>{message}</td>\n\
             \t\t\t\t\t<td class='login'>{login}</td>\n\
             \t\t\t\t\t<td class='timestamp'>{date}</td>\n\
             \t\t\t\t</tr>\n",
            name = error_name(errno),
            filename = htmlspecialchars(&filename),
            login = htmlspecialchars(&login.unwrap_or_default()),
            date = make_local_datetime(Some(&created_at), false, &tz),
        ));
    }
    out.push_str(
        "\t\t\t</table>\n\
         \t\t</div>\n\
         \t</div>",
    );
    out
}

/// `Pref_System::index` — the admin system page: a Dojo accordion with an
/// event-log pane, an email-test form, and lazy-loaded scheduled-tasks /
/// server-info panes. The event-log pane assumes `LOG_DESTINATION == sql` (the
/// default, like `recent_log_events`); the `getphpinfo` pane surfaces
/// Rust/server diagnostics instead of `phpinfo()`.
pub async fn index(state: &AppState, sess: &Sess, extra: &Map<String, Value>) -> String {
    let severity = clean_int(extra.get("severity"));
    let severity = if severity == 0 { 512 } else { severity }; // default E_USER_WARNING
    let page = clean_int(extra.get("page"));
    let log_html = log_viewer(state, sess, page, severity).await;

    let pool = state.db.pool();
    let uid = sess.data().uid().unwrap_or(0);
    let email: String = sqlx::query_scalar("SELECT email FROM ttrss_users WHERE id = $1")
        .bind(uid)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str("<div dojoType='dijit.layout.AccordionContainer' region='center'>\n");

    // Event log pane (assumes SQL logging — the port's default).
    out.push_str(
        "<div dojoType='dijit.layout.AccordionPane' style='padding : 0' title='<i class=\"material-icons\">report</i> Event log'>\n",
    );
    out.push_str(&log_html);
    out.push_str("</div>\n");

    // Email configuration pane (the mailer is a no-op — see send_test_email).
    out.push_str(
        "<div dojoType='dijit.layout.AccordionPane' style='padding : 0' title='<i class=\"material-icons\">mail</i> Email configuration'>\n",
    );
    out.push_str("\t<div dojoType=\"dijit.layout.ContentPane\">\n");
    out.push_str("\t\t<form dojoType=\"dijit.form.Form\">\n");
    out.push_str(
        "\t\t\t<script type=\"dojo/method\" event=\"onSubmit\" args=\"evt\">\n\
         \t\t\t\tevt.preventDefault();\n\
         \t\t\t\tif (this.validate()) {\n\
         \t\t\t\t\txhr.json(\"backend.php\", this.getValues(), (reply) => {\n\
         \t\t\t\t\t\tconst msg = document.getElementById(\"mail-test-result\");\n\
         \t\t\t\t\t\tif (reply.rc) {\n\
         \t\t\t\t\t\t\tmsg.innerHTML = \"Email sent.\";\n\
         \t\t\t\t\t\t\tmsg.className = 'alert alert-success';\n\
         \t\t\t\t\t\t} else {\n\
         \t\t\t\t\t\t\tmsg.innerHTML = reply.error;\n\
         \t\t\t\t\t\t\tmsg.className = 'alert alert-danger';\n\
         \t\t\t\t\t\t}\n\
         \t\t\t\t\t\tmsg.show();\n\
         \t\t\t\t\t})\n\
         \t\t\t\t}\n\
         \t\t\t</script>\n",
    );
    out.push_str(&crate::controls::hidden_tag("op", "Pref_System"));
    out.push_str(&crate::controls::hidden_tag("method", "sendTestEmail"));
    out.push_str("\t\t\t<fieldset>\n");
    out.push_str("\t\t\t\t<label>To:</label>\n");
    out.push_str(&crate::controls::input_tag(
        "mail_address",
        &email,
        "text",
        &[("required", "1".to_string())],
        "",
    ));
    out.push_str(&crate::controls::submit_tag("Send test email"));
    out.push_str(
        "\t\t\t\t<span style=\"display: none; margin-left : 10px\" class=\"alert alert-error\" id=\"mail-test-result\">...</span>\n",
    );
    out.push_str("\t\t\t</fieldset>\n");
    out.push_str("\t\t</form>\n");
    out.push_str("\t</div>\n");
    out.push_str("</div>\n");

    // Scheduled tasks pane (lazy — loads getscheduledtasks on select).
    out.push_str(
        "<div dojoType='dijit.layout.AccordionPane' title='<i class=\"material-icons\">alarm</i> Scheduled tasks'>\n",
    );
    out.push_str(
        "<script type='dojo/method' event='onSelected' args='evt'>\n\
         if (this.domNode.querySelector('.loading'))\n\
         window.setTimeout(() => {\n\
         xhr.post(\"backend.php\", {op: 'Pref_System', method: 'getscheduledtasks'}, (reply) => {\n\
         this.attr('content', `<div class='phpinfo'>${reply}</div>`);\n\
         });\n\
         }, 200);\n\
         </script>\n",
    );
    out.push_str("<span class='loading'>Loading, please wait...</span>\n");
    out.push_str("</div>\n");

    // Server Information pane (lazy — the port surfaces Rust/server diagnostics
    // via get_php_info in place of phpinfo()).
    out.push_str(
        "<div dojoType='dijit.layout.AccordionPane' title='<i class=\"material-icons\">info</i> Server Information'>\n",
    );
    out.push_str(
        "<script type='dojo/method' event='onSelected' args='evt'>\n\
         if (this.domNode.querySelector('.loading'))\n\
         window.setTimeout(() => {\n\
         xhr.post(\"backend.php\", {op: 'Pref_System', method: 'getphpinfo'}, (reply) => {\n\
         this.attr('content', `<div class='phpinfo'>${reply}</div>`);\n\
         });\n\
         }, 200);\n\
         </script>\n",
    );
    out.push_str("<span class='loading'>Loading, please wait...</span>\n");
    out.push_str("</div>\n");

    // M7.22 item 7 — the `Pref_System::index` plugin chain (PHP site after
    // the PHP Information pane, before the container closes).
    out.push_str(&crate::pluginhost::dispatch::prefs_chain(
        state,
        sess,
        crate::pluginhost::HOOK_PREFS_TAB,
        "prefSystem",
    )
    .await);

    out.push_str("</div>");
    out
}

/// `Pref_System::clearLog` — delete all rows from the SQL error log.
pub async fn clear_log(state: &AppState) {
    sqlx::query("DELETE FROM ttrss_error_log")
        .execute(state.db.pool())
        .await
        .ok();
}

/// `Pref_System::sendTestEmail` — no-op. The Rust server does not wire up the
/// system MTA (the PHP `Mailer` shells out to the host mailer), so a test
/// e-mail can't actually be sent. Returns the `{rc, error}` shape the form's JS
/// expects so the dialog degrades to an error message rather than hanging.
pub fn send_test_email() -> Value {
    json!({
        "rc": 0,
        "error": "Outgoing mail is not supported by the Rust server (the system MTA is not wired up)."
    })
}

/// `Pref_System::getphpinfo` — server diagnostics. The PHP `phpinfo()` dump
/// doesn't apply to the Rust server, so this surfaces the relevant Rust/server
/// info (version, build target, OS/arch, key config, database) to bridge the
/// gap. (No sensitive values: the db password and encryption key are omitted.)
pub fn get_php_info(state: &AppState) -> String {
    use std::env::consts;
    let c = &state.config;
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let target = format!(
        "{}/{} (family {})",
        consts::OS,
        consts::ARCH,
        consts::FAMILY
    );
    let crate_id = concat!(env!("CARGO_PKG_NAME"), " v", env!("CARGO_PKG_VERSION"));

    let mut out = String::new();
    out.push_str("<table border=\"0\" cellpadding=\"2\" cellspacing=\"0\" style=\"font-family:monospace\">\n");
    let row = |out: &mut String, key: &str, value: &str| {
        out.push_str(&format!(
            "<tr><td style=\"text-align:right\">{}</td><td>{}</td></tr>\n",
            htmlspecialchars(key),
            htmlspecialchars(value)
        ));
    };
    let head = |out: &mut String, title: &str| {
        out.push_str(&format!(
            "<tr><th colspan=\"2\" style=\"background:#e8e8e8\">{}</th></tr>\n",
            htmlspecialchars(title)
        ));
    };

    head(&mut out, "Server");
    row(
        &mut out,
        "tt-rss (Rust) version",
        crate::core::config::VERSION,
    );
    row(&mut out, "Server crate", crate_id);
    row(&mut out, "Build target", &target);
    row(&mut out, "Available parallelism", &cpus.to_string());

    head(&mut out, "Configuration");
    row(&mut out, "Self URL", &c.self_url);
    row(&mut out, "Listen", &c.listen.to_string());
    row(&mut out, "Doc root", &c.docroot.display().to_string());
    row(
        &mut out,
        "Feed icons dir",
        &c.feed_icon_dir.display().to_string(),
    );
    row(
        &mut out,
        "Single-user mode",
        &c.single_user_mode.to_string(),
    );
    row(&mut out, "Plugins", &c.plugins);
    row(
        &mut out,
        "Max favicon size (bytes)",
        &c.max_favicon_file_size.to_string(),
    );

    head(&mut out, "Database");
    row(&mut out, "Host", &c.db_host);
    row(&mut out, "Port", &c.db_port.to_string());
    row(&mut out, "Name", &c.db_name);
    row(&mut out, "User", &c.db_user);
    row(&mut out, "SSL mode", &c.db_sslmode);

    out.push_str("</table>");
    out
}
