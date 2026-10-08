//! Scenario executor: performs the HTTP exchanges and records fixtures.

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde_json::json;
use sqlx::AssertSqlSafe;
use sqlx::Row;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::scenario::{SaveSpec, ScenarioFile, Step, substitute};

pub async fn run(
    cli_base_url: &Option<String>,
    scenarios: &Path,
    out: &Path,
    vars_file: Option<&Path>,
) -> Result<()> {
    let raw = std::fs::read_to_string(scenarios)
        .with_context(|| format!("reading {}", scenarios.display()))?;
    let file: ScenarioFile =
        toml::from_str(&raw).with_context(|| format!("parsing {}", scenarios.display()))?;

    let base_url = cli_base_url
        .clone()
        .or(file.base_url.clone())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("no base URL (set --base-url or base_url in scenario file)"))?;
    let base_url = base_url.trim_end_matches('/').to_string();

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    if let Some(f) = vars_file {
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?,
        )?;
        flatten_vars(&v, "", &mut vars);
    }
    vars.insert("base".to_string(), base_url.clone());

    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    // Clear stale fixtures: step numbering shifts when scenarios change, and a
    // leftover `NNN_*.json` from an older run would otherwise poison the diff.
    for entry in std::fs::read_dir(out).with_context(|| format!("reading {}", out.display()))? {
        let p = entry?.path();
        if p.is_file() && p.extension().is_some_and(|e| e == "json") {
            std::fs::remove_file(&p)
                .with_context(|| format!("removing stale fixture {}", p.display()))?;
        }
    }

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .build()?;

    // Optional shared-DB handle for M3 `db_state` capture (set `TTRSS_HARNESS_DB`).
    let db_pool: Option<sqlx::PgPool> = match std::env::var("TTRSS_HARNESS_DB") {
        Ok(url) if !url.is_empty() => Some(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .with_context(|| format!("connecting to harness DB {url}"))?,
        ),
        _ => None,
    };

    let mut failures = 0usize;
    for (i, step) in file.steps.iter().enumerate() {
        if step.skip {
            println!("[run] {:03} {:<32} skipped", i + 1, step.name);
            continue;
        }
        if step.sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(step.sleep_ms)).await;
        }
        let result = run_step(
            &client,
            &base_url,
            step,
            &mut vars,
            i,
            out,
            db_pool.as_ref(),
        )
        .await;
        match result {
            Ok(()) => {}
            Err(e) => {
                failures += 1;
                eprintln!("[run] step {:03} {}: ERROR: {e:#}", i + 1, step.name);
            }
        }
        if file.sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(file.sleep_ms)).await;
        }
    }

    if failures > 0 {
        anyhow::bail!(
            "{failures} step(s) failed; fixtures still written to {}",
            out.display()
        );
    }
    let executed = file.steps.iter().filter(|s| !s.skip).count();
    println!(
        "[run] ok: {} exchanges recorded to {} ({} of {} steps skipped)",
        executed,
        out.display(),
        file.steps.len() - executed,
        file.steps.len()
    );
    Ok(())
}

/// Flatten a JSON object into dotted-key string variables.
/// ({"categories": {"tech_news": 3}}) -> ("categories.tech_news", "3").
/// Recursively substitute {{...}} placeholders in all string values of a JSON
/// body.
fn substitute_json(v: &mut serde_json::Value, vars: &BTreeMap<String, String>) -> Result<()> {
    match v {
        serde_json::Value::String(s) => {
            *s = substitute(s, vars)?;
        }
        serde_json::Value::Object(map) => {
            for val in map.values_mut() {
                substitute_json(val, vars)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for val in arr.iter_mut() {
                substitute_json(val, vars)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn flatten_vars(v: &serde_json::Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_vars(val, &key, out);
            }
        }
        serde_json::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        serde_json::Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        serde_json::Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        _ => {}
    }
}

/// Run a step's `db_state` SELECT (if any) and return its rows as a JSON array
/// of arrays (one per row, one value per column).
async fn capture_db_state(
    pool: Option<&sqlx::PgPool>,
    query: Option<&str>,
) -> Result<Option<serde_json::Value>> {
    let (Some(pool), Some(q)) = (pool, query) else {
        return Ok(None);
    };
    let rows = sqlx::query(AssertSqlSafe(q)).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let ncols = row.columns().len();
        let mut cells = Vec::with_capacity(ncols);
        for i in 0..ncols {
            cells.push(decode_cell(row, i));
        }
        out.push(serde_json::Value::Array(cells));
    }
    Ok(Some(serde_json::Value::Array(out)))
}

/// Best-effort decode of one cell to a JSON value (int → i64, bool, float, text).
fn decode_cell(row: &sqlx::postgres::PgRow, i: usize) -> serde_json::Value {
    if let Ok(v) = row.try_get::<i32, _>(i) {
        return serde_json::Value::from(v as i64);
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return serde_json::Value::from(v);
    }
    if let Ok(v) = row.try_get::<bool, _>(i) {
        return serde_json::Value::from(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(i) {
        return v
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null);
    }
    serde_json::Value::Null
}

async fn run_step(
    client: &reqwest::Client,
    base_url: &str,
    step: &Step,
    vars: &mut BTreeMap<String, String>,
    seq: usize,
    out: &Path,
    db_pool: Option<&sqlx::PgPool>,
) -> Result<()> {
    let method = step.method.to_ascii_uppercase();
    let url = format!("{}{}", base_url, substitute(&step.path, vars)?);

    let mut req = match method.as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        other => return Err(anyhow!("unsupported method {other}")),
    };

    if !step.query.is_empty() {
        let pairs = step
            .query
            .iter()
            .map(|(k, v)| Ok((k.clone(), substitute(v, vars)?)))
            .collect::<Result<Vec<_>>>()?;
        req = req.query(&pairs);
    }

    if !step.form.is_empty() {
        let pairs = step
            .form
            .iter()
            .map(|(k, v)| Ok((k.clone(), substitute(v, vars)?)))
            .collect::<Result<Vec<_>>>()?;
        req = req.form(&pairs);
    }
    if let Some(j) = &step.json {
        let mut body = j.clone();
        // Substitute {{...}} placeholders in all string values.
        substitute_json(&mut body, vars)?;
        // Auto-inject API session id for subsequent API calls.
        if step.path.starts_with("/api/index.php")
            && body.get("sid").is_none()
            && let Some(sid) = vars.get("api_sid")
            && let serde_json::Value::Object(map) = &mut body
        {
            map.insert("sid".to_string(), json!(sid));
        }
        req = req.json(&body);
    }
    for (k, v) in &step.headers {
        req = req.header(k, substitute(v, vars)?);
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("sending {method} {url}"))?;
    let status = resp.status().as_u16();
    // Capture the resulting DB state (M3 mutations) if this step declares it.
    let db_state = capture_db_state(db_pool, step.db_state.as_deref()).await?;
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                v.to_str().unwrap_or("<non-utf8>").to_string(),
            )
        })
        .collect();
    let bytes = resp.bytes().await?;
    let body_str = String::from_utf8_lossy(&bytes).to_string();

    // Save specs.
    let json_body = if body_str.trim_start().starts_with('{') {
        serde_json::from_str::<serde_json::Value>(&body_str).ok()
    } else {
        None
    };
    for (name, spec) in &step.save {
        let spec = SaveSpec::parse(spec)?;
        match spec.apply(&body_str, json_body.as_ref(), &headers) {
            Some(v) => {
                vars.insert(name.clone(), v);
            }
            None => {
                eprintln!(
                    "[run] step {}: warning: save spec {name:?} matched nothing",
                    step.name
                );
            }
        }
    }

    if let Some(expected) = step.expect_status
        && status != expected
    {
        eprintln!(
            "[run] step {}: expected status {expected}, got {status}",
            step.name
        );
        // record anyway; treat as failure
        write_exchange(
            out,
            seq,
            step,
            &url,
            &headers,
            status,
            &bytes,
            db_state.as_ref(),
        )?;
        return Err(anyhow!("status mismatch"));
    }

    write_exchange(
        out,
        seq,
        step,
        &url,
        &headers,
        status,
        &bytes,
        db_state.as_ref(),
    )?;
    println!("[run] {:03} {:<32} {status}", seq + 1, step.name);
    Ok(())
}

#[derive(Serialize)]
struct Exchange {
    seq: usize,
    step: String,
    method: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    form: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_b64: Option<String>,
    body_encoding: String,
    status: u16,
    headers: Vec<(String, String)>,
    body_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    db_state: Option<serde_json::Value>,
}

#[allow(clippy::too_many_arguments)] // the exchange has this many facets
fn write_exchange(
    out: &Path,
    seq: usize,
    step: &Step,
    url: &str,
    headers: &[(String, String)],
    status: u16,
    resp_bytes: &[u8],
    db_state: Option<&serde_json::Value>,
) -> Result<()> {
    let method = step.method.to_ascii_uppercase();
    // Binary detection: invalid UTF-8 or image/* content type.
    let is_binary = std::str::from_utf8(resp_bytes).is_err()
        || headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.starts_with("image/"));
    let body_text = if is_binary {
        None
    } else {
        Some(String::from_utf8_lossy(resp_bytes).to_string())
    };
    let body_b64 = if is_binary {
        Some(base64_encode(resp_bytes))
    } else {
        None
    };

    let exchange = Exchange {
        seq,
        step: step.name.clone(),
        method: method.to_string(),
        url: url.to_string(),
        form: if step.form.is_empty() {
            None
        } else {
            Some(step.form.clone())
        },
        body_b64,
        body_encoding: if is_binary {
            "binary".into()
        } else {
            "text".into()
        },
        status,
        headers: headers.to_vec(),
        body_text,
        db_state: db_state.cloned(),
    };

    let filename = format!("{:03}_{}.json", seq + 1, sanitize_name(&step.name));
    let path = out.join(&filename);
    let mut pretty = serde_json::to_string_pretty(&exchange)?;
    pretty.push('\n');
    std::fs::write(&path, pretty).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn base64_encode(data: &[u8]) -> String {
    // Small dependency-free base64 (RFC 4648).
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            CHARS[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
