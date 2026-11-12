//! `gsc` — Google Search Console from the command line.
//!
//! The binary in `main.rs` is a thin wrapper around [`run`]; everything else
//! lives in this library so it can be unit-tested and reused.

pub mod cli;
pub mod error;

use std::process::ExitCode;

use cli::Cli;

/// Execute a parsed command line and return the process exit code.
pub async fn run(_cli: Cli) -> ExitCode {
    ExitCode::SUCCESS
}
