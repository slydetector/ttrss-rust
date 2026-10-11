//! `ttrss importdb` — import this deployment's database from an existing
//! tt-rss install (the "try the port on your existing data" path).
//!
//! Streams `pg_dump` (source, custom format) straight into `pg_restore`
//! (this deployment's own database — no intermediate file). The source
//! coordinates are remembered in the data dir after a run, so re-importing
//! is one keypress (or `--yes` for scripts).

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password};

/// The existing install to import from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub user: String,
    pub pass: String,
}

impl Source {
    /// `postgres://` URL for the preflight (sqlx).
    pub fn url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}?sslmode=prefer",
            urlenc(&self.user),
            urlenc(&self.pass),
            self.host,
            self.port,
            urlenc(&self.db),
        )
    }
}

/// The remembered-source file (in the data dir): one `key=value` per line.
/// Trivially inspectable by hand — homelab users should be able to read
/// what is remembered without a tool.
pub fn source_file_lines(s: &Source) -> String {
    format!(
        "host={}\nport={}\ndb={}\nuser={}\npass={}\n",
        s.host, s.port, s.db, s.user, s.pass
    )
}

pub fn parse_source_file(text: &str) -> Option<Source> {
    let mut m: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    Some(Source {
        host: m.get("host")?.clone(),
        port: m.get("port")?.parse().ok()?,
        db: m.get("db")?.clone(),
        user: m.get("user")?.clone(),
        pass: m.get("pass")?.clone(),
    })
}

/// Percent-encode a userinfo/path segment of a postgres URL.
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Write the remembered source (0600 — it carries the source password).
fn remember(data_dir: &Path, s: &Source) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = data_dir.join("importdb-source");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&p)?;
    f.write_all(source_file_lines(s).as_bytes())
}

fn forget(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join("importdb-source")).is_ok()
}

/// Environment overrides (consistent with the command name). The password
/// also accepts `PGPASSWORD` — the libpq convention.
fn env_source() -> Option<Source> {
    let host = std::env::var("IMPORTDB_HOST")
        .ok()
        .filter(|v| !v.is_empty())?;
    let port = std::env::var("IMPORTDB_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5432);
    let db = std::env::var("IMPORTDB_DB")
        .ok()
        .filter(|v| !v.is_empty())?;
    let user = std::env::var("IMPORTDB_USER")
        .ok()
        .filter(|v| !v.is_empty())?;
    let pass = std::env::var("IMPORTDB_PASS")
        .ok()
        .or_else(|| std::env::var("PGPASSWORD").ok())
        .filter(|v| !v.is_empty())?;
    Some(Source {
        host,
        port,
        db,
        user,
        pass,
    })
}

/// Prompt for one field (TTY only): env beats the remembered value; both
/// prefill the prompt so the usual answer is a bare Enter.
fn prompt_field(theme: &ColorfulTheme, prompt: &str, prefilled: Option<String>) -> Option<String> {
    let mut i = Input::<String>::with_theme(theme).with_prompt(prompt);
    if let Some(v) = prefilled {
        i = i.default(v);
    }
    i.interact_text().ok()
}

/// Resolve the full source: non-TTY or `--yes` uses env, else remembered,
/// verbatim (missing is an error); a TTY without `--yes` prompts for any
/// missing field, prefilled by env/remembered.
fn resolve_source(yes: bool, tty: bool, theme: &ColorfulTheme, data_dir: &Path) -> Option<Source> {
    let from_env = env_source();
    let from_file = std::fs::read_to_string(data_dir.join("importdb-source"))
        .ok()
        .and_then(|t| parse_source_file(&t));

    if !tty || yes {
        let s = from_env.or(from_file)?;
        println!("  source: {}:{}/{} (user {})", s.host, s.port, s.db, s.user);
        return Some(s);
    }

    let pref = |get: fn(&Source) -> String| {
        from_env
            .as_ref()
            .map(get)
            .or_else(|| from_file.as_ref().map(get))
    };
    let host = prompt_field(theme, "existing tt-rss host", pref(|s| s.host.clone()))?;
    let port: u16 = prompt_field(theme, "existing tt-rss port", pref(|s| s.port.to_string()))?
        .trim()
        .parse()
        .ok()?;
    let db = prompt_field(theme, "database name", pref(|s| s.db.clone()))?;
    let user = prompt_field(theme, "database user", pref(|s| s.user.clone()))?;
    let pass = match (
        std::env::var("IMPORTDB_PASS")
            .ok()
            .filter(|v| !v.is_empty()),
        std::env::var("PGPASSWORD").ok().filter(|v| !v.is_empty()),
    ) {
        (Some(p), _) | (_, Some(p)) => Some(p),
        (None, None) => {
            let prompt = if from_file.as_ref().map(|s| &s.pass).is_some() {
                "database password (remembered — Enter to reuse)"
            } else {
                "database password"
            };
            Password::with_theme(theme)
                .with_prompt(prompt)
                .interact()
                .ok()
                .filter(|p| !p.is_empty())
                .or_else(|| from_file.as_ref().map(|s| s.pass.clone()))
        }
    }?;
    Some(Source {
        host,
        port,
        db,
        user,
        pass,
    })
}

/// sqlx preflight: prove the source is reachable and a tt-rss database
/// before touching the target. A `schema_version` mismatch is a NOTE, not a
/// failure — the port migrates older schemas at boot.
async fn preflight(s: &Source) -> Result<String, String> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&s.url())
        .await
        .map_err(|e| {
            format!(
                "could not connect to {}:{} as {}: {e}",
                s.host, s.port, s.user,
            )
        })?;
    let version: String = sqlx::query_scalar("SELECT version()")
        .fetch_one(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let schema: Option<i32> = sqlx::query_scalar("SELECT schema_version FROM ttrss_version")
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);
    let size: String =
        sqlx::query_scalar("SELECT pg_size_pretty(pg_database_size(current_database()))")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())?;
    let _ = pool.close().await;

    let engine = version.split(' ').next().unwrap_or("?");
    let mut line = format!("  source ok: {engine}, tt-rss schema {schema:?}, {size}");
    match schema {
        Some(v) if v < 151 => line
            .push_str("  (note: older than the current schema — the port will migrate it at boot)"),
        Some(_) => {}
        None => line.push_str("  (note: no ttrss_version table — is this a tt-rss database?)"),
    }
    Ok(line)
}

struct StreamOutcome {
    bytes: u64,
    dump_err: String,
    restore_err: String,
    dump_ok: bool,
    restore_ok: bool,
}

/// `pg_dump -Fc | pg_restore` — the whole import, with a live `\r` byte
/// counter on stderr.
async fn stream(s: &Source, cfg: &crate::core::config::Config) -> Result<StreamOutcome, String> {
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;
    use tokio::sync::mpsc;

    let mut dump = Command::new("pg_dump")
        .arg("--format=custom")
        .arg("--no-password")
        .arg("-h")
        .arg(&s.host)
        .arg("-p")
        .arg(s.port.to_string())
        .arg("-U")
        .arg(&s.user)
        .arg("-d")
        .arg(&s.db)
        .env("PGPASSWORD", &s.pass)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pg_dump not runnable: {e} (the image ships postgresql-client)"))?;
    let mut restore = Command::new("pg_restore")
        .arg("--clean")
        .arg("--if-exists")
        .arg("--no-owner")
        .arg("--no-acl")
        .arg("--exit-on-error")
        .arg("-h")
        .arg(&cfg.db_host)
        .arg("-p")
        .arg(cfg.db_port.to_string())
        .arg("-U")
        .arg(&cfg.db_user)
        .arg("-d")
        .arg(&cfg.db_name)
        .env("PGPASSWORD", &cfg.db_pass)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pg_restore not runnable: {e}"))?;

    let mut dump_out = dump.stdout.take().expect("piped");
    let mut dump_err_io = dump.stderr.take().expect("piped");
    let mut restore_in = restore.stdin.take().expect("piped");
    let mut restore_err_io = restore.stderr.take().expect("piped");

    let (tx, mut rx) = mpsc::channel::<(u64, u64)>(64);
    let started = std::time::Instant::now();

    // Pump the pipe, ticking (bytes, elapsed_secs) ~2/s.
    let pump = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut buf = [0u8; 1 << 16];
        let mut total = 0u64;
        let mut last_tick = std::time::Instant::now();
        loop {
            match dump_out.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    total += n as u64;
                    let _ = restore_in.write_all(&buf[..n]).await;
                    if last_tick.elapsed() > std::time::Duration::from_millis(500) {
                        let _ = tx.send((total, started.elapsed().as_secs())).await;
                        last_tick = std::time::Instant::now();
                    }
                }
                Err(_) => break,
            }
        }
        let _ = restore_in.shutdown().await;
        let _ = tx.send((total, started.elapsed().as_secs())).await;
        total
    });

    let pb = indicatif::ProgressBar::new_spinner();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb.set_message("importing… 0.0 MB");

    // Drive the counter from the ticks; the pump's completion ends the loop.
    let total = {
        let mut fut = Box::pin(pump);
        loop {
            tokio::select! {
                biased;
                t = rx.recv() => match t {
                    Some(t) => pb.set_message(format!("importing… {} MB · {}s", fmt_mb(t.0), t.1)),
                    None => break fut.await.unwrap_or(0),
                },
                r = &mut fut => break r.unwrap_or(0),
            }
        }
    };
    // Non-TTY: hidden bars draw nothing — the stdout success banner below
    // is the script-facing summary; no redundant stderr line.
    if pb.is_hidden() {
        pb.finish_and_clear();
    } else {
        pb.finish_with_message(format!("imported {} MB", fmt_mb(total)));
    }

    // Drain both stderrs (before wait — a full pipe would deadlock), then
    // collect statuses.
    let mut e = String::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = dump_err_io.read(&mut buf).await {
        if n == 0 {
            break;
        }
        e.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    let dump_err = e;
    let mut e = String::new();
    while let Ok(n) = restore_err_io.read(&mut buf).await {
        if n == 0 {
            break;
        }
        e.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    let restore_err = e;

    let dump_st = dump.wait().await.map_err(|e| e.to_string())?;
    let restore_st = restore.wait().await.map_err(|e| e.to_string())?;

    Ok(StreamOutcome {
        bytes: total,
        dump_err,
        restore_err,
        dump_ok: dump_st.success(),
        restore_ok: restore_st.success(),
    })
}

fn fmt_mb(b: u64) -> String {
    format!("{:.1}", b as f64 / (1024.0 * 1024.0))
}

/// Args parsed by clap in `main` (`ttrss importdb -y/--yes [--forget]`);
/// the long help text lives there too (`long_about`).
pub async fn run(yes: bool, do_forget: bool) -> i32 {
    let config = match crate::core::config::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ttrss importdb: config: {e:#}");
            return 1;
        }
    };
    // Same privilege posture as the server (data-dir ownership, 0600 file).
    let _ = crate::privs::drop_privileges(&config.data_dir, config.owner_uid, config.owner_gid);

    if do_forget {
        if forget(&config.data_dir) {
            println!("  remembered source forgotten");
        } else {
            println!("  nothing was remembered");
        }
        return 0;
    }

    println!();
    println!("  ttrss importdb — import your existing tt-rss data into this deployment");
    println!("  (copies from the other install; nothing over there is touched)");
    println!();

    let tty = dialoguer::console::user_attended();
    let theme = ColorfulTheme::default();
    let source = match resolve_source(yes, tty, &theme, &config.data_dir) {
        Some(s) => s,
        None => {
            eprintln!(
                "  no source — re-run interactively, or set IMPORTDB_HOST/PORT/DB/USER + IMPORTDB_PASS (or PGPASSWORD)"
            );
            return 1;
        }
    };

    match preflight(&source).await {
        Ok(line) => println!("{line}"),
        Err(e) => {
            eprintln!("  {e}");
            return 1;
        }
    }
    println!(
        "  target: {}:{}/{} (this deployment's database, user {})",
        config.db_host, config.db_port, config.db_name, config.db_user
    );

    let confirmed = yes || {
        let ok: bool = Confirm::with_theme(&theme)
            .with_prompt("This replaces the target's contents. Continue?")
            .default(false)
            .interact()
            .unwrap_or_default();
        ok
    };
    if !confirmed {
        println!("  cancelled — nothing changed");
        return 1;
    }

    let mut imported = Box::pin(stream(&source, &config));
    let outcome = tokio::select! {
        r = &mut imported => r,
        // The children die with the Ctrl-C too (same process group); just
        // tell the user the re-run story before exiting.
        _ = tokio::signal::ctrl_c() => {
            eprintln!();
            eprintln!("  interrupted — a partial import is fine to re-run (the restore is idempotent)");
            return 130;
        }
    };
    match outcome {
        Ok(o) if o.dump_ok && o.restore_ok => {
            let _ = remember(&config.data_dir, &source);
            println!();
            println!("  ✓ imported {} MB", fmt_mb(o.bytes));
            println!("  → restart the app to see it:  docker compose restart app");
            0
        }
        Ok(o) => {
            if !o.dump_ok {
                eprintln!("  pg_dump failed:\n{}", o.dump_err.trim());
            }
            if !o.restore_ok {
                eprintln!("  pg_restore failed:\n{}", o.restore_err.trim());
            }
            eprintln!("  (a partial import is fine to re-run — the restore is idempotent)");
            1
        }
        Err(e) => {
            eprintln!("  {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> Source {
        Source {
            host: "192.168.1.20".into(),
            port: 5432,
            db: "tt-rss".into(),
            user: "ttrss".into(),
            pass: "p@ss:word/x".into(),
        }
    }

    #[test]
    fn source_file_roundtrip() {
        let s = src();
        assert_eq!(parse_source_file(&source_file_lines(&s)), Some(s.clone()));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_source_file(""), None);
        assert_eq!(
            parse_source_file("host=x\nport=abc\ndb=y\nuser=z\npass=w\n"),
            None
        );
        assert_eq!(parse_source_file("host=x\nport=1\ndb=y\nuser=z\n"), None);
    }

    #[test]
    fn url_encodes_userinfo() {
        let s = src();
        assert_eq!(
            s.url(),
            "postgres://ttrss:p%40ss%3Aword%2Fx@192.168.1.20:5432/tt-rss?sslmode=prefer"
        );
    }
}
