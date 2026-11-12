//! `gsc guide` — how to use `gsc`, written for AI agents.

use std::io::Write;

use clap::Args;

use crate::error::{Error, Result};

/// The skill file is the single source of truth for agent documentation.
/// Embedding it means any agent can learn the tool from the binary alone.
const SKILL: &str = include_str!("../../skills/gsc/SKILL.md");

#[derive(Debug, Args)]
pub struct GuideArgs {
    /// Print the complete skill file with its frontmatter, ready to save as
    /// `SKILL.md` in an agent's skills directory
    #[arg(long)]
    pub skill: bool,
}

pub fn run(args: GuideArgs) -> Result<()> {
    let text = if args.skill { SKILL } else { body(SKILL) };
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(Error::io("writing output", e)),
        _ => Ok(()),
    }
}

/// The guide without its YAML frontmatter.
fn body(skill: &str) -> &str {
    skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(_, body)| body.trim_start())
        .unwrap_or(skill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_drops_the_frontmatter() {
        assert_eq!(
            body("---\nname: x\ndescription: y\n---\n\n# Title\n\nText\n"),
            "# Title\n\nText\n"
        );
        assert_eq!(body("# No frontmatter\n"), "# No frontmatter\n");
    }

    #[test]
    fn the_embedded_skill_is_well_formed() {
        assert!(SKILL.starts_with("---\nname: gsc\ndescription: "));
        assert!(body(SKILL).starts_with("# Using `gsc`"));
        assert!(!body(SKILL).contains("\n---\nname:"));
    }
}
