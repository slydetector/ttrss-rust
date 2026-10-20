//! `ttrss importdb` — the end-to-end import path: two databases cloned from
//! the shared template (a "source" install with a marker row, a "target"),
//! then the real binary (`CARGO_BIN_EXE_ttrss importdb --yes`) run with env
//! pointed at both. Asserts the marker lands in the target and the source
//! is remembered for one-keypress re-runs.

use sqlx::AssertSqlSafe;
use sqlx::Connection;
use test_db::master_url;
use url::Url;

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Clone the shared template into a fresh database; returns (name, url).
async fn clone_template(master: &str) -> (String, String) {
    let u: Url = master.parse().expect("master url");
    let name = format!(
        "_ttrss_impdb_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let mut conn = sqlx::postgres::PgConnection::connect(master)
        .await
        .expect("connect master");
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE {name} TEMPLATE _ttrss_tpl"
    )))
    .execute(&mut conn)
    .await
    .expect("clone template");
    let mut t = u.clone();
    t.set_path(&format!("/{name}"));
    (name, t.to_string())
}

async fn drop_db(master: &str, name: &str) {
    let mut conn = match sqlx::postgres::PgConnection::connect(master).await {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = sqlx::query(AssertSqlSafe(format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{name}'"
    )))
    .execute(&mut conn)
    .await;
    let _ = sqlx::query(AssertSqlSafe(format!("DROP DATABASE IF EXISTS {name}")))
        .execute(&mut conn)
        .await;
}

/// Run the built binary `importdb --yes` against (source, target) databases.
async fn run_importdb(data_dir: &std::path::Path, src: &str, tgt: &str) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ttrss");
    let parse = |u: &str| -> (String, u16) {
        let pu: Url = u.parse().unwrap();
        (
            pu.host_str().unwrap().to_string(),
            pu.port().unwrap_or(5432),
        )
    };
    let (sh, sp) = parse(src);
    let (th, tp) = parse(tgt);
    let db_of = |u: &str| {
        Url::parse(u)
            .unwrap()
            .path()
            .trim_start_matches('/')
            .to_string()
    };
    let tgt_db = db_of(tgt);
    let src_db = db_of(src);

    tokio::process::Command::new(bin)
        .arg("importdb")
        .arg("--yes")
        .env("TTRSS_DB_HOST", &th)
        .env("TTRSS_DB_PORT", tp.to_string())
        .env("TTRSS_DB_NAME", &tgt_db)
        .env("TTRSS_DB_USER", "postgres")
        .env("TTRSS_DB_PASS", "postgres")
        .env("IMPORTDB_HOST", &sh)
        .env("IMPORTDB_DB", &src_db)
        .env("IMPORTDB_PORT", sp.to_string())
        .env("IMPORTDB_USER", "postgres")
        .env("IMPORTDB_PASS", "postgres")
        .env("TTRSS_DATA_DIR", data_dir)
        .env("OWNER_UID", unsafe { libc::getuid() }.to_string())
        .env("OWNER_GID", unsafe { libc::getgid() }.to_string())
        .env("RUST_LOG", "warn")
        .output()
        .await
        .expect("spawn ttrss importdb")
}

#[tokio::test]
async fn test_importdb_imports_and_remembers() {
    let master = master_url();
    let (src_name, src_url) = clone_template(&master).await;
    let (tgt_name, tgt_url) = clone_template(&master).await;
    let data = tempfile::tempdir().expect("data dir");

    // Distinctive marker row in the source only.
    {
        let mut conn = sqlx::postgres::PgConnection::connect(&src_url)
            .await
            .expect("connect source");
        sqlx::query(
            "INSERT INTO ttrss_feeds (feed_url, title, owner_uid, site_url)
             VALUES ('https://importdb.marker.example/atom.xml', 'IMPORTDB MARKER', 1,
                     'https://importdb.marker.example')",
        )
        .execute(&mut conn)
        .await
        .expect("insert marker");
    }

    // First import.
    let out = run_importdb(data.path(), &src_url, &tgt_url).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "importdb failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    // Non-TTY: the hidden spinner draws nothing; the stdout success banner
    // is the script-facing summary.
    assert!(
        stdout.contains("imported") && stdout.contains(" MB"),
        "success banner missing from stdout:\n{stdout}"
    );

    // The marker landed in the target.
    let mut conn = sqlx::postgres::PgConnection::connect(&tgt_url)
        .await
        .expect("connect target");
    let marker: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ttrss_feeds WHERE title = 'IMPORTDB MARKER'")
            .fetch_one(&mut conn)
            .await
            .expect("count marker");
    assert_eq!(marker, 1, "marker row missing in target");
    let _ = conn.close().await;

    // The source was remembered (0600, inspectable format).
    let remembered = std::fs::read_to_string(data.path().join("importdb-source"))
        .expect("remembered source file");
    assert!(remembered.contains("host="));
    assert!(remembered.contains(&format!("db={src_name}")));
    assert!(remembered.contains("user=postgres"));

    // Re-run over an already-imported target: idempotent, clean exit.
    let out2 = run_importdb(data.path(), &src_url, &tgt_url).await;
    assert!(
        out2.status.success(),
        "re-import failed: {}\n{}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );

    drop_db(&master, &src_name).await;
    drop_db(&master, &tgt_name).await;
}
