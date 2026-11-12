use std::process::ExitCode;

use clap::Parser;
use gsc_cli::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    gsc_cli::run(Cli::parse()).await
}
