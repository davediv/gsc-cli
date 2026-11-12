//! `gsc` — Google Search Console from the command line.
//!
//! The binary in `main.rs` is a thin wrapper around [`run`]; everything else
//! lives in this library so it can be unit-tested and reused.

pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod fsutil;
pub mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use cli::{Cli, Command, GlobalArgs};
use config::Config;
use error::{Error, Result};
use output::{Format, Report};

/// Everything a command needs besides its own arguments.
pub struct Context {
    /// Resolved output format: flag or env, then config, then auto-detected.
    pub format: Format,
    pub config_dir: PathBuf,
    pub config: Config,
}

impl Context {
    fn new(global: &GlobalArgs) -> Result<Self> {
        let config_dir = config::config_dir()?;
        let config = Config::load(&config_dir)?;
        let format = global
            .format
            .or(config.format)
            .unwrap_or_else(Format::detect);
        Ok(Self {
            format,
            config_dir,
            config,
        })
    }

    /// Print a command's result in the active format.
    pub fn emit(&self, report: &Report) -> Result<()> {
        output::print(report, self.format).map_err(|e| Error::io("writing output", e))
    }
}

/// Execute a parsed command line and return the process exit code.
///
/// Failures are reported on stderr in the active output format, so a caller
/// that asked for JSON gets a JSON error object it can parse.
pub async fn run(cli: Cli) -> ExitCode {
    // Until the config file has been read, fall back to what was asked for on
    // the command line so that even a broken config is reported consistently.
    let mut format = cli.global.format.unwrap_or_else(Format::detect);
    let result = match Context::new(&cli.global) {
        Ok(ctx) => {
            format = ctx.format;
            dispatch(cli.command, &ctx).await
        }
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report_error(&err, format);
            ExitCode::from(err.exit_code())
        }
    }
}

async fn dispatch(command: Command, ctx: &Context) -> Result<()> {
    match command {
        Command::Config { command } => commands::config::run(command, ctx),
    }
}

fn report_error(err: &Error, format: Format) {
    match format {
        Format::Json => eprintln!("{}", err.render_json()),
        Format::Table | Format::Csv => eprintln!("{}", err.render_human()),
    }
}
