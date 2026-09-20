//! Test-parity: 1:1 ports of the frozen PHP `SanitizerTest` suites
//! (commit 2f7d831):
//!
//! - `tests/mocked/SanitizerUnitTest.php`        — 81 tests (mocked deps)
//! - `tests_functional/SanitizerTest.php`        — 11 tests (highlight)
//! - `tests/integration/SanitizerTest.php`       — 2 tests
//! - `tests_integration/SanitizerTest.php`       — 162 tests (real DB)
//!
//! Ground truth: PHP 8.5.9 / libxml 2.13.09 oracle (see `oracle/`). 253 of the
//! 256 PHP cases pass on the oracle; the 3 that fail there (test-side bugs)
//! are ported with the *corrected* expectation and a NOTE comment. Two more
//! cases (libxml numeric character-reference escaping) are ported with the
//! semantically-equivalent raw-UTF-8 expectation (NOTE comments).
//!
//! PHP session defaults used by the suites:
//! - mocked: `$_SESSION = []` (no uid) → owner null, hasSandbox false.
//!   `Prefs::get` (mock) → false; `Config::get_self_url` → http (not https).
//! - tests_integration: `$_SESSION['uid'] = 1`, `hasSandbox = false`,
//!   `STRIP_IMAGES` pref off in the DB (unless a test enables it).

#[cfg(test)]
mod functional;
#[cfg(test)]
mod integration;
#[cfg(test)]
mod mocked;
#[cfg(test)]
mod unit;

use crate::sanitizer::sanitize_core;

/// `Sanitizer::sanitize($html)` with empty session state: force=false,
/// strip_images=false, has_sandbox=false, bw_limit=false, no site URL, no
/// highlight words. (The Rust `site_url: None` maps to PHP's default base
/// `http://domain.invalid/`.)
pub(crate) fn san(html: &str) -> String {
    sanitize_core(html, false, false, false, false, None, None, None)
}

/// `Sanitizer::sanitize($html, false, null, $site_url)`.
pub(crate) fn san_site(html: &str, site: &str) -> String {
    sanitize_core(html, false, false, false, false, Some(site), None, None)
}

/// `Sanitizer::sanitize($html, false, 1, null, $words)` (tests_integration
/// section K; STRIP_IMAGES pref is off for uid 1 in the DB).
pub(crate) fn san_hw(html: &str, words: &[&str]) -> String {
    let ws: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    sanitize_core(html, false, false, false, false, None, Some(&ws), None)
}

/// `$_SESSION['hasSandbox'] = true` variant.
pub(crate) fn san_sandbox(html: &str) -> String {
    sanitize_core(html, false, false, true, false, None, None, None)
}

/// `Prefs::set(STRIP_IMAGES, true, 1, null)` + session uid 1.
pub(crate) fn san_strip(html: &str) -> String {
    sanitize_core(html, false, true, false, false, None, None, None)
}

/// `Sanitizer::sanitize($html, true)` — force_remove_images.
pub(crate) fn san_force(html: &str) -> String {
    sanitize_core(html, true, false, false, false, None, None, None)
}

#[cfg(test)]
pub(crate) fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}
