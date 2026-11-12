//! `gsc insights` — ready-made analyses of search performance.

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use super::analytics::{Scope, metric_columns};
use crate::Context;
use crate::api::Client;
use crate::api::analytics::{Dimension, Metric, PAGE_SIZE, Query, Rows};
use crate::dates::{self, DateRange};
use crate::error::{Error, Result};
use crate::insights::{self, Change, Movers};
use crate::output::{Column, Kind, Report};

#[derive(Debug, Subcommand)]
pub enum InsightsCommand {
    /// Compare a period with an earlier one, item by item
    ///
    /// Shows current, previous, and change for every metric, largest change
    /// first. A negative position change means the item moved up.
    Compare(ChangeArgs),

    /// Pages or queries that lost the most since the earlier period
    Declining(ChangeArgs),

    /// Pages or queries that gained the most since the earlier period
    Rising(ChangeArgs),

    /// Keywords ranking just below the top results
    ///
    /// Queries with an average position between 8 and 20 that people
    /// actually see: the ones a modest improvement can move up page one.
    #[command(visible_alias = "opportunities")]
    StrikingDistance(StrikingArgs),

    /// Pages or queries that earn fewer clicks than their ranking should bring
    ///
    /// Compares each item's CTR with this property's own average CTR at the
    /// same position. A large gap usually points at the title or description
    /// shown in search results.
    LowCtr(LowCtrArgs),
}

/// What a comparison is broken down by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum By {
    Query,
    Page,
    Country,
    Device,
}

impl By {
    fn dimension(self) -> Dimension {
        match self {
            By::Query => Dimension::Query,
            By::Page => Dimension::Page,
            By::Country => Dimension::Country,
            By::Device => Dimension::Device,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Versus {
    /// The period of the same length immediately before
    Previous,
    /// The same dates one year earlier
    Year,
}

#[derive(Debug, Args)]
pub struct ChangeArgs {
    #[command(flatten)]
    pub scope: Scope,

    /// What to break the comparison down by
    /// [default: query for compare, page for declining and rising]
    #[arg(long, value_enum, value_name = "DIMENSION")]
    pub by: Option<By>,

    /// Which earlier period to compare against
    #[arg(long, value_enum, value_name = "PERIOD", default_value = "previous")]
    pub vs: Versus,

    /// Metric whose change decides the order
    #[arg(long, value_enum, value_name = "METRIC", default_value = "clicks")]
    pub sort: Metric,

    /// Maximum number of rows
    #[arg(short = 'n', long, value_name = "N", default_value_t = 50)]
    pub limit: usize,

    /// Compare every row, not just the 25,000 with the most clicks in each period
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct StrikingArgs {
    #[command(flatten)]
    pub scope: Scope,

    /// Best average position to include
    #[arg(long, value_name = "POSITION", default_value_t = 8.0)]
    pub min_position: f64,

    /// Worst average position to include
    #[arg(long, value_name = "POSITION", default_value_t = 20.0)]
    pub max_position: f64,

    /// Ignore keywords shown fewer times than this in the period
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub min_impressions: u32,

    /// Also show which page ranks for each keyword
    #[arg(long)]
    pub with_page: bool,

    /// Maximum number of rows
    #[arg(short = 'n', long, value_name = "N", default_value_t = 50)]
    pub limit: usize,

    /// Scan every row, not just the 25,000 with the most clicks
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Subject {
    Page,
    Query,
}

#[derive(Debug, Args)]
pub struct LowCtrArgs {
    #[command(flatten)]
    pub scope: Scope,

    /// Whether to look at pages or at queries
    #[arg(long, value_enum, value_name = "DIMENSION", default_value = "page")]
    pub by: Subject,

    /// Only consider items ranking at this average position or better
    #[arg(long, value_name = "POSITION", default_value_t = 10.0)]
    pub max_position: f64,

    /// Ignore items shown fewer times than this in the period
    #[arg(long, value_name = "N", default_value_t = 100)]
    pub min_impressions: u32,

    /// Maximum number of rows
    #[arg(short = 'n', long, value_name = "N", default_value_t = 50)]
    pub limit: usize,

    /// Scan every row, not just the 25,000 with the most clicks
    #[arg(long)]
    pub all: bool,
}

pub async fn run(command: InsightsCommand, ctx: &Context) -> Result<()> {
    match command {
        InsightsCommand::Compare(args) => changes(args, Movers::Any, ctx).await,
        InsightsCommand::Declining(args) => changes(args, Movers::Declining, ctx).await,
        InsightsCommand::Rising(args) => changes(args, Movers::Rising, ctx).await,
        InsightsCommand::StrikingDistance(args) => striking_distance(args, ctx).await,
        InsightsCommand::LowCtr(args) => low_ctr(args, ctx).await,
    }
}

/// A query for this scope's search type and filters over `range`.
fn query(scope: &Scope, range: DateRange, dimensions: Vec<Dimension>) -> Query {
    Query {
        range,
        dimensions,
        search_type: scope.search_type,
        filters: scope.filters(),
        aggregation: None,
        data_state: None,
    }
}

/// How many rows to read for an analysis: one full page, or everything.
fn scan_limit(all: bool) -> Option<u32> {
    (!all).then_some(PAGE_SIZE)
}

async fn fetch(client: &Client, site: &str, query: &Query, limit: Option<u32>) -> Result<Rows> {
    client.search_analytics(site, query, 0, limit).await
}

fn period_json(range: DateRange) -> Value {
    json!({ "startDate": range.start.to_string(), "endDate": range.end.to_string() })
}

async fn changes(args: ChangeArgs, movers: Movers, ctx: &Context) -> Result<()> {
    let by = args.by.unwrap_or(match movers {
        Movers::Any => By::Query,
        Movers::Declining | Movers::Rising => By::Page,
    });
    let dimensions = vec![by.dimension()];
    let current = args.scope.dates.resolve(dates::today(), false)?;
    let previous = match args.vs {
        Versus::Previous => current.previous(),
        Versus::Year => current.year_earlier(),
    };

    let client = ctx.client()?;
    let site = ctx.site(&client).await?;
    let scan = scan_limit(args.all);
    let rows_now = query(&args.scope, current, dimensions.clone());
    let rows_before = query(&args.scope, previous, dimensions.clone());
    let total_now = query(&args.scope, current, Vec::new());
    let total_before = query(&args.scope, previous, Vec::new());
    // Totals are fetched separately: summing rows would miss anonymised
    // queries and anything beyond the rows scanned.
    let (now, before, total_now, total_before) = tokio::try_join!(
        fetch(&client, &site, &rows_now, scan),
        fetch(&client, &site, &rows_before, scan),
        fetch(&client, &site, &total_now, Some(1)),
        fetch(&client, &site, &total_before, Some(1)),
    )?;

    let partial = !now.exhausted || !before.exhausted;
    let mut ranked = insights::rank(insights::join(now.rows, before.rows), args.sort, movers);
    let matched = ranked.len();
    ranked.truncate(args.limit);
    let totals = insights::join(total_now.rows, total_before.rows)
        .into_iter()
        .next()
        .unwrap_or(Change {
            keys: Vec::new(),
            current: None,
            previous: None,
        })
        .to_json(&[]);

    let document = json!({
        "site": site,
        "by": by.dimension().api_name(),
        "sortedBy": metric_name(args.sort),
        "period": period_json(current),
        "comparedWith": period_json(previous),
        "totals": totals,
        "rowCount": ranked.len(),
        "truncated": ranked.len() < matched,
        "rows": ranked.iter().map(|change| change.to_json(&dimensions)).collect::<Vec<_>>(),
    });

    let mut report = Report::new(document)
        .field("Property", site)
        .field("Period", current.to_string())
        .field("Compared with", previous.to_string())
        .field("Clicks", total_line(&totals, "clicks", Kind::Int))
        .field("Impressions", total_line(&totals, "impressions", Kind::Int))
        .field("CTR", total_line(&totals, "ctr", Kind::Percent))
        .field("Position", total_line(&totals, "position", Kind::Decimal))
        .table(change_columns(by.dimension()), "/rows");

    if ranked.is_empty() {
        report = report.note(match movers {
            Movers::Any => "No data in either period.",
            Movers::Declining => "Nothing declined between the two periods.",
            Movers::Rising => "Nothing rose between the two periods.",
        });
    }
    if partial {
        report = report.note(
            "Compared the 25,000 rows with the most clicks in each period. Items outside \
             that set can wrongly appear as new or lost; pass --all to compare every row.",
        );
    }
    ctx.emit(&report)
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Clicks => "clicks",
        Metric::Impressions => "impressions",
        Metric::Ctr => "ctr",
        Metric::Position => "position",
    }
}

/// `1,350 (was 1,200, +150)` for one metric of the totals.
fn total_line(totals: &Value, metric: &str, kind: Kind) -> String {
    let delta_kind = match kind {
        Kind::Int => Kind::IntDelta,
        Kind::Percent => Kind::PercentDelta,
        _ => Kind::DecimalDelta,
    };
    let show = |key: String, kind: Kind| crate::output::display_cell(totals.get(&key), kind);
    format!(
        "{} (was {}, {})",
        show(metric.to_owned(), kind),
        show(format!("{metric}Previous"), kind),
        show(format!("{metric}Change"), delta_kind),
    )
}

fn change_columns(dimension: Dimension) -> Vec<Column> {
    vec![
        Column::text(dimension.api_name(), dimension.title()),
        Column::new("clicks", "Clicks", Kind::Int),
        Column::new("clicksChange", "+/-", Kind::IntDelta),
        Column::new("impressions", "Impr.", Kind::Int),
        Column::new("impressionsChange", "+/-", Kind::IntDelta),
        Column::new("ctr", "CTR", Kind::Percent),
        Column::new("ctrChange", "+/-", Kind::PercentDelta),
        Column::new("position", "Pos.", Kind::Decimal),
        Column::new("positionChange", "+/-", Kind::DecimalDelta),
    ]
}

async fn striking_distance(args: StrikingArgs, ctx: &Context) -> Result<()> {
    if args.min_position > args.max_position {
        return Err(Error::usage(format!(
            "--min-position {} is greater than --max-position {}",
            args.min_position, args.max_position
        )));
    }
    let mut dimensions = vec![Dimension::Query];
    if args.with_page {
        dimensions.push(Dimension::Page);
    }
    let range = args.scope.dates.resolve(dates::today(), false)?;

    let client = ctx.client()?;
    let site = ctx.site(&client).await?;
    let request = query(&args.scope, range, dimensions.clone());
    let fetched = fetch(&client, &site, &request, scan_limit(args.all)).await?;

    let scanned = fetched.rows.len();
    let mut found = insights::striking_distance(
        fetched.rows,
        args.min_position..=args.max_position,
        f64::from(args.min_impressions),
    );
    let matched = found.len();
    found.truncate(args.limit);

    let document = json!({
        "site": site,
        "startDate": range.start.to_string(),
        "endDate": range.end.to_string(),
        "minPosition": args.min_position,
        "maxPosition": args.max_position,
        "minImpressions": args.min_impressions,
        "rowsScanned": scanned,
        "rowCount": found.len(),
        "truncated": found.len() < matched,
        "rows": found.iter().map(|row| row.to_json(&dimensions)).collect::<Vec<_>>(),
    });
    let mut columns: Vec<Column> = dimensions
        .iter()
        .map(|d| Column::text(d.api_name(), d.title()))
        .collect();
    columns.extend(metric_columns());

    let mut report = Report::new(document)
        .field("Property", site)
        .field("Period", range.to_string())
        .field(
            "Looking for",
            format!(
                "position {} to {}, at least {} impressions",
                args.min_position, args.max_position, args.min_impressions
            ),
        )
        .table(columns, "/rows");
    if found.is_empty() {
        report = report.note("No keywords in that position range with enough impressions.");
    }
    if !fetched.exhausted {
        report = report.note(SCAN_NOTE);
    }
    ctx.emit(&report)
}

/// Google returns rows by clicks, so a capped scan favours what already performs.
const SCAN_NOTE: &str = "Scanned the 25,000 rows with the most clicks. Items with few clicks \
     may be missing; pass --all to scan every row.";

async fn low_ctr(args: LowCtrArgs, ctx: &Context) -> Result<()> {
    let dimension = match args.by {
        Subject::Page => Dimension::Page,
        Subject::Query => Dimension::Query,
    };
    let dimensions = vec![dimension];
    let range = args.scope.dates.resolve(dates::today(), false)?;

    let client = ctx.client()?;
    let site = ctx.site(&client).await?;
    let request = query(&args.scope, range, dimensions.clone());
    let fetched = fetch(&client, &site, &request, scan_limit(args.all)).await?;

    let mut found = insights::low_ctr(
        &fetched.rows,
        args.max_position,
        f64::from(args.min_impressions),
    );
    let matched = found.len();
    found.truncate(args.limit);

    let document = json!({
        "site": site,
        "by": dimension.api_name(),
        "startDate": range.start.to_string(),
        "endDate": range.end.to_string(),
        "maxPosition": args.max_position,
        "minImpressions": args.min_impressions,
        "rowsScanned": fetched.rows.len(),
        "rowCount": found.len(),
        "truncated": found.len() < matched,
        "rows": found.iter().map(|item| item.to_json(&dimensions)).collect::<Vec<_>>(),
    });
    let mut columns = vec![Column::text(dimension.api_name(), dimension.title())];
    columns.extend(metric_columns());
    columns.push(Column::new("expectedCtr", "Expected", Kind::Percent));
    columns.push(Column::new("missedClicks", "Missed clicks", Kind::Int));

    let mut report = Report::new(document)
        .field("Property", site)
        .field("Period", range.to_string())
        .field(
            "Looking for",
            format!(
                "position {} or better, at least {} impressions",
                args.max_position, args.min_impressions
            ),
        )
        .table(columns, "/rows");
    report = report.note(if found.is_empty() {
        "Nothing stands out: no item's CTR is clearly below this property's average for its position."
    } else {
        "Expected CTR is this property's own average at the same position, not an industry \
         benchmark. Review the title and description of the items listed."
    });
    if !fetched.exhausted {
        report = report.note(SCAN_NOTE);
    }
    ctx.emit(&report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_lines_show_current_previous_and_signed_change() {
        let totals = json!({
            "clicks": 1350, "clicksPrevious": 1200, "clicksChange": 150,
            "ctr": 0.031, "ctrPrevious": 0.0355, "ctrChange": -0.0045,
            "position": 12.4, "positionPrevious": 14.0, "positionChange": -1.6,
        });
        assert_eq!(
            total_line(&totals, "clicks", Kind::Int),
            "1,350 (was 1,200, +150)"
        );
        assert_eq!(
            total_line(&totals, "ctr", Kind::Percent),
            "3.10% (was 3.55%, -0.45%)"
        );
        assert_eq!(
            total_line(&totals, "position", Kind::Decimal),
            "12.4 (was 14.0, -1.6)"
        );
    }

    #[test]
    fn totals_without_data_show_placeholders_for_position() {
        let totals = json!({
            "clicks": 0, "clicksPrevious": 0, "clicksChange": 0,
            "position": null, "positionPrevious": null, "positionChange": null,
        });
        assert_eq!(total_line(&totals, "clicks", Kind::Int), "0 (was 0, 0)");
        assert_eq!(
            total_line(&totals, "position", Kind::Decimal),
            "- (was -, -)"
        );
    }
}
