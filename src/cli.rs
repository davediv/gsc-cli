//! Command-line definition.

use clap::{Args, Parser};

use crate::output::Format;

/// Google Search Console from the command line.
///
/// Query search performance, manage properties and sitemaps, and inspect how
/// Google indexes your URLs. Output is a table on a terminal and JSON when
/// piped, so the same commands work for people and for AI agents.
#[derive(Debug, Parser)]
#[command(name = "gsc", version, propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Options accepted by every command.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Output format [default: table on a terminal, json when piped]
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "FORMAT",
        env = "GSC_FORMAT",
        hide_default_value = true
    )]
    pub format: Option<Format>,
}
