//! Scenario file format (TOML).
//!
//! ```toml
//! base_url = "http://127.0.0.1:8280/tt-rss"
//!
//! [[step]]
//! name = "healthcheck"
//! path = "/public.php"
//! query = [ ["op", "healthcheck"] ]
//!
//! [[step]]
//! name = "shell"
//! path = "/index.php"
//! save = { csrf = '''regex:const __csrf_token = "([0-9a-f]{64})"''' }
//!
//! [[step]]
//! name = "login"
//! method = "POST"
//! path = "/public.php"
//! form = [ ["op", "login"], ["login", "admin"], ["password", "admin"],
//!          ["csrf_token", "{{csrf}}"], ["return", "{{base}}/index.php"] ]
//! expect_status = 302
//!
//! [[step]]
//! name = "api-login"
//! method = "POST"
//! path = "/api/index.php"
//! json = { op = "login", user = "admin", password = "admin" }
//! save = { api_sid = "json:$.content.session_id" }
//! ```
//!
//! - `query`/`form`/`headers`: arrays of [key, value] pairs (order preserved,
//!   duplicates allowed).
//! - `save`: map of variable name -> spec. Spec is `regex:<pattern>` (first
//!   match, group 1 if present, else whole match) or `json:<dotted.path>`
//!   against the JSON response body (arrays by index: `content.0.id`).
//! - `{{var}}` substitution in path/query/form/header values; variables come
//!   from `--vars` JSON and from earlier `save` specs. `{{base}}` is always
//!   the base URL.
//! - Steps whose `path` starts with `/api/index.php` and have a `json` body
//!   get `sid` injected from the `api_sid` variable (if set and not already
//!   present in the body).

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct ScenarioFile {
    pub base_url: Option<String>,
    #[serde(default)]
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
    /// Default sleep between steps (ms), 0 = none
    #[serde(default)]
    pub sleep_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    pub name: String,
    /// URL path, e.g. "/backend.php"
    pub path: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub query: Vec<(String, String)>,
    #[serde(default)]
    pub form: Vec<(String, String)>,
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub save: BTreeMap<String, String>,
    #[serde(default)]
    pub expect_status: Option<u16>,
    /// Extra sleep before this step (ms)
    #[serde(default)]
    pub sleep_ms: u64,
    /// Optional SQL `SELECT` run against the shared DB after this step; its rows
    /// are captured into the exchange as `db_state` (M3: resulting DB state).
    #[serde(default)]
    pub db_state: Option<String>,
    /// Documentation only; not used by the harness.
    #[serde(default)]
    #[allow(dead_code)]
    pub description: Option<String>,
    /// Skipped steps are not executed and write no fixture, but they keep
    /// their position so the `NNN_` fixture numbering stays aligned across
    /// runs and baselines.
    #[serde(default)]
    pub skip: bool,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, Clone)]
pub enum SaveSpec {
    Regex(regex::Regex),
    JsonPath(Vec<String>),
    Header(String),
}

impl SaveSpec {
    pub fn parse(spec: &str) -> Result<Self> {
        if let Some(name) = spec.strip_prefix("header:") {
            Ok(Self::Header(name.trim().to_string()))
        } else if let Some(pattern) = spec.strip_prefix("regex:") {
            Ok(Self::Regex(regex::Regex::new(pattern)
                .map_err(|e| anyhow!("bad regex {pattern:?}: {e}"))?))
        } else if let Some(path) = spec.strip_prefix("json:") {
            let parts: Vec<String> = path
                .trim_start_matches('$')
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                return Err(anyhow!("empty json path in {spec:?}"));
            }
            Ok(Self::JsonPath(parts))
        } else {
            Err(anyhow!(
                "save spec must start with 'regex:' or 'json:': {spec:?}"
            ))
        }
    }

    /// Apply to a response, returning the captured value (if any).
    pub fn apply(
        &self,
        body: &str,
        json: Option<&serde_json::Value>,
        headers: &[(String, String)],
    ) -> Option<String> {
        match self {
            Self::Regex(re) => re.captures(body).and_then(|caps| {
                caps.get(1)
                    .or_else(|| caps.get(0))
                    .map(|m| m.as_str().to_string())
            }),
            Self::JsonPath(parts) => json.and_then(|v| {
                let mut cur = v;
                for p in parts {
                    match cur {
                        serde_json::Value::Object(map) => cur = map.get(p)?,
                        serde_json::Value::Array(arr) => {
                            let idx: usize = p.parse().ok()?;
                            cur = arr.get(idx)?;
                        }
                        _ => return None,
                    }
                }
                match cur {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                }
            }),
            Self::Header(name) => headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone()),
        }
    }
}

/// Substitute {{var}} placeholders; error on unknown variables.
///
/// Nesting-aware: {{totp:{{totp_secret}}}} resolves the inner variable first,
/// then treats the result as `totp:<base32-secret>` and computes a live TOTP
/// code (so the corpus can pass OTP login challenges).
pub fn substitute(input: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while let Some(open) = input[i..].find("{{") {
        out.push_str(&input[i..i + open]);
        let start = i + open + 2;
        // find the matching close, accounting for nested {{ }}
        let mut depth = 1usize;
        let mut j = start;
        let mut close: Option<usize> = None;
        while j < input.len() && depth > 0 {
            if input[j..].starts_with("{{") {
                depth += 1;
                j += 2;
            } else if input[j..].starts_with("}}") {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    close = Some(j);
                }
            } else {
                j += 1;
            }
        }
        let end = close.ok_or_else(|| anyhow!("unterminated {{ in {input:?}"))?;
        let name = input[start..end - 2].trim();
        out.push_str(&resolve_var(name, vars)?);
        i = end;
    }
    out.push_str(&input[i..]);
    Ok(out)
}

fn resolve_var(name: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    if let Some(v) = vars.get(name) {
        return Ok(v.clone());
    }
    if let Some(rest) = name.strip_prefix("totp:") {
        let secret = if rest.contains("{{") {
            substitute(rest, vars)?
        } else {
            rest.trim().to_string()
        };
        return Ok(crate::totp::totp(&secret));
    }
    Err(anyhow!(
        "unknown variable {name:?} (check save specs / --vars)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn nested_totp_substitution() {
        let mut vars = BTreeMap::new();
        vars.insert("totp_secret".to_string(), "JBSWY3DPEHPK3PXP".to_string());
        let out = substitute("{{totp:{{totp_secret}}}}", &vars).unwrap();
        assert_eq!(out.len(), 6);
        assert!(out.chars().all(|c| c.is_ascii_digit()));
        // literal secret form works too
        let out2 = substitute("{{totp:JBSWY3DPEHPK3PXP}}", &vars).unwrap();
        assert_eq!(out, out2);
    }

    #[test]
    fn plain_substitution() {
        let mut vars = BTreeMap::new();
        vars.insert("a.b".to_string(), "42".to_string());
        assert_eq!(substitute("x{{a.b}}y", &vars).unwrap(), "x42y");
    }
}
