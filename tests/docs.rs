//! Every `gsc ...` example in the documentation must be a command line the
//! program accepts. This catches renamed flags and subcommands before a
//! reader, or an agent following the guide, runs into them.

use clap::Parser;
use clap::error::ErrorKind;
use gsc_cli::cli::Cli;

const DOCUMENTS: [(&str, &str); 3] = [
    ("README.md", include_str!("../README.md")),
    ("AGENTS.md", include_str!("../AGENTS.md")),
    (
        "skills/gsc/SKILL.md",
        include_str!("../skills/gsc/SKILL.md"),
    ),
];

/// Command lines found in a Markdown document: lines of fenced code blocks
/// and inline code spans that start with `gsc `.
fn examples(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            let command = trimmed.strip_prefix("$ ").unwrap_or(trimmed);
            if command.starts_with("gsc ") {
                found.push(command.to_owned());
            }
        } else {
            // Odd-numbered pieces of a line split on backticks are code spans.
            for span in line.split('`').skip(1).step_by(2) {
                if span.starts_with("gsc ") {
                    found.push(span.to_owned());
                }
            }
        }
    }
    found
}

/// Reduce an example to the words `gsc` itself would receive: no trailing
/// comment, and nothing after a pipe or redirection.
fn arguments(example: &str) -> Option<Vec<String>> {
    let mut words = shlex::split(example)?;
    if let Some(end) = words
        .iter()
        .position(|word| word.starts_with('#') || matches!(word.as_str(), "|" | ">" | ">>" | "&&"))
    {
        words.truncate(end);
    }
    Some(words)
}

#[test]
fn every_documented_command_line_is_accepted() {
    let mut checked = 0;
    let mut problems = Vec::new();

    for (name, text) in DOCUMENTS {
        for example in examples(text) {
            // `gsc <command> --help` and similar describe a shape, not a command.
            if example.contains("<command>") || example.contains("...") {
                continue;
            }
            let Some(words) = arguments(&example) else {
                problems.push(format!("{name}: cannot tokenise `{example}`"));
                continue;
            };
            checked += 1;
            let Err(error) = Cli::try_parse_from(&words) else {
                continue;
            };
            match error.kind() {
                // Prose often names a command without its arguments
                // ("run `gsc auth login`"); that is still a real command.
                ErrorKind::MissingRequiredArgument
                | ErrorKind::MissingSubcommand
                | ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {}
                kind => problems.push(format!(
                    "{name}: `{example}` is rejected ({kind:?}): {}",
                    error.to_string().lines().next().unwrap_or_default()
                )),
            }
        }
    }

    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    assert!(
        checked > 60,
        "expected to find many examples, found only {checked}; is the extraction broken?"
    );
}

#[test]
fn extraction_finds_fenced_and_inline_examples() {
    let markdown = "\
Run `gsc sites list` first, not `cargo build`.

```sh
gsc analytics keywords --days 7     # a comment
$ gsc guide --skill > SKILL.md
cargo test
```

| Task | Command |
| --- | --- |
| Totals | `gsc analytics summary`, `gsc analytics dates` |
";
    assert_eq!(
        examples(markdown),
        [
            "gsc sites list",
            "gsc analytics keywords --days 7     # a comment",
            "gsc guide --skill > SKILL.md",
            "gsc analytics summary",
            "gsc analytics dates",
        ]
    );
    assert_eq!(
        arguments("gsc analytics keywords --days 7     # a comment").unwrap(),
        ["gsc", "analytics", "keywords", "--days", "7"]
    );
    assert_eq!(
        arguments("gsc guide --skill > SKILL.md").unwrap(),
        ["gsc", "guide", "--skill"]
    );
    assert_eq!(
        arguments(r#"gsc api POST /x -d '{"a": "b c"}'"#).unwrap(),
        ["gsc", "api", "POST", "/x", "-d", r#"{"a": "b c"}"#]
    );
}

#[test]
fn a_wrong_flag_would_be_caught() {
    let words = arguments("gsc analytics keywords --dayz 7").unwrap();
    let error = Cli::try_parse_from(words).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnknownArgument);

    let words = arguments("gsc insights compare --by planet").unwrap();
    assert_eq!(
        Cli::try_parse_from(words).unwrap_err().kind(),
        ErrorKind::InvalidValue
    );
}
