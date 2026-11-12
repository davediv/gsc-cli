# AGENTS.md

`gsc` is a command-line client for Google Search Console, written in Rust and
designed to be driven by AI agents as well as people. This file has two parts:
how to **use** the tool, and how to **work on** this repository.

## Using `gsc`

The full agent guide ships inside the binary. Read it before your first command:

```sh
gsc guide
```

It covers the output contract, exit codes, a task-to-command table, workflows
(traffic drops, keyword opportunities, fixing indexing errors), and the
meaning of every issue code. The same text lives in
[`skills/gsc/SKILL.md`](skills/gsc/SKILL.md).

The short version:

```sh
gsc auth status                                   # exit 3 means the user must run `gsc auth login`
gsc sites list                                    # properties you can read
gsc analytics keywords --site example.com         # top search queries, last 28 days
gsc insights compare --by page                    # what changed vs the previous period
gsc inspect batch --submitted-sitemaps --only-issues   # indexing problems, with fixes
```

- Output is JSON whenever stdout is not a terminal. `--format csv` is the most
  compact form for rows.
- Errors arrive on stderr as `{"error":{"kind","message","hint",...}}`. The
  `hint` says what to do next.
- Exit codes: `0` ok, `1` transient or I/O, `2` usage, `3` auth, `4` refused
  by Google, `5` quota.
- Commands that delete something need `--yes`. Ask the user before adding it.

## Working on this repository

### Commands

```sh
cargo build
cargo test                                   # unit tests and end-to-end tests
cargo clippy --all-targets -- -D warnings    # must be clean
cargo fmt
```

Run all four before committing. Tests never contact Google: they run the real
binary against a local mock server.

### Layout

```
src/
  main.rs            thin wrapper around gsc_cli::run
  lib.rs             Context (format, config, property, API client) and dispatch
  cli.rs             top-level clap definition and global options
  error.rs           Error: kind -> exit code, plus an optional hint
  output.rs          Report: one JSON document rendered as table, JSON, or CSV
  config.rs          config directory and config.toml
  auth/              credential lookup, OAuth (PKCE), service accounts, token cache
  api/               HTTP client (retries, error mapping) and one module per API area
  commands/          one module per top-level command: clap args + run()
  dates.rs           date ranges and defaults
  site.rs            property identifiers and bare-domain matching
  diagnose.rs        inspection result -> issues with codes and fixes
  insights.rs        pure analysis functions over analytics rows
  sitemap_xml.rs     sitemap fetching and parsing
skills/gsc/SKILL.md  agent guide, embedded in the binary for `gsc guide`
tests/               end-to-end tests; tests/common has the mock-API harness
```

### Conventions

- **A command builds one `Report`**: a JSON document plus column definitions
  pointing into it. Never print results directly. The same rows then back the
  table, JSON, and CSV output.
- **JSON field names are camelCase**, matching Google's API. Put context
  first and `rows` last; field order is preserved in the output.
- **Caveats go in `Report::note`**, which reaches all three formats. If the
  output is partial or approximate, say so there.
- **Errors carry a hint.** Return `Error::usage(..)`, `Error::auth(..)` and
  friends with `.with_hint(..)` whenever the next step is knowable. Exit codes
  are part of the public contract; do not repurpose them.
- **Destructive operations require `--yes`** and must not call the API without it.
- **Secrets never reach output or `Debug`.** Types holding tokens or keys
  implement `Debug` by hand.
- **Keep logic pure where possible** (`insights.rs`, `diagnose.rs`,
  `dates.rs`) and unit-test it there; test the wiring end to end in `tests/`.

### Adding a command

1. Add the API call in `src/api/<area>.rs` as a method on `Client`.
2. Add `src/commands/<name>.rs` with its clap arguments and a `run` function
   that returns a `Report` through `ctx.emit`.
3. Register it in `src/commands/mod.rs`, `src/cli.rs`, and the `dispatch`
   match in `src/lib.rs`.
4. Add `tests/<name>.rs` using `MockApi` from `tests/common`.
5. Document it in `skills/gsc/SKILL.md`. `tests/guide.rs` fails if the guide
   and the command tree, or the issue codes, drift apart.

### Testing notes

- `MockApi::new()` gives an isolated config directory and a mock server;
  `api.json(&[...])` runs `gsc` and parses stdout, `api.error(&[...], code)`
  asserts the exit code and parses the error from stderr.
- The environment is cleared for every test run, so nothing leaks in from the
  developer's own credentials or `GSC_*` variables.
- Service-account tests generate a throwaway RSA key at run time. Do not
  commit private keys, even fake ones.

### Commits

One logical change per commit, in Conventional Commits style
(`feat(inspect): ...`, `fix(output): ...`). The body explains why, not just what.
