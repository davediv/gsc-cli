---
name: gsc
description: Query and manage Google Search Console with the `gsc` CLI. Use when the user asks about search performance or SEO data (keywords, queries, clicks, impressions, CTR, rankings, traffic changes), why a page is not indexed or has indexing errors, sitemap status, or wants to submit sitemaps or notify Google of changed URLs.
---

# Using `gsc`, the Google Search Console CLI

`gsc` reads search performance and index status from Google Search Console and
prints JSON you can parse. Run `gsc <command> --help` for every option.

## Before the first command

```sh
gsc auth status     # exit 0: ready. exit 3: not logged in.
gsc sites list      # the properties this account can read
```

If `auth status` exits 3, stop and ask the user to run `gsc auth login`
themselves: it opens a browser, which you cannot complete on their behalf.

Pick the property with `--site`. It accepts the exact identifier
(`sc-domain:example.com`, `https://example.com/`) or just `example.com`, which
is matched against the account's properties. To avoid repeating it:
`gsc config set site example.com`, or set `GSC_SITE`.

## Output contract

- **stdout** is one JSON object whenever output is piped, which is always the
  case for you. Use `--format csv` for the most compact rows, and
  `--format table` only when showing results to a person.
- A **`notes`** array, when present, holds caveats you must take into account
  (results cut off, data still processing, sitemaps that could not be read).
- **`truncated: true`** means more rows exist. Raise `--limit` or pass `--all`.
- **stderr** carries errors as `{"error":{"kind","message","hint","status","code"}}`.
  Read `hint`: it says what to do next.

| Exit | Meaning | What to do |
| --- | --- | --- |
| 0 | Success | |
| 1 | Network failure, Google 5xx, or local I/O | Retry later |
| 2 | Bad arguments | Fix the command; see `--help` |
| 3 | Not logged in or credentials rejected | Ask the user to run `gsc auth login` |
| 4 | Google refused: no access, not found, invalid request | Read `message` and `hint` |
| 5 | Quota exhausted | Wait; do not retry in a loop |

Units: `ctr` is a fraction (`0.0353` is 3.53%). `position` is the average
rank, where 1 is the top result and lower is better. Dates are Pacific time.
Data is final about 3 days after the fact, which is why the default period is
the 28 days ending 3 days ago.

## Commands by task

| Task | Command |
| --- | --- |
| Totals for a period | `gsc analytics summary` |
| Top keywords | `gsc analytics keywords` |
| Top pages | `gsc analytics pages` |
| Daily trend | `gsc analytics dates` |
| By country or device | `gsc analytics countries`, `gsc analytics devices` |
| By rich-result type | `gsc analytics appearance` |
| Keywords for one page | `gsc analytics keywords --page /blog/post` |
| Pages ranking for a term | `gsc analytics pages --query "term"` |
| Any combination of dimensions | `gsc analytics query -d query,page,country` |
| What changed vs the previous period | `gsc insights compare --by page` |
| Biggest losers and winners | `gsc insights declining`, `gsc insights rising` |
| Keywords close to page one | `gsc insights striking-distance --with-page` |
| Good ranking but few clicks | `gsc insights low-ctr` |
| Index status of one URL | `gsc inspect url <URL>` |
| Indexing problems across the site | `gsc inspect batch --submitted-sitemaps --only-issues` |
| Sitemap status | `gsc sitemaps list` |
| Submit or refresh a sitemap | `gsc sitemaps submit <URL>` |
| Withdraw a sitemap | `gsc sitemaps delete <URL> --yes` |
| Anything else in the API | `gsc api GET /webmasters/v3/sites` |

Options shared by `analytics` and `insights`:

- Period: `--days N`, or `--start YYYY-MM-DD --end YYYY-MM-DD`.
- Filters (combined with AND): `--query TEXT`, `--page TEXT` (both "contains"),
  `--country usa` (ISO alpha-3), `--device mobile|desktop|tablet`, or the
  general form `--filter "<dimension> <operator> <value>"` with operators
  `equals`, `not-equals`, `contains`, `not-contains`, `regex`, `not-regex`.
  Example: `--filter "query regex ^(how|what|why) "`.
- `--type web|image|video|news|discover|google-news`.
- Size: `--limit N` (default 100 for analytics, 50 for insights) or `--all`.
- `analytics` only: `--sort clicks|impressions|ctr|position`, and
  `--data-state all` to include the most recent, still-changing days.

## Workflows

### How is the site doing?

```sh
gsc analytics summary --days 28
gsc insights compare --by page --days 28      # totals, then per-page changes
gsc analytics dates --days 90 --format csv    # trend
```

In `insights compare`, each row has `clicks`, `clicksPrevious`, `clicksChange`
and the same triple for impressions, CTR and position. `positionChange` is
current minus previous, so **negative means the page moved up**. Add
`--vs year` to compare with the same dates last year, which removes
seasonality.

### Why did traffic drop?

1. `gsc insights declining --by page` to find which pages lost clicks.
2. For a page that dropped: `gsc insights declining --by query --page <path>`
   to see which keywords it lost.
3. Read the cause from the numbers:
   - position worse, impressions steady: lost rankings (content, competition).
   - impressions down, position steady: less demand, or the page left the index.
   - both steady, CTR down: the search result looks less appealing, or a
     SERP feature is taking the clicks.
4. `gsc inspect url <page>` to rule out an indexing problem.
5. Repeat step 1 with `--by device` or `--by country` if the drop is not
   page-specific.

### Find keyword opportunities

```sh
gsc insights striking-distance --with-page   # positions 8-20 with real impressions
gsc insights low-ctr                         # ranks well, under-clicked
gsc insights low-ctr --by query
```

Striking-distance keywords need stronger content and internal links on the
page shown. Low-CTR items need a better title and meta description:
`expectedCtr` is this property's own average at that position and
`missedClicks` is what closing the gap is worth.

### Find and fix indexing errors

```sh
gsc inspect batch --submitted-sitemaps --only-issues --limit 200
```

The output's `issues` array groups problems, most serious first. Each entry
has a `code`, a `severity`, the affected `urls`, and a `fix`. Apply the fix in
the site's source, then:

1. Confirm the live page is right yourself (fetch it and check the status
   code, headers, robots.txt, and HTML). `gsc inspect` reports Google's
   **last crawl**, so it keeps showing the old state until Google returns.
2. `gsc sitemaps submit <sitemap-url>` to prompt a recrawl.
3. Re-run `gsc inspect url <URL>` after a few days. Compare `lastCrawlTime`
   with when you deployed to know whether Google has seen the fix.

There is no API to "request indexing"; that button exists only in the
Search Console UI. Tell the user if a page needs it urgently.

| Code | Meaning | Where the fix usually is |
| --- | --- | --- |
| `blocked_by_robots_txt` | robots.txt forbids crawling | `robots.txt` |
| `noindex_meta_tag` | `<meta name="robots" content="noindex">` | Page template or SEO plugin settings |
| `noindex_http_header` | `X-Robots-Tag: noindex` response header | Server, CDN, or framework config |
| `noindex` | Excluded by noindex, mechanism not reported | Check both of the above |
| `not_found_404` | URL returned 404 | Restore, redirect, or remove from sitemap and links |
| `soft_404` | 200 response that looks empty or like an error | Page content or rendering; else return a real 404 |
| `server_error_5xx` | Server failed when Google crawled | Server logs, timeouts, firewall |
| `unauthorized_401`, `forbidden_403`, `blocked_4xx` | Googlebot was refused | Auth rules, WAF, bot protection |
| `redirect_error` | Redirect loop or chain | Redirect rules |
| `invalid_url` | Malformed URL | The links that point to it |
| `crawl_error` | Google-side crawl failure | Usually temporary; inspect again |
| `crawled_not_indexed` | Crawled, Google chose not to index | Content quality, duplication, internal links |
| `discovered_not_indexed` | Known but not crawled yet | Internal links, sitemap, server speed, time |
| `unknown_to_google` | Never seen by Google | Sitemap and internal links; check the URL is right |
| `indexed_without_content` | Indexed, content unreadable | What the server returns to Googlebot |
| `canonical_mismatch` | Google chose a different canonical than declared | `rel=canonical`, internal links, redirects |
| `duplicate_no_canonical` | Duplicate with no canonical declared | Add `rel=canonical` |
| `canonicalised_elsewhere` (info) | Another URL is indexed instead | Nothing, if intended |
| `redirected` (info) | URL redirects; target is indexed | Update links and sitemap to the target |
| `not_in_sitemap` (info) | Indexed but not in a sitemap | Add to sitemap |
| `not_indexed` | Not indexed, reason not recognised | Read `coverageState`; open `inspectionLink` |
| `rich_result_error`, `rich_result_warning` | Structured data problem, in Google's words | JSON-LD or microdata on the page |
| `amp_error`, `amp_warning` | AMP validation problem | The AMP version of the page |
| `mobile_usability_error`, `mobile_usability_warning` | Mobile layout problem | Viewport, font sizes, tap targets |
| `inspection_failed` | The URL could not be inspected | Often the URL is outside the property |
| `not_inspected` | Skipped because the quota ran out | Rerun tomorrow |

### Check sitemaps

```sh
gsc sitemaps list
gsc sitemaps get https://example.com/sitemap.xml
```

`errors` and `warnings` are counts only; the API does not say what they are.
Fetch the sitemap and validate it yourself: well-formed XML, absolute URLs on
the property's host, and URLs that return 200 and are not redirected,
noindexed, or blocked.

## Limits and gotchas

- **URL inspection quota**: 2,000 per day and 600 per minute for each
  property. `inspect batch` defaults to `--limit 100`. Do not raise it
  without a reason, and never loop on exit code 5.
- **Search analytics rows**: one request returns the 25,000 rows with the
  most clicks. `--sort` and the `insights` reports rank within that set and
  add a note when it was not everything; `--all` scans every row.
- **Row sums are lower than totals.** Google hides rare queries for privacy,
  so adding up keyword rows undercounts. Use `gsc analytics summary` for totals.
- **Grouping by page changes the numbers.** Position and impressions are
  counted per URL instead of per property, so page-level and query-level
  reports do not add up to each other.
- **Property types.** A Domain property (`sc-domain:`) covers all subdomains
  and protocols. A URL-prefix property covers only URLs under that prefix,
  and only for those can paths like `/pricing` be used in place of full URLs.
- **Destructive commands** (`sites remove`, `sitemaps delete`,
  `indexing publish --deleted`) refuse to run without `--yes`. Ask the user
  before adding it.
- **`gsc indexing`** is officially for job-posting and livestream pages only.
  Do not use it as a general "index this page" tool.
- **`gsc auth token`** prints a live credential. Never echo it into logs,
  files, or messages.
