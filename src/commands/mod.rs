//! One module per top-level command. Each defines its clap arguments and a
//! `run` function that turns them into a [`crate::output::Report`].

pub mod analytics;
pub mod api;
pub mod auth;
pub mod config;
pub mod sites;
