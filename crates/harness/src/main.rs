//! tt-rust-harness: oracle-parity record/diff harness.
//!
//! Subcommands:
//!   run  -- execute a scenario file against a base URL, record every HTTP
//!            exchange to a run directory (fixtures).
//!   diff -- structurally compare two run directories with non-determinism
//!            normalization.
//!
//! Design notes:
//! - Traffic is defined once (scenario TOML) and re-executed per run, so
//!   volatile state (session cookies, CSRF tokens, API session ids) is always
//!   re-derived live. Fixtures are *outputs* of a run, not inputs to a replay.
//! - Each exchange is saved as `<seq:03>_<scenario>_<step>.json`.
//! - diff pairs files by name, normalizes volatile values, and reports
//!   structural differences.

mod diff;
mod html;
mod normalize;
mod run;
mod scenario;
mod totp;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tt-rust-harness", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute scenarios against a base URL and record exchanges
    Run {
        /// Base URL, e.g. http://127.0.0.1:8280/tt-rss
        #[arg(long, global = true)]
        base_url: Option<String>,

        /// Scenario file (TOML)
        #[arg(long)]
        scenarios: std::path::PathBuf,

        /// Output directory for this run's fixtures
        #[arg(long)]
        out: std::path::PathBuf,

        /// Optional JSON file of pre-seeded variables ({"name": "value"})
        #[arg(long)]
        vars: Option<std::path::PathBuf>,
    },
    /// Compare two run directories
    Diff {
        /// Run directory A (e.g. oracle)
        #[arg(long)]
        a: std::path::PathBuf,

        /// Run directory B (e.g. candidate)
        #[arg(long)]
        b: std::path::PathBuf,

        /// Max diff details to print per exchange
        #[arg(long, default_value_t = 10)]
        max_details: usize,

        /// Tolerate up to this many mismatched exchanges (exit 0)
        #[arg(long, default_value_t = 0)]
        allow_mismatches: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            base_url,
            scenarios,
            out,
            vars,
        } => run::run(&base_url, &scenarios, &out, vars.as_deref()).await,
        Command::Diff {
            a,
            b,
            max_details,
            allow_mismatches,
        } => diff::diff(&a, &b, max_details, allow_mismatches),
    }
}
