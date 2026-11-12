//! Command-line definition.

use clap::{Args, Parser, Subcommand};

use crate::commands::auth::AuthCommand;
use crate::commands::config::ConfigCommand;
use crate::output::Format;

/// Google Search Console from the command line.
///
/// Query search performance, manage properties and sitemaps, and inspect how
/// Google indexes your URLs. Output is a table on a terminal and JSON when
/// piped, so the same commands work for people and for AI agents.
#[derive(Debug, Parser)]
#[command(
    name = "gsc",
    version,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
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
        hide_env_values = true,
        help_heading = "Global options"
    )]
    pub format: Option<Format>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to Google and manage stored credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// View and change saved defaults (default property, output format)
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}
