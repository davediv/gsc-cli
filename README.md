# gsc

Google Search Console from the command line, built for people and for AI agents.

```sh
gsc analytics keywords --days 7                         # what people searched to find you
gsc insights compare --by page                          # what changed since the previous period
gsc inspect batch --submitted-sitemaps --only-issues    # indexing problems, each with a fix
```

- **Search performance**: keywords, pages, countries, devices, daily trends, with filters, any dimension combination, and automatic pagination.
- **Insights**: period-over-period comparison, biggest losers and winners, keywords just off page one, and pages whose CTR trails their ranking.
- **Index diagnostics**: inspect one URL or a whole sitemap. Problems come back with a stable code and a concrete fix, not just Google's status enum.
- **Sitemaps, properties, and the Indexing API**, plus `gsc api` as an escape hatch for any endpoint.
- **Agent-friendly by default**: JSON when piped, a table on a terminal, stable exit codes, errors that say what to do next, and a built-in guide (`gsc guide`).

## Install

Requires Rust 1.88 or newer and a C compiler (for the TLS library).

```sh
git clone https://github.com/davediv/gsc-cli.git && cd gsc-cli
cargo install --path .
gsc --version
```

Developed and tested on macOS. Linux should work the same way; Windows is untested.

## Setup

`gsc` talks to Google with your own credentials, from your own Google Cloud project. This takes about five minutes, once.

### 1. Create a project and enable the API

1. Open the [Google Cloud Console](https://console.cloud.google.com/) and create a project (or pick an existing one).
2. Enable the **Google Search Console API** for it: <https://console.cloud.google.com/apis/library/searchconsole.googleapis.com>

Then choose one of the two ways to authenticate.

### 2a. Browser login (your own Google account)

Best for working on your own machine.

1. Configure the OAuth consent screen: <https://console.cloud.google.com/apis/credentials/consent>. Choose **External**, fill in the app name and your email, and add your Google account as a **test user**.
2. Create the client at <https://console.cloud.google.com/apis/credentials>: **Create credentials → OAuth client ID → Application type: Desktop app**. Download the JSON file.
3. Log in:

   ```sh
   gsc auth login --client-secret-file ~/Downloads/client_secret_XXXX.json
   ```

   Your browser opens Google's consent page. If it warns that the app is unverified, that is expected for an app you just created: choose **Advanced → Go to (your app)**. After you approve, `gsc` stores a refresh token and you can delete the downloaded file.

> **Logins that expire after 7 days.** While the consent screen's publishing status is **Testing**, Google expires refresh tokens after a week and `gsc` will ask you to log in again. To avoid that, set the status to **In production** on the consent screen page. No verification is needed for personal use.

Useful flags: `--readonly` requests read-only access, `--indexing` adds the Indexing API scope, and `--no-browser` prints the URL instead of opening it. To log in on a remote machine, pick a port and forward it: `ssh -L 8085:127.0.0.1:8085 host`, then `gsc auth login --port 8085 --no-browser` there and open the printed URL locally.

### 2b. Service account (servers, CI, unattended agents)

No browser involved.

1. Create a service account at <https://console.cloud.google.com/iam-admin/serviceaccounts>, open it, and under **Keys** choose **Add key → Create new key → JSON**.
2. In [Search Console](https://search.google.com/search-console), open each property's **Settings → Users and permissions → Add user** and add the service account's email address (it ends in `iam.gserviceaccount.com`). **Restricted** is enough to read data; making changes such as submitting sitemaps needs **Full**; the Indexing API needs **Owner**.
3. Point `gsc` at the key:

   ```sh
   gsc auth login --service-account /secure/path/key.json
   ```

   Only the path is saved, so keep the file where it is. Alternatively set `GSC_CREDENTIALS_FILE=/secure/path/key.json` and skip `auth login` entirely.

A service account sees only the properties it was added to. If `gsc sites list` comes back empty, step 2 is what is missing.

### 3. Check it works and pick a default property

```sh
gsc auth status
gsc sites list
gsc config set site example.com     # so you can omit --site from now on
```

`--site` accepts the exact identifier (`sc-domain:example.com` for a Domain property, `https://example.com/` for a URL-prefix property) or just the domain, which is matched against your properties.

## Usage

### Search performance

```sh
gsc analytics summary                                   # totals, last 28 days
gsc analytics keywords --days 7 --limit 20
gsc analytics pages --sort impressions
gsc analytics keywords --page /blog/my-post             # keywords one page ranks for
gsc analytics pages --query "pricing" --country usa --device mobile
gsc analytics dates --start 2026-01-01 --end 2026-03-31 --format csv
gsc analytics query -d query,page --filter "query regex ^how to" --all
```

Dates default to the 28 days ending three days ago, because Google needs that long to finalise its numbers. Add `--data-state all` to include the most recent days.

Filters combine with AND. Besides the shorthands above there is a general form, `--filter "<dimension> <operator> <value>"`, with dimensions `query`, `page`, `country`, `device`, `search-appearance` and operators `equals`, `not-equals`, `contains`, `not-contains`, `regex`, `not-regex`.

### Insights

```sh
gsc insights compare --by page                # current vs the previous period
gsc insights compare --by query --vs year     # vs the same dates last year
gsc insights declining                        # pages that lost the most clicks
gsc insights rising --by query
gsc insights striking-distance --with-page    # keywords at positions 8-20
gsc insights low-ctr                          # ranking well, clicked rarely
```

### Indexing problems

```sh
gsc inspect url https://example.com/pricing
gsc inspect batch --submitted-sitemaps --only-issues
gsc inspect batch --sitemap https://example.com/sitemap.xml --limit 500
gsc inspect batch --file urls.txt
```

`inspect batch` stands in for Search Console's Page indexing report, which the API does not offer. It inspects URLs one by one and groups what it finds:

```
SEVERITY  ISSUE                URLS  WHAT IS WRONG                                       HOW TO FIX IT
error     noindex_meta_tag        3  The page has a robots meta tag containing noindex.  Remove `noindex` from the <meta name="robots"> ...
warning   crawled_not_indexed    12  Google crawled the page but chose not to index it.  This is a quality or duplication signal ...
```

Inspection reports what Google saw on its **last crawl**, not the live page, and Google allows 2,000 inspections per property per day. The default `--limit` is 100.

### Sitemaps and properties

```sh
gsc sitemaps list
gsc sitemaps submit https://example.com/sitemap.xml
gsc sitemaps delete https://example.com/old.xml --yes
gsc sites list
```

### Everything else

```sh
gsc api GET '/webmasters/v3/sites/{site}/sitemaps'      # {site} is replaced with your property
gsc indexing publish https://example.com/jobs/123       # Indexing API: job postings and livestreams only
gsc auth token                                          # a bearer token, e.g. for curl
```

Every command has `--help` with its full option list.

## Output

| Format | When | Notes |
| --- | --- | --- |
| `table` | default on a terminal | Formatted numbers, aligned columns |
| `json` | default when piped | One object; rows as flat objects; raw values |
| `csv` | `--format csv` | The primary table only; raw values; caveats go to stderr |

Choose explicitly with `--format`, `GSC_FORMAT`, or `gsc config set format json`.

In JSON and CSV, `ctr` is a fraction (`0.0353` means 3.53%) and `position` is the average rank (1 is the top). A `notes` array lists anything you need to know to read the data correctly, such as a result that was cut off.

Errors go to stderr, as JSON when the output format is JSON:

```json
{"error":{"kind":"api","message":"User does not have sufficient permission for site 'https://example.com/'.","hint":"the signed-in account cannot access this property; run `gsc sites list` to see the ones it can. ...","status":403,"code":"PERMISSION_DENIED"}}
```

| Exit code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Network failure, Google server error, or local I/O problem |
| 2 | Invalid arguments |
| 3 | Not logged in, or credentials rejected |
| 4 | Google refused the request (no access, not found, bad request) |
| 5 | Quota or rate limit exhausted |

## For AI agents

```sh
gsc guide
```

prints a guide written for agents: the output contract, a task-to-command table, workflows for questions like "why did traffic drop?" and "fix the indexing errors", and what each issue code means.

To install it as a [Claude Code](https://claude.com/claude-code) skill, so the agent reaches for `gsc` on its own:

```sh
mkdir -p ~/.claude/skills/gsc
gsc guide --skill > ~/.claude/skills/gsc/SKILL.md
```

For other agents, add a line such as "Use the `gsc` CLI for Google Search Console data; run `gsc guide` first" to the project's `AGENTS.md`.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `GSC_SITE` | Default property (same as `--site`) |
| `GSC_FORMAT` | Default output format |
| `GSC_CREDENTIALS_FILE` | Service-account key or `authorized_user` file to use instead of the saved login |
| `GSC_ACCESS_TOKEN` | A ready-made bearer token; overrides everything else and is never refreshed |
| `GOOGLE_APPLICATION_CREDENTIALS` | Google's standard variable; used only when there is no saved login |
| `GSC_CLIENT_ID`, `GSC_CLIENT_SECRET` | OAuth client for `gsc auth login`, instead of `--client-secret-file` |
| `GSC_CONFIG_DIR` | Where config and credentials live (default `~/.config/gsc`) |
| `GSC_MAX_RETRIES` | Retries for rate limits and server errors (default 3) |

Credentials are looked up in this order: `GSC_ACCESS_TOKEN`, `GSC_CREDENTIALS_FILE`, the login saved by `gsc auth login`, then `GOOGLE_APPLICATION_CREDENTIALS`. `gsc auth status` shows which one is in use.

## Where credentials are kept

`gsc auth login` writes to the config directory (`gsc config path`), in files readable only by your user:

- `credentials.json`: the OAuth refresh token and client, or the path to your service-account key (the key itself is not copied).
- `token_cache.json`: short-lived access tokens, so consecutive commands do not each request a new one.

`gsc` refuses to write there if the directory is writable by every user on the machine, because file permissions alone cannot stop someone replacing the files.

`gsc auth logout` deletes both. Add `--revoke` to also invalidate the grant at Google, which signs out every machine using the same OAuth client and account.

## Good to know

- **Row totals are lower than the summary.** Google withholds rare queries for privacy, so summing keyword rows undercounts. `gsc analytics summary` gives the real totals.
- **One request returns at most 25,000 rows**, ordered by clicks. `--all` pages through everything; `--sort` and the insight reports say so when they ranked only the first page.
- **There is no API to request indexing.** That button exists only in the Search Console UI. Submitting a sitemap is the closest equivalent.
- **Sitemap error details are not exposed.** The API reports how many errors a sitemap has, not what they are.
- **The Indexing API** is supported by Google only for pages with `JobPosting` or `BroadcastEvent` structured data.

## Development

```sh
cargo test                                   # unit and end-to-end tests; no network or credentials needed
cargo clippy --all-targets -- -D warnings
cargo fmt
```

End-to-end tests run the real binary against a local mock of Google's APIs. See [AGENTS.md](AGENTS.md) for the code layout and conventions.
