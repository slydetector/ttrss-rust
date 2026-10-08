//! Privilege handling for the container image (docs/19): the process starts
//! as root (needed for the data-dir `chown`), then drops to the configured
//! `OWNER_UID`/`OWNER_GID` (the official image's `app` user, default 1000)
//! and serves entirely as that user.
//!
//! The drop is the standard Unix sequence — `setgroups(0)` → `setgid` →
//! `setuid` — each syscall checked. It is irreversible: once the real UID is
//! non-root, the kernel removes `CAP_SETUID`, so the process can never gain
//! root back. Non-root invocations (dev, `cargo test`, `docker run --user`)
//! skip everything and run as-is.
//!
//! Call this before any I/O and before the tokio runtime exists: Linux
//! credentials are per-thread and new threads inherit the caller's
//! credentials, so every runtime worker is *born* the target UID.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Root-only setup + privilege drop (see the module docs).
///
/// No-op when `geteuid() != 0` (no root, nothing to drop) or `owner_uid == 0`
/// (explicit opt-out: run everything as root).
pub fn drop_privileges(data_dir: &Path, owner_uid: u32, owner_gid: u32) -> io::Result<()> {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        tracing::info!(
            euid,
            "not running as root; skipping chown and privilege drop"
        );
        return Ok(());
    }
    if owner_uid == 0 {
        tracing::info!("OWNER_UID=0; running as root without dropping privileges");
        return Ok(());
    }

    // Top-level chown only (the fresh-volume contract: the app writes
    // everything below as the app user, so a recursive sweep buys nothing —
    // the official's `chown -R` parity is documented as intentionally weaker).
    if data_dir.exists() {
        chown(data_dir, owner_uid, owner_gid)?;
        tracing::info!(
            dir = %data_dir.display(),
            uid = owner_uid,
            gid = owner_gid,
            "chowned data dir"
        );
    }

    // setgroups must come before setuid (it requires root); setgid before
    // setuid. Order is load-bearing.
    let rc = unsafe { libc::setgroups(0, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::setgid(owner_gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::setuid(owner_uid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    tracing::info!(
        uid = owner_uid,
        gid = owner_gid,
        "dropped privileges (irreversible)"
    );
    Ok(())
}

fn chown(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    let bytes = path.as_os_str().as_bytes();
    let cstr = std::ffi::CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "data dir path contains NUL"))?;
    let rc = unsafe { libc::chown(cstr.as_ptr(), uid, gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The non-root skip path is the only one CI exercises (tests run as the
    // invoking user, euid != 0). It must succeed and leave the process
    // untouched.
    #[test]
    fn non_root_is_a_noop() {
        let before = unsafe { libc::geteuid() };
        if before == 0 {
            eprintln!("skipping: running as root");
            return;
        }
        assert!(drop_privileges(std::path::Path::new("data"), 1000, 1000).is_ok());
        // credentials untouched
        assert_eq!(before, unsafe { libc::geteuid() });
    }
}
