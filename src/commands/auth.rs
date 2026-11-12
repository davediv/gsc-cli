//! `gsc auth` — sign in, inspect, and remove credentials.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::json;

use crate::Context;
use crate::auth::oauth::{self, ClientConfig, LoginRequest};
use crate::auth::store::{self, StoredCredentials};
use crate::auth::{
    self, Api, Credentials, ENV_ACCESS_TOKEN, Identity, SCOPE_INDEXING, SCOPE_WEBMASTERS,
    SCOPE_WEBMASTERS_READONLY, TokenProvider,
};
use crate::error::{Error, Result};
use crate::http;
use crate::output::{Format, Report};

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in with a Google account
    ///
    /// Opens a browser for Google's consent page and stores a refresh token
    /// in the config directory. You need an OAuth client of type "Desktop
    /// app" from your own Google Cloud project, with the Search Console API
    /// enabled:
    ///
    ///   https://console.cloud.google.com/apis/credentials
    ///
    /// Download the client's JSON file and pass it once with
    /// --client-secret-file; later logins reuse the saved client.
    ///
    /// The redirect goes to 127.0.0.1 on the machine running gsc. To sign in
    /// from a remote shell, choose a --port and forward it over SSH
    /// (`ssh -L PORT:127.0.0.1:PORT host`), then open the printed URL locally.
    #[command(verbatim_doc_comment)]
    Login(LoginArgs),

    /// Show which credentials are active and check that they work
    Status,

    /// Remove stored credentials from this machine
    Logout {
        /// Also revoke the grant at Google. This signs out every machine
        /// that used the same OAuth client with the same Google account.
        #[arg(long)]
        revoke: bool,
    },

    /// Print an access token, for example to use with curl
    ///
    /// Prints the bare token unless --format json is given explicitly.
    Token {
        /// Get a token for the Indexing API instead of Search Console
        #[arg(long)]
        indexing: bool,
    },
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// OAuth client JSON downloaded from Google Cloud Console
    #[arg(long, value_name = "PATH", conflicts_with_all = ["client_id", "client_secret"])]
    pub client_secret_file: Option<PathBuf>,

    /// OAuth client ID (alternative to --client-secret-file)
    #[arg(
        long,
        value_name = "ID",
        env = "GSC_CLIENT_ID",
        hide_env_values = true,
        requires = "client_secret"
    )]
    pub client_id: Option<String>,

    /// OAuth client secret (alternative to --client-secret-file)
    #[arg(
        long,
        value_name = "SECRET",
        env = "GSC_CLIENT_SECRET",
        hide_env_values = true,
        requires = "client_id"
    )]
    pub client_secret: Option<String>,

    /// Request read-only access to Search Console
    #[arg(long)]
    pub readonly: bool,

    /// Also request access to the Indexing API (`gsc indexing`)
    #[arg(long)]
    pub indexing: bool,

    /// Print the sign-in URL instead of opening a browser
    #[arg(long)]
    pub no_browser: bool,

    /// Loopback port for the sign-in redirect [default: any free port]
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
}

pub async fn run(command: AuthCommand, ctx: &Context) -> Result<()> {
    match command {
        AuthCommand::Login(args) => login(args, ctx).await,
        AuthCommand::Status => status(ctx).await,
        AuthCommand::Logout { revoke } => logout(revoke, ctx).await,
        AuthCommand::Token { indexing } => token(indexing, ctx).await,
    }
}

async fn login(args: LoginArgs, ctx: &Context) -> Result<()> {
    let client = client_config(&args, ctx)?;
    let mut scopes = vec![if args.readonly {
        SCOPE_WEBMASTERS_READONLY
    } else {
        SCOPE_WEBMASTERS
    }];
    if args.indexing {
        scopes.push(SCOPE_INDEXING);
    }

    let http = http::client()?;
    let grant = oauth::login(
        &http,
        LoginRequest {
            client: &client,
            scopes: &scopes,
            port: args.port.unwrap_or(0),
            open_browser: !args.no_browser,
        },
    )
    .await?;

    store::save_credentials(
        &ctx.config_dir,
        &StoredCredentials::AuthorizedUser(grant.user.clone()),
    )?;
    TokenProvider::remember(&ctx.config_dir, &grant.user, &grant.access_token);

    let path = store::credentials_path(&ctx.config_dir);
    let report = Report::new(json!({
        "authenticated": true,
        "method": "oauth",
        "scopes": grant.user.scopes,
        "credentialsFile": path,
    }))
    .field("Status", "Logged in")
    .field("Scopes", short_scopes(&grant.user.scopes))
    .field("Saved to", path.display().to_string())
    .note("Next: run `gsc sites list` to see your properties.");
    ctx.emit(&report)
}

/// Decide which OAuth client to use: explicit flags first, then whatever a
/// previous login saved.
fn client_config(args: &LoginArgs, ctx: &Context) -> Result<ClientConfig> {
    if let Some(path) = &args.client_secret_file {
        return ClientConfig::from_secret_file(path);
    }
    if let (Some(id), Some(secret)) = (&args.client_id, &args.client_secret) {
        return Ok(ClientConfig::new(id.clone(), secret.clone()));
    }
    // A corrupt credentials file should not block signing in again.
    if let Ok(Some(StoredCredentials::AuthorizedUser(user))) =
        store::load_credentials(&ctx.config_dir)
    {
        let mut client = ClientConfig::new(user.client_id, user.client_secret);
        client.token_uri = user.token_uri;
        return Ok(client);
    }
    Err(Error::usage("no OAuth client configured").with_hint(
        "create an OAuth client ID of type \"Desktop app\" at \
         https://console.cloud.google.com/apis/credentials, download its JSON, \
         and run `gsc auth login --client-secret-file <path>`",
    ))
}

async fn status(ctx: &Context) -> Result<()> {
    let provider = ctx.token_provider()?;
    let identity = provider.identity();
    // Proves the credentials still work: refreshes the token if needed.
    let token = provider.token(Api::SearchConsole).await?;

    let scopes = identity.scopes();
    let mut report = Report::new(json!({
        "authenticated": true,
        "method": identity.method(),
        "source": identity.source.to_string(),
        "account": identity.account(),
        "scopes": scopes,
        "tokenExpiresAt": token.expires_at,
    }))
    .field("Method", method_label(identity))
    .field("Source", identity.source.to_string());
    if let Some(account) = identity.account() {
        report = report.field("Client ID", account);
    }
    if !scopes.is_empty() {
        report = report.field("Scopes", short_scopes(&scopes));
    }
    report = report.field(
        "Token",
        match token.expires_at {
            Some(expires_at) => {
                let minutes = (expires_at - auth::unix_now()).max(0) / 60;
                format!("valid, expires in {minutes} min")
            }
            None => "supplied by the environment, not verified".to_owned(),
        },
    );
    ctx.emit(&report)
}

fn method_label(identity: &Identity) -> &'static str {
    match identity.credentials {
        Credentials::AccessToken(_) => "Access token",
        Credentials::AuthorizedUser(_) => "OAuth (browser login)",
    }
}

async fn logout(revoke: bool, ctx: &Context) -> Result<()> {
    let mut revoked = false;
    if revoke
        && let Ok(Some(StoredCredentials::AuthorizedUser(user))) =
            store::load_credentials(&ctx.config_dir)
    {
        oauth::revoke(&http::client()?, &user.refresh_token).await?;
        revoked = true;
    }
    let removed = store::clear(&ctx.config_dir)?;

    let status = match (removed, revoked) {
        (true, true) => "Logged out and revoked access at Google",
        (true, false) => "Logged out",
        (false, _) => "No stored credentials to remove",
    };
    let mut report =
        Report::new(json!({ "loggedOut": removed, "revoked": revoked })).field("Status", status);
    if std::env::var_os(ENV_ACCESS_TOKEN).is_some() {
        report = report.note(format!(
            "${ENV_ACCESS_TOKEN} is still set and will keep being used."
        ));
    }
    ctx.emit(&report)
}

async fn token(indexing: bool, ctx: &Context) -> Result<()> {
    let api = if indexing {
        Api::Indexing
    } else {
        Api::SearchConsole
    };
    let token = ctx.token_provider()?.token(api).await?;

    // This command exists to be used as `$(gsc auth token)`, so the usual
    // "JSON when piped" default would be a trap. Only emit a document when a
    // structured format was asked for by name.
    match ctx.explicit_format {
        Some(Format::Json) | Some(Format::Csv) => ctx.emit(
            &Report::new(json!({
                "accessToken": token.access_token,
                "expiresAt": token.expires_at,
            }))
            .field("accessToken", token.access_token),
        ),
        Some(Format::Table) | None => {
            println!("{}", token.access_token);
            Ok(())
        }
    }
}

/// `https://www.googleapis.com/auth/webmasters.readonly` → `webmasters.readonly`.
fn short_scopes(scopes: &[String]) -> String {
    scopes
        .iter()
        .map(|scope| scope.rsplit('/').next().unwrap_or(scope))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_scope_urls() {
        assert_eq!(
            short_scopes(&[SCOPE_WEBMASTERS_READONLY.into(), SCOPE_INDEXING.into()]),
            "webmasters.readonly, indexing"
        );
        assert_eq!(short_scopes(&["custom".into()]), "custom");
        assert_eq!(short_scopes(&[]), "");
    }
}
