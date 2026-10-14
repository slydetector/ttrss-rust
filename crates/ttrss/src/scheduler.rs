//! Port of `classes/Scheduler.php` + `RSSUtils::init_housekeeping_tasks` +
//! `RSSUtils::housekeeping_common` — the cron-driven scheduled tasks the
//! updater runs during housekeeping ("task 0", M6c.4).
//!
//! Task state is kept in `ttrss_scheduled_tasks` (`task_name`, `last_run`,
//! `last_duration`, `last_rc`, `last_cron_expression`). A task is *due* when
//! its cron expression's next run (from the recorded `last_run`, or the epoch
//! if it has never run) is in the past — the PHP's
//! `CronExpression::getNextRunDate($last_run)->getTimestamp() - time() < 0`.
//! The `cron` crate (like the PHP's `dragonmantank/cron-expression`) supports
//! the `@daily` / `@hourly` / `@weekly` macros and full cron expressions.
//!
//! The per-user variant (`RSSUtils::housekeeping_user` — per-owner plugin
//! tasks + the `HOOK_HOUSE_KEEPING` hook) is [`run_plugin_tasks`] (the
//! `HOOK_HOUSE_KEEPING` hook itself remains M8).

use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime, TimeZone as _};
use croner::Cron;
use sqlx::{AssertSqlSafe, PgPool, Row};
use std::str::FromStr;

use crate::core::config::Config;

/// `Scheduler::TASK_RC_EXCEPTION` — the return code recorded when a task
/// throws (the PHP's `-100`).
pub const TASK_RC_EXCEPTION: i32 = -100;

/// The `ttrss_scheduled_tasks` default for a task that has never run (the
/// PHP's `'1970-01-01 00:00'`).
const EPOCH: NaiveDateTime = NaiveDateTime::new(
    chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid date"),
    chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("valid time"),
);

/// A registered scheduled task (a `Scheduler::add_scheduled_task` entry).
pub struct ScheduledTask {
    pub name: &'static str,
    pub cron_expression: String,
}

/// `RSSUtils::init_housekeeping_tasks` — the system tasks the updater
/// registers (names + cron expressions from `Config`, mirroring the PHP's
/// `SCHEDULE_*` options). Also includes the Scheduler's own
/// `purge_orphaned_scheduled_tasks` task (`@weekly` — registered by the
/// Scheduler's constructor in the PHP).
pub fn system_tasks(cfg: &Config) -> Vec<ScheduledTask> {
    vec![
        ScheduledTask {
            name: "purge_orphans",
            cron_expression: cfg.schedule_purge_orphans.clone(),
        },
        ScheduledTask {
            name: "disk_cache_expire_all",
            cron_expression: cfg.schedule_disk_cache_expire_all.clone(),
        },
        ScheduledTask {
            name: "disable_failed_feeds",
            cron_expression: cfg.schedule_disable_failed_feeds.clone(),
        },
        ScheduledTask {
            name: "cleanup_feed_icons",
            cron_expression: cfg.schedule_cleanup_feed_icons.clone(),
        },
        ScheduledTask {
            name: "log_daemon_update_login_limit_users",
            cron_expression: cfg.schedule_log_daemon_update_login_limit_users.clone(),
        },
        ScheduledTask {
            name: "expire_error_log",
            cron_expression: cfg.schedule_expire_error_log.clone(),
        },
        ScheduledTask {
            name: "send_headlines_digests",
            cron_expression: cfg.schedule_send_headlines_digests.clone(),
        },
        ScheduledTask {
            name: "purge_orphaned_scheduled_tasks",
            cron_expression: "@weekly".to_string(),
        },
    ]
}

/// Interpret a `timestamp without time zone` value (the session is pinned to
/// `TIME ZONE 0`, so the stored value is a UTC wall clock; the PHP reads it
/// as a string and interprets it in the server timezone — the host local
/// time, which is what `chrono::Local` is).
fn local_dt(naive: NaiveDateTime) -> chrono::DateTime<Local> {
    use chrono::LocalResult;
    match Local.from_local_datetime(&naive) {
        LocalResult::None => naive.and_utc().with_timezone(&Local),
        other => other
            .earliest()
            .expect("local datetime resolves to a value"),
    }
}

/// Whether a task is due (the PHP's `getNextRunDate($last_run) < time()`).
///
/// The PHP's `CronExpression::getNextRunDate($last_run)` returns the next run
/// strictly after `$last_run` at minute granularity; the task is due when that
/// next run is in the past. `find_next_occurrence(&last, false)` matches that
/// (`inclusive = false` → strictly after `last`).
pub fn is_due(cron: &Cron, last_run: NaiveDateTime, now: chrono::DateTime<Local>) -> bool {
    let last = local_dt(last_run);
    cron.find_next_occurrence(&last, false)
        .is_ok_and(|next| next < now)
}

/// `Scheduler::run_due_tasks` — run the registered scheduled tasks that are
/// due, and record the results in `ttrss_scheduled_tasks` (the
/// `housekeeping_common` entry point the daemon loop calls each cycle).
pub async fn run_due_tasks(pool: &PgPool, cfg: &Config, tasks: &[ScheduledTask]) {
    let now = Local::now();
    for task in tasks {
        let Ok(cron) = Cron::from_str(&task.cron_expression) else {
            tracing::warn!(
                task = task.name,
                expression = %task.cron_expression,
                "invalid cron expression; skipping task"
            );
            continue;
        };

        let last_run: NaiveDateTime = match sqlx::query_scalar(
            "SELECT last_run FROM ttrss_scheduled_tasks WHERE task_name = $1",
        )
        .bind(task.name)
        .fetch_optional(pool)
        .await
        .context("load scheduled task record")
        {
            Ok(r) => r.unwrap_or(EPOCH),
            Err(e) => {
                tracing::error!(task = task.name, error = %e, "failed to load task record");
                continue;
            }
        };

        if !is_due(&cron, last_run, now) {
            continue;
        }

        let started = std::time::Instant::now();
        let rc = run_task(pool, cfg, task).await;
        let duration = started.elapsed().as_secs() as i32;
        if rc == 0 {
            tracing::debug!(task = task.name, "scheduled task finished in {duration}s");
        } else {
            tracing::warn!(
                task = task.name,
                rc,
                "scheduled task failed after {duration}s"
            );
        }

        // Store the normalized expression (the PHP's `getExpression()`: the
        // `@daily` shorthand is expanded to `0 0 * * *`, etc.) — croner's
        // `pattern` renders the parsed fields back to that exact form.
        let cron_expr = cron.pattern.to_string();
        let _ = record_run(pool, task.name, duration, rc, &cron_expr).await;
    }
}

/// `Scheduler::purge_orphaned_tasks` — purge records of tasks that are no
/// longer registered and have not run in 5 weeks. Returns the PHP return
/// code (0 = success, 1 = failure).
pub async fn purge_orphaned_scheduled_tasks(pool: &PgPool, registered: &[&str]) -> i32 {
    if registered.is_empty() {
        tracing::error!("purge_orphaned_scheduled_tasks: invoked before tasks were registered");
        return 1;
    }
    let qmarks: Vec<String> = (0..registered.len())
        .map(|i| format!("${}", i + 1))
        .collect();
    let sql = format!(
        "DELETE FROM ttrss_scheduled_tasks WHERE task_name NOT IN ({}) \
         AND last_run < NOW() - INTERVAL '5 weeks'",
        qmarks.join(", ")
    );
    let mut q = sqlx::query(AssertSqlSafe(sql));
    for name in registered {
        q = q.bind(name);
    }
    match q.execute(pool).await {
        Ok(_) => 0,
        Err(e) => {
            tracing::error!(error = %e, "purge_orphaned_scheduled_tasks failed");
            1
        }
    }
}

/// Record (or create) a task's run result (the PHP's ORM record update).
/// `last_run` is `NOW()` (the PHP's `Db::NOW()`).
pub async fn record_run(
    pool: &PgPool,
    task_name: &str,
    last_duration: i32,
    last_rc: i32,
    last_cron_expression: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ttrss_scheduled_tasks \
             (task_name, last_duration, last_rc, last_run, last_cron_expression) \
         VALUES ($1, $2, $3, NOW(), $4) \
         ON CONFLICT (task_name) DO UPDATE SET \
             last_duration = EXCLUDED.last_duration, \
             last_rc = EXCLUDED.last_rc, \
             last_run = NOW(), \
             last_cron_expression = EXCLUDED.last_cron_expression",
    )
    .bind(task_name)
    .bind(last_duration)
    .bind(last_rc)
    .bind(last_cron_expression)
    .execute(pool)
    .await
    .context("record scheduled task run")?;
    Ok(())
}

/// Dispatch a single registered task (the PHP's closure set). Returns the
/// task's return code (0 = success; [`TASK_RC_EXCEPTION`] on a task error).
pub async fn run_task(pool: &PgPool, cfg: &Config, task: &ScheduledTask) -> i32 {
    match run_task_inner(pool, cfg, task.name).await {
        Ok(rc) => rc,
        Err(e) => {
            tracing::error!(task = task.name, error = %e, "scheduled task failed");
            TASK_RC_EXCEPTION
        }
    }
}

async fn run_task_inner(pool: &PgPool, cfg: &Config, name: &str) -> Result<i32> {
    match name {
        "purge_orphans" => {
            let deleted = purge_orphans(pool).await?;
            tracing::debug!("purged {deleted} orphaned posts");
            Ok(0)
        }
        // `RSSUtils::init_housekeeping_tasks` → `DiskCache::instance("");
        // expire_all()` — sweep every `CACHE_DIR` subdirectory (the 30s
        // feed-XML cache itself was dropped by D5 = c; the `images`/plugin
        // caches remain and are swept here, `.no-auto-expiry` dirs exempt).
        "disk_cache_expire_all" => {
            let removed = crate::diskcache::expire_all(&cfg.cache_dir, cfg.cache_max_days as i64);
            if removed > 0 {
                tracing::info!("expired {removed} stale cache files");
            }
            Ok(0)
        }
        "disable_failed_feeds" => {
            let disabled = disable_failed_feeds(pool, cfg).await?;
            if disabled > 0 {
                tracing::info!("disabled {disabled} failed feeds");
            }
            Ok(0)
        }
        "cleanup_feed_icons" => {
            cleanup_feed_icons(pool, cfg).await?;
            Ok(0)
        }
        "log_daemon_update_login_limit_users" => {
            log_daemon_update_login_limit_users(pool, cfg).await?;
            Ok(0)
        }
        "expire_error_log" => {
            expire_error_log(pool).await?;
            Ok(0)
        }

        // NOTE(M6c.4): the PHP's `Digest::send_headlines_digests` sends the
        // pending headline digests by email. The digest *body* is ported
        // (`crate::digest::prepare_headlines_digest`); the mailer (SMTP send)
        // is not part of this port, so the task is a no-op.
        "send_headlines_digests" => Ok(0),
        "purge_orphaned_scheduled_tasks" => {
            Ok(purge_orphaned_scheduled_tasks(pool, &system_task_names(cfg)).await)
        }
        other => Err(anyhow::anyhow!("unknown scheduled task: {other}")),
    }
}

/// The names of all registered system tasks (for the orphaned-task purge).
fn system_task_names(cfg: &Config) -> Vec<&'static str> {
    system_tasks(cfg).into_iter().map(|t| t.name).collect()
}

/// `Article::_purge_orphans` — delete `ttrss_entries` rows with no remaining
/// `ttrss_user_entries`. Returns the number of deleted rows.
pub async fn purge_orphans(pool: &PgPool) -> Result<i64> {
    let res = sqlx::query(
        "DELETE FROM ttrss_entries WHERE \
         NOT EXISTS (SELECT ref_id FROM ttrss_user_entries WHERE ref_id = id)",
    )
    .execute(pool)
    .await
    .context("purge_orphans")?;
    Ok(res.rows_affected() as i64)
}

/// `RSSUtils::disable_failed_feeds` — auto-disable feeds that have not
/// succeeded in updating for `DAEMON_UNSUCCESSFUL_DAYS_LIMIT` days (but are
/// still being attempted: `last_updated` within the last day). Runs in a
/// transaction, logging each feed it disables. Returns the number of feeds
/// disabled.
pub async fn disable_failed_feeds(pool: &PgPool, cfg: &Config) -> Result<i64> {
    let days = cfg.daemon_unsuccessful_days_limit;
    if days <= 0 {
        return Ok(0);
    }

    let qpart = format!(
        "update_interval != -1 AND last_successful_update IS NOT NULL \
         AND last_successful_update < NOW() - INTERVAL '{days} day' \
         AND last_updated > NOW() - INTERVAL '1 day'"
    );

    let mut tx = pool.begin().await.context("begin disable_failed_feeds")?;

    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT id, title, owner_uid FROM ttrss_feeds WHERE {qpart}"
    )))
    .fetch_all(&mut *tx)
    .await
    .context("select failing feeds")?;

    for r in &rows {
        let id: i32 = r.get(0);
        let title: String = r.get(1);
        let owner_uid: i32 = r.get(2);
        tracing::info!(
            "Auto disabling feed {id} ({title}, UID: {owner_uid}) because it failed to update for {days} days."
        );
    }

    let res = sqlx::query(AssertSqlSafe(format!(
        "UPDATE ttrss_feeds SET update_interval = -1 WHERE {qpart}"
    )))
    .execute(&mut *tx)
    .await
    .context("disable failing feeds")?;

    tx.commit().await.context("commit disable_failed_feeds")?;
    Ok(res.rows_affected() as i64)
}

/// `RSSUtils::cleanup_feed_icons` — walk the feed-icon cache: a numeric
/// entry (a feed id) older than `CACHE_MAX_DAYS` days is refreshed (mtime
/// updated) if the feed still exists, otherwise deleted. Returns the number
/// of files removed.
pub async fn cleanup_feed_icons(pool: &PgPool, cfg: &Config) -> std::io::Result<i64> {
    let dir = &cfg.feed_icon_dir();
    if !dir.is_dir() {
        return Ok(0);
    }
    let max_age = 86400u64 * (cfg.cache_max_days.max(1) as u64);
    let now = std::time::SystemTime::now();
    let mut removed = 0i64;

    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let mtime = match entry.metadata()?.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let age = now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0);
        if age < max_age {
            continue;
        }
        let feed_id: i32 = match name.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };
        // `SELECT id FROM ttrss_feeds WHERE id = ?`
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ttrss_feeds WHERE id = $1)")
                .bind(feed_id)
                .fetch_one(pool)
                .await
                .unwrap_or(false);
        if exists {
            // The PHP: `$cache->put($icon, $cache->get($icon))` — rewrite the
            // file (refreshing its mtime).
            let path = entry.path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let _ = std::fs::write(&path, bytes);
        } else {
            tracing::debug!(icon = %name, "removing orphaned feed icon");
            let _ = std::fs::remove_file(entry.path());
            removed += 1;
        }
    }
    Ok(removed)
}

/// `RSSUtils::expire_error_log` — delete `ttrss_error_log` rows older than a
/// week. Returns the number of deleted rows.
pub async fn expire_error_log(pool: &PgPool) -> Result<i64> {
    let res =
        sqlx::query("DELETE FROM ttrss_error_log WHERE created_at < NOW() - INTERVAL '1 week'")
            .execute(pool)
            .await
            .context("expire_error_log")?;
    Ok(res.rows_affected() as i64)
}

/// The `log_daemon_update_login_limit_users` task — log the users excluded
/// from daemon updates by the `DAEMON_UPDATE_LOGIN_LIMIT` check.
pub async fn log_daemon_update_login_limit_users(pool: &PgPool, cfg: &Config) -> Result<()> {
    if cfg.single_user_mode || cfg.daemon_update_login_limit <= 0 {
        return Ok(());
    }
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT login, last_login FROM ttrss_users \
         WHERE access_level NOT IN (-2, -1) \
         AND last_login < NOW() - INTERVAL '{} days'",
        cfg.daemon_update_login_limit
    )))
    .fetch_all(pool)
    .await
    .context("select stale-login users")?;
    if !rows.is_empty() {
        tracing::info!(
            limit_days = cfg.daemon_update_login_limit,
            "Feeds will not be updated for these users because of DAEMON_UPDATE_LOGIN_LIMIT check"
        );
        for r in &rows {
            let login: String = r.get(0);
            let last_login: Option<NaiveDateTime> = r.get(1);
            tracing::info!("=> {login}, last logged in: {last_login:?}");
        }
    }
    Ok(())
}

/// `housekeeping_user` — run the plugin-registered scheduled tasks that are
/// due (PHP: `RSSUtils::housekeeping_user` + `PluginHost::run_scheduled_tasks`).
///
/// Unlike [`run_due_tasks`] (system tasks, matched by name to a known handler),
/// plugin tasks carry their own `callback` closure. The full task name follows
/// the PHP's `add_scheduled_task` naming: `Class:task` (system plugin) or
/// `Class:task:uid` (user plugin).
pub async fn run_plugin_tasks(
    pool: &PgPool,
    cfg: &Config,
    plugins: &crate::pluginhost::host::PluginHost,
) {
    let now = Local::now();
    let tasks = plugins.scheduled_tasks();
    for task in tasks {
        let plugin = &plugins.plugin(task.plugin);
        let full_name =
            crate::pluginhost::host::scheduled_task_name(*plugin, &task.task_name, None);

        let Ok(cron) = Cron::from_str(&task.cron) else {
            tracing::warn!(
                task = %full_name,
                expression = %task.cron,
                "invalid cron expression; skipping plugin task"
            );
            continue;
        };

        let last_run: NaiveDateTime = match sqlx::query_scalar(
            "SELECT last_run FROM ttrss_scheduled_tasks WHERE task_name = $1",
        )
        .bind(&full_name)
        .fetch_optional(pool)
        .await
        .context("load scheduled task record")
        {
            Ok(r) => r.unwrap_or(EPOCH),
            Err(e) => {
                tracing::error!(task = %full_name, error = %e, "failed to load task record");
                continue;
            }
        };

        if !is_due(&cron, last_run, now) {
            continue;
        }

        let started = std::time::Instant::now();
        let pool_clone = pool.clone();
        let cfg_clone = cfg.clone();
        (task.callback)(pool_clone, None, cfg_clone).await;
        let duration = started.elapsed().as_secs() as i32;
        tracing::debug!(task = %full_name, "plugin task finished in {duration}s");

        let cron_expr = cron.pattern.to_string();
        let _ = record_run(pool, &full_name, duration, 0, &cron_expr).await;
    }
}
