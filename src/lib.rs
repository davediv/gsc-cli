//! `gsc` — Google Search Console from the command line.
//!
//! The binary in `main.rs` is a thin wrapper around [`run`]; everything else
//! lives in this library so it can be unit-tested and reused.

pub mod api;
pub mod auth;
pub mod cli;
pub mod commands;
pub mod config;
pub mod dates;
pub mod diagnose;
pub mod error;
pub mod fsutil;
pub mod http;
pub mod insights;
pub mod output;
pub mod site;
pub mod sitemap_xml;

use std::path::PathBuf;
use std::process::ExitCode;

use auth::TokenProvider;
use cli::{Cli, Command, GlobalArgs};
use config::Config;
use error::{Error, Result};
use output::{Format, Report};
use site::SiteRef;

/// Everything a command needs besides its own arguments.
pub struct Context {
    /// Resolved output format: flag or env, then config, then auto-detected.
    pub format: Format,
    /// The format the user asked for by name, if any. `None` means
    /// [`Context::format`] was auto-detected.
    pub explicit_format: Option<Format>,
    pub config_dir: PathBuf,
    pub config: Config,
    /// Log HTTP requests to stderr.
    pub verbose: bool,
    /// The property named by `--site` or `$GSC_SITE`, as typed.
    pub site_arg: Option<String>,
}

impl Context {
    fn new(global: &GlobalArgs) -> Result<Self> {
        let config_dir = config::config_dir()?;
        let config = Config::load(&config_dir)?;
        let explicit_format = global.format.or(config.format);
        Ok(Self {
            format: explicit_format.unwrap_or_else(Format::detect),
            explicit_format,
            config_dir,
            config,
            verbose: global.verbose,
            site_arg: global.site.clone(),
        })
    }

    /// The property to operate on: `--site`, then `$GSC_SITE`, then the
    /// saved default. A bare domain is matched against the account's properties.
    pub async fn site(&self, client: &api::Client) -> Result<String> {
        let input = self
            .site_arg
            .as_deref()
            .or(self.config.site.as_deref())
            .ok_or_else(|| {
                Error::usage("no property selected").with_hint(
                    "pass --site <property>, set $GSC_SITE, or save a default with \
                     `gsc config set site <property>`; `gsc sites list` shows what is available",
                )
            })?;
        self.resolve_site(client, input).await
    }

    /// Turn what the user typed into the exact identifier the API expects.
    pub async fn resolve_site(&self, client: &api::Client, input: &str) -> Result<String> {
        match site::parse(input)? {
            SiteRef::Exact(site_url) => Ok(site_url),
            SiteRef::Bare(name) => {
                let sites = client.list_sites().await?;
                Ok(site::pick(&name, &sites)?.site_url.clone())
            }
        }
    }

    /// Credentials for calling Google, or an error telling the user to log in.
    pub fn token_provider(&self) -> Result<TokenProvider> {
        self.token_provider_with(http::client()?)
    }

    fn token_provider_with(&self, http: reqwest::Client) -> Result<TokenProvider> {
        let identity = auth::resolve(&self.config_dir)?.ok_or_else(auth::not_logged_in)?;
        Ok(TokenProvider::new(identity, http, self.config_dir.clone()))
    }

    /// An authenticated API client.
    pub fn client(&self) -> Result<api::Client> {
        let http = http::client()?;
        let tokens = self.token_provider_with(http.clone())?;
        Ok(api::Client::new(http, tokens, self.verbose))
    }

    /// Print a raw JSON document, whatever the active format. Used where
    /// there is no meaningful table to draw.
    pub fn emit_json(&self, value: &serde_json::Value) -> Result<()> {
        output::print(&Report::new(value.clone()), Format::Json)
            .map_err(|e| Error::io("writing output", e))
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
        Command::Analytics { command } => commands::analytics::run(command, ctx).await,
        Command::Api(args) => commands::api::run(args, ctx).await,
        Command::Auth { command } => commands::auth::run(command, ctx).await,
        Command::Config { command } => commands::config::run(command, ctx),
        Command::Insights { command } => commands::insights::run(command, ctx).await,
        Command::Inspect { command } => commands::inspect::run(command, ctx).await,
        Command::Sitemaps { command } => commands::sitemaps::run(command, ctx).await,
        Command::Sites { command } => commands::sites::run(command, ctx).await,
    }
}

fn report_error(err: &Error, format: Format) {
    match format {
        Format::Json => eprintln!("{}", err.render_json()),
        Format::Table | Format::Csv => eprintln!("{}", err.render_human()),
    }
}
