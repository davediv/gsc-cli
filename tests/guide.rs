//! `gsc guide`, and checks that the agent guide stays in step with the code.

mod common;

use std::collections::BTreeSet;

use clap::CommandFactory;
use common::TestEnv;
use gsc_cli::cli::Cli;
use regex::Regex;

fn guide(args: &[&str]) -> String {
    let output = TestEnv::new()
        .gsc()
        .arg("guide")
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn guide_prints_markdown_without_needing_credentials() {
    let text = guide(&[]);
    assert!(text.starts_with("# Using `gsc`"));
    assert!(
        !text.contains("\nname: gsc"),
        "frontmatter is for the skill file only"
    );
    // Not wrapped in JSON, even though stdout is not a terminal here.
    assert!(!text.starts_with('{'));
}

#[test]
fn skill_flag_prints_an_installable_skill_file() {
    let text = guide(&["--skill"]);
    assert!(text.starts_with("---\nname: gsc\ndescription: "));
    assert!(text.ends_with(&guide(&[])), "same guide, plus frontmatter");
}

#[test]
fn the_guide_mentions_every_command() {
    let text = guide(&[]);
    let cli = Cli::command();
    for command in cli.get_subcommands() {
        let name = command.get_name();
        if matches!(name, "guide" | "help") {
            continue;
        }
        assert!(
            text.contains(&format!("gsc {name}")),
            "the guide never shows `gsc {name}`"
        );
        for sub in command.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            // Maintenance commands need no walkthrough.
            if matches!(name, "config" | "auth" | "sites" | "indexing") {
                continue;
            }
            assert!(
                text.contains(&format!("gsc {name} {}", sub.get_name())),
                "the guide never shows `gsc {name} {}`",
                sub.get_name()
            );
        }
    }
}

#[test]
fn issue_codes_in_the_guide_match_the_codes_the_program_emits() {
    // Codes produced by the diagnosis rules and by bulk inspection.
    let emitted = Regex::new(r#"(?:findings\.add\(\s*|=> \(|json!\(\[)"([a-z0-9_]+)""#).unwrap();
    let mut in_code = BTreeSet::new();
    for source in [
        include_str!("../src/diagnose.rs"),
        include_str!("../src/commands/inspect_batch.rs"),
    ] {
        for found in emitted.captures_iter(source) {
            in_code.insert(found[1].to_owned());
        }
    }
    assert!(
        in_code.contains("noindex_meta_tag"),
        "the source scan found nothing"
    );
    assert!(in_code.contains("rich_result_error"));
    assert!(in_code.contains("inspection_failed"));

    // Codes documented in the guide's issue table: backticked names in the
    // first column of rows that follow the "| Code |" header.
    let text = guide(&[]);
    let table = text
        .split("| Code | Meaning |")
        .nth(1)
        .expect("the guide has an issue-code table")
        .split("\n\n")
        .next()
        .unwrap();
    let code = Regex::new(r"`([a-z0-9_]+)`").unwrap();
    let mut in_guide = BTreeSet::new();
    for row in table.lines().filter(|line| line.starts_with("| `")) {
        let first_column = row.split('|').nth(1).unwrap();
        for found in code.captures_iter(first_column) {
            in_guide.insert(found[1].to_owned());
        }
    }

    assert_eq!(
        in_guide, in_code,
        "left: codes documented in skills/gsc/SKILL.md, right: codes the program emits"
    );
}
