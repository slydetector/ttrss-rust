//! The in-process first-boot bootstrap (docs/19 P0, docs/20 §4): the
//! user-management steps of the official image's `startup.sh`, ported into
//! the server's startup path — env-driven, idempotent, a no-op when the
//! env var is unset. Runs at every startup, after
//! [`crate::core::schema::ensure_up_to_date`] and before the daemon/listener
//! (fail-closed: an error exits before binding).
//!
//! The reference's sequence (`tt-rss/.docker/app/startup.sh`):
//!
//! ```sh
//! if [ ! -z "$ADMIN_USER_PASS" ]; then
//!     update.php --user-set-password "admin:$ADMIN_USER_PASS"
//! else
//!     if update.php --user-check-password "admin:password"; then
//!         RANDOM_PASS=…16 chars…; echo <star banner>;
//!         update.php --user-set-password "admin:$RANDOM_PASS"
//!     fi
//! fi
//! if [ ! -z "$ADMIN_USER_ACCESS_LEVEL" ]; then
//!     update.php --user-set-access-level "admin:$ADMIN_USER_ACCESS_LEVEL"
//! fi
//! if [ ! -z "$AUTO_CREATE_USER" ]; then
//!     update.php --user-exists $AUTO_CREATE_USER ||
//!         update.php --user-add "$USER:$PASS:$LEVEL"
//!     if [ ! -z "$AUTO_CREATE_USER_ENABLE_API" ]; then
//!         update.php --user-enable-api "$USER:$VAL" || true
//!     fi
//! fi
//! ```
//!
//! (The `VAR__FILE` secret expansion, `pg_isready` wait, chown and the
//! post-init SQL patches live in the entrypoint / [`run`] respectively.)

use crate::core::db::Db;
use crate::core::user;
use crate::users;
use rand::Rng;

/// The six official bootstrap env vars (exactly the reference's set; a var
/// is "set" when present and non-empty, mirroring `[ ! -z "$VAR" ]`).
#[derive(Debug, Clone, Default)]
pub struct BootstrapVars {
    pub admin_user_pass: Option<String>,
    pub admin_user_access_level: Option<String>,
    pub auto_create_user: Option<String>,
    pub auto_create_user_pass: String,
    pub auto_create_user_access_level: String,
    pub auto_create_user_enable_api: Option<String>,
}

impl BootstrapVars {
    pub fn from_env() -> Self {
        fn var(key: &str) -> Option<String> {
            std::env::var(key).ok().filter(|v| !v.is_empty())
        }
        Self {
            admin_user_pass: var("ADMIN_USER_PASS"),
            admin_user_access_level: var("ADMIN_USER_ACCESS_LEVEL"),
            auto_create_user: var("AUTO_CREATE_USER"),
            auto_create_user_pass: std::env::var("AUTO_CREATE_USER_PASS").unwrap_or_default(),
            auto_create_user_access_level: std::env::var("AUTO_CREATE_USER_ACCESS_LEVEL")
                .unwrap_or_default(),
            auto_create_user_enable_api: var("AUTO_CREATE_USER_ENABLE_API"),
        }
    }
}

/// What [`run`] did — for the startup log and the tests.
#[derive(Debug, Clone, Default)]
pub struct BootstrapReport {
    /// `ADMIN_USER_PASS` was applied to `admin`.
    pub admin_password_set: bool,
    /// The stock `password` was still set; this generated password replaced
    /// it (the star banner printed it).
    pub admin_password_randomized: Option<String>,
    /// `ADMIN_USER_ACCESS_LEVEL` was applied to `admin`.
    pub admin_level_set: bool,
    /// `AUTO_CREATE_USER` did not exist and was created.
    pub auto_user_created: bool,
    /// The resolved `AUTO_CREATE_USER_ENABLE_API` value (None = unset).
    pub auto_user_api: Option<bool>,
}

/// `update.php`'s `map_access_level`-validated level parse.
fn parse_access_level(raw: &str) -> Result<i32, anyhow::Error> {
    let level: i32 = raw
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Error: Invalid access level value: {raw}"))?;
    if !users::is_valid_access_level(level) {
        anyhow::bail!("Error: Invalid access level value: {raw}");
    }
    Ok(level)
}

/// `tr -dc A-Za-z0-9 </dev/urandom | head -c 16` — 16 random alphanumeric
/// chars.
fn random_password(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    rand::thread_rng()
        .sample_iter(rand::distributions::Uniform::new(0, CHARSET.len()))
        .take(len)
        .map(|i| CHARSET[i] as char)
        .collect()
}

/// The reference's star banner, verbatim (76 asterisks, its own ragged
/// trailing-star spacing included).
fn star_banner(password: &str) -> String {
    const STARS: &str = "************************************************************************";
    format!(
        "{STARS}\n\
         * Setting initial built-in admin user password to '{password}'        *\n\
         * If you want to set it manually, use ADMIN_USER_PASS environment variable. *\n\
         {STARS}"
    )
}

/// Run the bootstrap sequence on `db`. Idempotent: every step is a no-op
/// when its env var is unset / its precondition unmet.
pub async fn run(db: &Db, vars: &BootstrapVars) -> Result<BootstrapReport, anyhow::Error> {
    let pool = db.pool();
    let mut report = BootstrapReport::default();

    // ── admin password policy ──────────────────────────────────────────────
    if let Some(pass) = &vars.admin_user_pass {
        // Applied at every boot (the reference does not check for a change).
        if !users::user_set_password(pool, "admin", pass).await? {
            anyhow::bail!("Error: User not found: admin");
        }
        report.admin_password_set = true;
        tracing::info!("bootstrap: applied ADMIN_USER_PASS to admin");
    } else if users::user_check_password(pool, "admin", "password").await? {
        // The stock `password` is still set → randomize + banner (the
        // reference's first-boot nudge; printed to stderr like its echo).
        let new_pass = random_password(16);
        eprintln!("{}", star_banner(&new_pass));
        if !users::user_set_password(pool, "admin", &new_pass).await? {
            anyhow::bail!("Error: User not found: admin");
        }
        report.admin_password_randomized = Some(new_pass);
    }

    // ── admin access level ─────────────────────────────────────────────────
    if let Some(raw) = &vars.admin_user_access_level {
        let level = parse_access_level(raw)?;
        if !users::user_set_access_level(pool, "admin", level).await? {
            anyhow::bail!("Error: User not found: admin");
        }
        report.admin_level_set = true;
        tracing::info!(level, "bootstrap: applied ADMIN_USER_ACCESS_LEVEL to admin");
    }

    // ── auto-create user ───────────────────────────────────────────────────
    if let Some(login) = &vars.auto_create_user {
        if !users::user_exists(pool, login).await? {
            // `update.php` user-add: `if (!$access_level) → USER (0)`.
            let level = if vars.auto_create_user_access_level.is_empty()
                || vars.auto_create_user_access_level == "0"
            {
                user::ACCESS_LEVEL_USER
            } else {
                parse_access_level(&vars.auto_create_user_access_level)?
            };
            // PHP prompts on stdin for a blank password; the in-process
            // equivalent of an empty read is its "may not be blank" error.
            if vars.auto_create_user_pass.is_empty() {
                anyhow::bail!("Error: password may not be blank.");
            }
            users::user_add(pool, login, &vars.auto_create_user_pass, level).await?;
            report.auto_user_created = true;
            tracing::info!(login, level, "bootstrap: created AUTO_CREATE_USER");
        }
        if let Some(raw) = &vars.auto_create_user_enable_api {
            // The reference tolerates this one failing (`|| true`).
            let enable = crate::core::config::sql_bool_to_bool(raw);
            let _ = users::user_enable_api(db, login, enable).await;
            report.auto_user_api = Some(enable);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_banner_is_verbatim() {
        let b = star_banner("abc");
        let mut lines = b.lines();
        let stars = "************************************************************************";
        assert_eq!(lines.next().unwrap(), stars);
        assert_eq!(
            lines.next().unwrap(),
            "* Setting initial built-in admin user password to 'abc'        *"
        );
        assert_eq!(
            lines.next().unwrap(),
            "* If you want to set it manually, use ADMIN_USER_PASS environment variable. *"
        );
        assert_eq!(lines.next().unwrap(), stars);
        assert!(lines.next().is_none());
    }

    #[test]
    fn random_password_shape() {
        let p = random_password(16);
        assert_eq!(p.len(), 16);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        // No two draws collide (astronomically unlikely otherwise).
        assert_ne!(p, random_password(16));
    }

    #[test]
    fn parse_access_level_validates() {
        assert_eq!(parse_access_level("10").unwrap(), 10);
        assert_eq!(parse_access_level("-2").unwrap(), -2);
        assert_eq!(parse_access_level("0").unwrap(), 0);
        // Not a real level (map_access_level → KEEP_CURRENT).
        assert!(parse_access_level("7").is_err());
        // Not an integer.
        assert!(parse_access_level("admin").is_err());
        let e = parse_access_level("7").unwrap_err().to_string();
        assert!(e.contains("Invalid access level value: 7"), "{e}");
    }

    #[test]
    fn vars_from_env_reads_official_names() {
        for (key, val) in [
            ("ADMIN_USER_PASS", "p"),
            ("ADMIN_USER_ACCESS_LEVEL", "10"),
            ("AUTO_CREATE_USER", "u"),
            ("AUTO_CREATE_USER_PASS", "pw"),
            ("AUTO_CREATE_USER_ACCESS_LEVEL", "5"),
            ("AUTO_CREATE_USER_ENABLE_API", "true"),
        ] {
            unsafe { std::env::set_var(key, val) };
        }
        let v = BootstrapVars::from_env();
        assert_eq!(v.admin_user_pass.as_deref(), Some("p"));
        assert_eq!(v.admin_user_access_level.as_deref(), Some("10"));
        assert_eq!(v.auto_create_user.as_deref(), Some("u"));
        assert_eq!(v.auto_create_user_pass, "pw");
        assert_eq!(v.auto_create_user_access_level, "5");
        assert_eq!(v.auto_create_user_enable_api.as_deref(), Some("true"));
        for key in [
            "ADMIN_USER_PASS",
            "ADMIN_USER_ACCESS_LEVEL",
            "AUTO_CREATE_USER",
            "AUTO_CREATE_USER_PASS",
            "AUTO_CREATE_USER_ACCESS_LEVEL",
            "AUTO_CREATE_USER_ENABLE_API",
        ] {
            unsafe { std::env::remove_var(key) };
        }
        // Empty string == unset (the reference's `[ ! -z ]`).
        unsafe { std::env::set_var("ADMIN_USER_PASS", "") };
        assert_eq!(BootstrapVars::from_env().admin_user_pass, None);
        unsafe { std::env::remove_var("ADMIN_USER_PASS") };
    }
}
