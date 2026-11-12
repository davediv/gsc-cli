//! `gsc config` — view and change saved defaults.

use clap::Subcommand;
use serde_json::json;

use crate::Context;
use crate::config::{Config, Key};
use crate::error::Result;
use crate::output::{Column, Report};

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show every setting and its current value
    List,
    /// Print one setting
    Get {
        #[arg(value_enum)]
        key: Key,
    },
    /// Save a default, e.g. `gsc config set site sc-domain:example.com`
    Set {
        #[arg(value_enum)]
        key: Key,
        value: String,
    },
    /// Remove a saved default
    Unset {
        #[arg(value_enum)]
        key: Key,
    },
    /// Print the directory where config and credentials are stored
    Path,
}

pub fn run(command: ConfigCommand, ctx: &Context) -> Result<()> {
    match command {
        ConfigCommand::List => {
            let settings: Vec<_> = Key::ALL
                .iter()
                .map(|key| json!({ "key": key.name(), "value": ctx.config.get(*key) }))
                .collect();
            let report = Report::new(json!({
                "configFile": Config::file_path(&ctx.config_dir),
                "settings": settings,
            }))
            .table(
                vec![Column::text("key", "Key"), Column::text("value", "Value")],
                "/settings",
            );
            ctx.emit(&report)
        }
        ConfigCommand::Get { key } => ctx.emit(&setting_report(key, ctx.config.get(key))),
        ConfigCommand::Set { key, value } => {
            let mut config = ctx.config.clone();
            config.set(key, &value)?;
            config.save(&ctx.config_dir)?;
            ctx.emit(&setting_report(key, config.get(key)))
        }
        ConfigCommand::Unset { key } => {
            let mut config = ctx.config.clone();
            config.unset(key);
            config.save(&ctx.config_dir)?;
            ctx.emit(&setting_report(key, None))
        }
        ConfigCommand::Path => {
            let dir = ctx.config_dir.display().to_string();
            ctx.emit(&Report::new(json!({ "configDir": dir })).field("Config directory", dir))
        }
    }
}

fn setting_report(key: Key, value: Option<String>) -> Report {
    let shown = value.clone().unwrap_or_else(|| "(not set)".into());
    Report::new(json!({ "key": key.name(), "value": value })).field(key.name(), shown)
}
