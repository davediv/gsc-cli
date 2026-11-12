//! Command-line definition.

use clap::Parser;

/// Google Search Console from the command line.
///
/// Query search performance, manage properties and sitemaps, and inspect how
/// Google indexes your URLs. Output is a table on a terminal and JSON when
/// piped, so the same commands work for people and for AI agents.
#[derive(Debug, Parser)]
#[command(name = "gsc", version, propagate_version = true)]
pub struct Cli {}
