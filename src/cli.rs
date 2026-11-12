//! Command-line definition.

use clap::{Args, Parser, Subcommand};

use crate::commands::analytics::AnalyticsCommand;
use crate::commands::api::ApiArgs;
use crate::commands::auth::AuthCommand;
use crate::commands::config::ConfigCommand;
use crate::commands::inspect::InspectCommand;
use crate::commands::sitemaps::SitemapsCommand;
use crate::commands::sites::SitesCommand;
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

    /// Property to use: `sc-domain:example.com`, `https://example.com/`, or
    /// just `example.com` [default: saved with `gsc config set site`]
    #[arg(
        short,
        long,
        global = true,
        value_name = "PROPERTY",
        env = "GSC_SITE",
        hide_env_values = true,
        help_heading = "Global options"
    )]
    pub site: Option<String>,

    /// Log each HTTP request and response status to stderr
    #[arg(short, long, global = true, help_heading = "Global options")]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search performance: keywords, pages, countries, devices, trends
    ///
    /// Every subcommand accepts the same options. Dates default to the last
    /// 28 days of finalised data.
    ///
    ///   gsc analytics keywords --days 7
    ///   gsc analytics pages --query "pricing" --sort impressions
    ///   gsc analytics keywords --page /blog/my-post --country usa
    ///   gsc analytics dates --start 2026-01-01 --end 2026-03-31
    ///   gsc analytics query -d query,page --filter "query regex ^how to" --all
    #[command(verbatim_doc_comment, visible_alias = "search")]
    Analytics {
        #[command(subcommand)]
        command: AnalyticsCommand,
    },

    /// Call any Search Console or Indexing API endpoint directly
    ///
    /// An escape hatch for anything the other commands do not cover. The
    /// response is printed as JSON.
    ///
    ///   gsc api GET /webmasters/v3/sites
    ///   gsc api GET '/webmasters/v3/sites/{site}/sitemaps' --site example.com
    ///   gsc api POST /v1/urlInspection/index:inspect \
    ///     -d '{"inspectionUrl":"https://example.com/","siteUrl":"sc-domain:example.com"}'
    #[command(verbatim_doc_comment)]
    Api(ApiArgs),

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

    /// See how Google indexes a URL, with issues and how to fix them
    Inspect {
        #[command(subcommand)]
        command: InspectCommand,
    },

    /// List, inspect, submit, and remove sitemaps
    Sitemaps {
        #[command(subcommand)]
        command: SitemapsCommand,
    },

    /// List and manage Search Console properties
    Sites {
        #[command(subcommand)]
        command: SitesCommand,
    },
}
