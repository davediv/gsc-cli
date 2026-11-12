//! `gsc` — Google Search Console from the command line.
//!
//! The binary in `main.rs` is a thin wrapper around [`run`]; everything else
//! lives in this library so it can be unit-tested and reused.

pub mod cli;
pub mod error;
pub mod output;

use std::process::ExitCode;

use cli::Cli;
use error::{Error, Result};
use output::Format;

/// Execute a parsed command line and return the process exit code.
///
/// Failures are reported on stderr in the active output format, so a caller
/// that asked for JSON gets a JSON error object it can parse.
pub async fn run(cli: Cli) -> ExitCode {
    let format = cli.global.format.unwrap_or_else(Format::detect);
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report_error(&err, format);
            ExitCode::from(err.exit_code())
        }
    }
}

async fn dispatch(_cli: Cli) -> Result<()> {
    Ok(())
}

fn report_error(err: &Error, format: Format) {
    match format {
        Format::Json => eprintln!("{}", err.render_json()),
        Format::Table | Format::Csv => eprintln!("{}", err.render_human()),
    }
}
