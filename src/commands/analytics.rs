//! `gsc analytics` — search performance: keywords, pages, and trends.

use std::cmp::Ordering;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::Context;
use crate::api::analytics::{
    Aggregation, DataState, Dimension, Filter, Metric, Operator, PAGE_SIZE, Query, Row, SearchType,
};
use crate::dates::{self, DateArgs};
use crate::error::Result;
use crate::output::{Column, Kind, Report};

/// Rows returned when neither --limit nor --all is given.
const DEFAULT_LIMIT: u32 = 100;

#[derive(Debug, Subcommand)]
pub enum AnalyticsCommand {
    /// Query with any combination of dimensions
    ///
    /// Without --dimensions this returns the totals for the period.
    Query {
        /// Dimensions to group by, comma-separated, e.g. `query,page`
        #[arg(
            short,
            long,
            value_enum,
            value_delimiter = ',',
            value_name = "DIMENSIONS"
        )]
        dimensions: Vec<Dimension>,

        #[command(flatten)]
        options: QueryOptions,
    },

    /// Top search queries: what people typed to find the site
    #[command(visible_alias = "queries")]
    Keywords(QueryOptions),

    /// Top pages by clicks
    Pages(QueryOptions),

    /// Performance by country
    Countries(QueryOptions),

    /// Performance by device type
    Devices(QueryOptions),

    /// Performance per day, oldest first
    #[command(visible_alias = "daily")]
    Dates(QueryOptions),

    /// Performance by search appearance (rich result types)
    Appearance(QueryOptions),

    /// Total clicks, impressions, CTR and average position for the period
    #[command(visible_alias = "totals")]
    Summary(QueryOptions),
}

impl AnalyticsCommand {
    fn into_parts(self) -> (Vec<Dimension>, QueryOptions) {
        match self {
            AnalyticsCommand::Query {
                dimensions,
                options,
            } => (dimensions, options),
            AnalyticsCommand::Keywords(options) => (vec![Dimension::Query], options),
            AnalyticsCommand::Pages(options) => (vec![Dimension::Page], options),
            AnalyticsCommand::Countries(options) => (vec![Dimension::Country], options),
            AnalyticsCommand::Devices(options) => (vec![Dimension::Device], options),
            AnalyticsCommand::Dates(options) => (vec![Dimension::Date], options),
            AnalyticsCommand::Appearance(options) => (vec![Dimension::SearchAppearance], options),
            AnalyticsCommand::Summary(options) => (Vec::new(), options),
        }
    }
}

/// Best-first direction for a metric: position 1 is the best, so it sorts
/// ascending, while more clicks, impressions, or CTR is better.
fn default_order(metric: Metric) -> Order {
    match metric {
        Metric::Position => Order::Asc,
        _ => Order::Desc,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Order {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Device {
    Desktop,
    Mobile,
    Tablet,
}

impl Device {
    fn api_name(self) -> &'static str {
        match self {
            Device::Desktop => "DESKTOP",
            Device::Mobile => "MOBILE",
            Device::Tablet => "TABLET",
        }
    }
}

/// What to measure: the period, the search type, and any filters.
///
/// Shared by `gsc analytics` and `gsc insights`.
#[derive(Debug, Clone, Args)]
pub struct Scope {
    #[command(flatten)]
    pub dates: DateArgs,

    /// Search type to report on
    #[arg(long = "type", value_enum, value_name = "TYPE", default_value = "web")]
    pub search_type: SearchType,

    /// Condition as `<dimension> <operator> <value>`, e.g. "query contains shoes".
    /// Repeat to combine with AND.
    ///
    /// Dimensions: query, page, country, device, search-appearance.
    /// Operators: equals, not-equals, contains, not-contains, regex, not-regex
    /// (RE2 syntax).
    #[arg(long, value_name = "FILTER")]
    pub filter: Vec<Filter>,

    /// Only queries containing this text
    #[arg(long, value_name = "TEXT")]
    pub query: Option<String>,

    /// Only pages whose URL contains this text
    #[arg(long, value_name = "TEXT")]
    pub page: Option<String>,

    /// Only this country, as an ISO 3166-1 alpha-3 code such as `usa`
    #[arg(long, value_name = "CODE")]
    pub country: Option<String>,

    /// Only this device type
    #[arg(long, value_enum, value_name = "DEVICE")]
    pub device: Option<Device>,
}

/// Options shared by every analytics subcommand.
#[derive(Debug, Args)]
pub struct QueryOptions {
    #[command(flatten)]
    pub scope: Scope,

    /// Which data to include [default: final]
    #[arg(long, value_enum, value_name = "STATE")]
    pub data_state: Option<DataState>,

    /// How to aggregate results [default: chosen by Google]
    #[arg(long, value_enum, value_name = "TYPE")]
    pub aggregation: Option<Aggregation>,

    /// Maximum number of rows [default: 100, or every day for `dates`]
    #[arg(short = 'n', long, value_name = "N", conflicts_with = "all")]
    pub limit: Option<u32>,

    /// Return every row, fetching as many pages as needed
    #[arg(long)]
    pub all: bool,

    /// Skip this many rows before the first one returned. With --sort, rows
    /// are skipped after sorting, so offsets page through the ranking
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub offset: u32,

    /// Sort by a metric instead of Google's order (clicks, highest first).
    /// Ranks up to 25,000 rows; add --all to rank every row.
    #[arg(long, value_enum, value_name = "METRIC")]
    pub sort: Option<Metric>,

    /// Sort direction [default: desc, or asc for position]
    #[arg(long, value_enum, value_name = "ORDER", requires = "sort")]
    pub order: Option<Order>,
}

impl Scope {
    /// Explicit --filter values plus the shorthand flags, as one AND group.
    pub fn filters(&self) -> Vec<Filter> {
        let mut filters = self.filter.clone();
        if let Some(text) = &self.query {
            filters.push(Filter::new(Dimension::Query, Operator::Contains, text));
        }
        if let Some(text) = &self.page {
            filters.push(Filter::new(Dimension::Page, Operator::Contains, text));
        }
        if let Some(code) = &self.country {
            filters.push(Filter::new(Dimension::Country, Operator::Equals, code));
        }
        if let Some(device) = self.device {
            filters.push(Filter::new(
                Dimension::Device,
                Operator::Equals,
                device.api_name(),
            ));
        }
        filters
    }
}

/// What to ask Google for, and which of the returned rows to show.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    /// First row to request, in Google's order (most clicks first).
    start_row: u32,
    /// How many rows to request. `None` fetches every row.
    fetch: Option<u32>,
    /// Rows to drop from the front once they are in their final order.
    skip: usize,
    /// How many rows to show. `None` shows everything that is left.
    show: Option<u32>,
}

impl Plan {
    fn new(options: &QueryOptions, dimensions: &[Dimension], period_days: i64) -> Self {
        let period_days = period_days.clamp(1, i64::from(u32::MAX / 24)) as u32;
        let limit = options.limit.unwrap_or(match dimensions {
            [] => 1,
            // A time series should cover its whole period by default.
            [Dimension::Date] => period_days.max(DEFAULT_LIMIT),
            [Dimension::Hour] => (period_days * 24).max(DEFAULT_LIMIT),
            _ => DEFAULT_LIMIT,
        });
        let show = (!options.all).then_some(limit);

        if options.sort.is_some() {
            // Google only sorts by clicks. To rank by anything else, fetch at
            // least a full page from the top and sort it here. --offset then
            // has to be applied to the sorted rows, not to Google's order.
            Plan {
                start_row: 0,
                fetch: show.map(|limit| limit.saturating_add(options.offset).max(PAGE_SIZE)),
                skip: options.offset as usize,
                show,
            }
        } else {
            Plan {
                start_row: options.offset,
                fetch: show,
                skip: 0,
                show,
            }
        }
    }
}

pub async fn run(command: AnalyticsCommand, ctx: &Context) -> Result<()> {
    let (dimensions, options) = command.into_parts();

    // Hourly rows exist only in the hourly data state.
    let data_state = options.data_state.or_else(|| {
        dimensions
            .contains(&Dimension::Hour)
            .then_some(DataState::HourlyAll)
    });
    let include_fresh = data_state.is_some_and(DataState::includes_fresh);
    let range = options.scope.dates.resolve(dates::today(), include_fresh)?;
    let query = Query {
        range,
        dimensions: dimensions.clone(),
        search_type: options.scope.search_type,
        filters: options.scope.filters(),
        aggregation: options.aggregation,
        data_state,
    };
    let plan = Plan::new(&options, &dimensions, range.days());

    let client = ctx.client()?;
    let site = ctx.site(&client).await?;
    let fetched = client
        .search_analytics(&site, &query, plan.start_row, plan.fetch)
        .await?;

    let mut rows = fetched.rows;
    let scanned = rows.len();
    if let Some(metric) = options.sort {
        sort_rows(
            &mut rows,
            metric,
            options.order.unwrap_or(default_order(metric)),
        );
    }
    let mut rows: Vec<Row> = rows.into_iter().skip(plan.skip).collect();
    let available = rows.len();
    if let Some(show) = plan.show {
        rows.truncate(show as usize);
    }
    // Rows were left out if Google has more than was fetched, or if some of
    // what was fetched is not shown. Ungrouped totals are a single row.
    let truncated = !dimensions.is_empty() && (!fetched.exhausted || rows.len() < available);

    let mut document = json!({
        "site": site,
        "startDate": range.start.to_string(),
        "endDate": range.end.to_string(),
        "type": query.search_type.api_name(),
        "dimensions": dimensions.iter().map(|d| d.api_name()).collect::<Vec<_>>(),
        "rowCount": rows.len(),
        "truncated": truncated,
        "rows": rows.iter().map(|row| row.to_json(&dimensions)).collect::<Vec<_>>(),
    });
    if !query.filters.is_empty() {
        document["filters"] = query
            .filters
            .iter()
            .map(|f| {
                json!({
                    "dimension": f.dimension.api_name(),
                    "operator": f.operator.api_name(),
                    "expression": f.expression,
                })
            })
            .collect();
    }
    if let Some(state) = data_state {
        document["dataState"] = json!(state.api_name());
    }
    if let Some(aggregation) = &fetched.aggregation {
        document["aggregation"] = json!(aggregation);
    }
    if let Some(metadata) = &fetched.metadata {
        document["metadata"] = metadata.clone();
    }

    let mut report = Report::new(document)
        .field("Property", site)
        .field("Period", range.to_string());
    if query.search_type != SearchType::Web {
        report = report.field("Search type", query.search_type.api_name());
    }
    if !query.filters.is_empty() {
        let described: Vec<String> = query
            .filters
            .iter()
            .map(|f| {
                format!(
                    "{} {} {}",
                    f.dimension.api_name(),
                    f.operator.api_name(),
                    f.expression
                )
            })
            .collect();
        report = report.field("Filters", described.join(" AND "));
    }
    report = report.table(columns(&dimensions), "/rows");

    if rows.is_empty() {
        report = report.note(
            "No search data matched. Check the period, the filters, and that the property \
             has data for this search type.",
        );
    } else if options.sort.is_some() && !fetched.exhausted {
        report = report.note(format!(
            "Ranked the {scanned} rows with the most clicks; pass --all to rank every row."
        ));
    } else if truncated {
        let shown = match rows.len() {
            1 => "the first row".to_owned(),
            count => format!("the first {count} rows"),
        };
        report = report.note(format!(
            "Showing {shown}; pass a higher --limit, or --all, for more."
        ));
    }
    if let Some(date) = first_incomplete(&fetched.metadata) {
        report = report.note(format!(
            "Data from {date} onward is still being processed and may change."
        ));
    }
    ctx.emit(&report)
}

fn columns(dimensions: &[Dimension]) -> Vec<Column> {
    let mut columns: Vec<Column> = dimensions
        .iter()
        .map(|d| Column::text(d.api_name(), d.title()))
        .collect();
    columns.extend(metric_columns());
    columns
}

/// The four metric columns every analytics table ends with.
pub fn metric_columns() -> [Column; 4] {
    [
        Column::new("clicks", "Clicks", Kind::Int),
        Column::new("impressions", "Impressions", Kind::Int),
        Column::new("ctr", "CTR", Kind::Percent),
        Column::new("position", "Position", Kind::Decimal),
    ]
}

fn sort_rows(rows: &mut [Row], metric: Metric, order: Order) {
    rows.sort_by(|a, b| {
        let ordering = metric
            .of(a)
            .partial_cmp(&metric.of(b))
            .unwrap_or(Ordering::Equal);
        match order {
            Order::Asc => ordering,
            Order::Desc => ordering.reverse(),
        }
    });
}

/// The first date or hour Google flagged as incomplete, if any.
fn first_incomplete(metadata: &Option<Value>) -> Option<&str> {
    let metadata = metadata.as_ref()?;
    ["firstIncompleteDate", "firstIncompleteHour"]
        .iter()
        .find_map(|key| metadata.get(key)?.as_str())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        options: QueryOptions,
    }

    fn options(args: &[&str]) -> QueryOptions {
        Harness::try_parse_from(std::iter::once("test").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{e}"))
            .options
    }

    fn row(key: &str, clicks: f64, impressions: f64, position: f64) -> Row {
        Row {
            keys: vec![key.into()],
            clicks,
            impressions,
            ctr: if impressions > 0.0 {
                clicks / impressions
            } else {
                0.0
            },
            position,
        }
    }

    fn keys(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.keys[0].as_str()).collect()
    }

    #[test]
    fn default_plan_fetches_exactly_what_is_shown() {
        let plan = Plan::new(&options(&[]), &[Dimension::Query], 28);
        assert_eq!(
            plan,
            Plan {
                start_row: 0,
                fetch: Some(100),
                skip: 0,
                show: Some(100)
            }
        );
        let plan = Plan::new(
            &options(&["--limit", "5", "--offset", "40"]),
            &[Dimension::Query],
            28,
        );
        assert_eq!(
            plan,
            Plan {
                start_row: 40,
                fetch: Some(5),
                skip: 0,
                show: Some(5)
            },
            "without --sort, Google's order is the final order, so it can skip the rows"
        );
    }

    #[test]
    fn sorting_scans_a_full_page_before_cutting_to_the_limit() {
        let plan = Plan::new(
            &options(&["--sort", "impressions", "-n", "20"]),
            &[Dimension::Query],
            28,
        );
        assert_eq!(
            plan,
            Plan {
                start_row: 0,
                fetch: Some(PAGE_SIZE),
                skip: 0,
                show: Some(20)
            }
        );
    }

    #[test]
    fn offset_is_applied_after_sorting_not_before() {
        let plan = Plan::new(
            &options(&["--sort", "impressions", "-n", "10", "--offset", "10"]),
            &[Dimension::Query],
            28,
        );
        assert_eq!(
            plan,
            Plan {
                start_row: 0,
                fetch: Some(PAGE_SIZE),
                skip: 10,
                show: Some(10)
            },
            "page two of a ranking must come from the same ranked set as page one"
        );

        let deep = Plan::new(
            &options(&["--sort", "ctr", "-n", "100", "--offset", "30000"]),
            &[Dimension::Query],
            28,
        );
        assert_eq!(deep.fetch, Some(30_100), "enough rows to reach the window");
    }

    #[test]
    fn all_removes_both_limits() {
        let plan = Plan::new(&options(&["--all"]), &[Dimension::Page], 28);
        assert_eq!(
            plan,
            Plan {
                start_row: 0,
                fetch: None,
                skip: 0,
                show: None
            }
        );
        let sorted = Plan::new(
            &options(&["--all", "--sort", "position", "--offset", "5"]),
            &[Dimension::Page],
            28,
        );
        assert_eq!(
            sorted,
            Plan {
                start_row: 0,
                fetch: None,
                skip: 5,
                show: None
            }
        );
    }

    #[test]
    fn time_series_and_totals_have_their_own_defaults() {
        assert_eq!(
            Plan::new(&options(&[]), &[Dimension::Date], 480).show,
            Some(480),
            "a long period is not cut off at 100 days"
        );
        assert_eq!(
            Plan::new(&options(&[]), &[Dimension::Date], 7).show,
            Some(100)
        );
        assert_eq!(
            Plan::new(&options(&[]), &[Dimension::Hour], 10).show,
            Some(240),
            "ten days of hourly data is 240 rows"
        );
        assert_eq!(Plan::new(&options(&[]), &[], 28).show, Some(1));
        assert_eq!(
            Plan::new(&options(&["-n", "3"]), &[Dimension::Date], 480).show,
            Some(3),
            "an explicit limit always wins"
        );
    }

    #[test]
    fn limit_and_all_are_mutually_exclusive() {
        assert!(Harness::try_parse_from(["test", "--all", "--limit", "5"]).is_err());
        assert!(
            Harness::try_parse_from(["test", "--order", "asc"]).is_err(),
            "--order needs --sort"
        );
    }

    #[test]
    fn shorthand_flags_become_filters() {
        let filters = options(&[
            "--filter",
            "query not-contains brand",
            "--query",
            "shoes",
            "--page",
            "/blog/",
            "--country",
            "GBR",
            "--device",
            "mobile",
        ])
        .scope
        .filters();
        let described: Vec<String> = filters
            .iter()
            .map(|f| {
                format!(
                    "{} {} {}",
                    f.dimension.api_name(),
                    f.operator.api_name(),
                    f.expression
                )
            })
            .collect();
        assert_eq!(
            described,
            [
                "query notContains brand",
                "query contains shoes",
                "page contains /blog/",
                "country equals gbr",
                "device equals MOBILE",
            ]
        );
    }

    #[test]
    fn invalid_filter_is_rejected_at_parse_time() {
        let Err(err) = Harness::try_parse_from(["test", "--filter", "query resembles x"]) else {
            panic!("expected a parse error");
        };
        assert!(err.to_string().contains("unknown operator `resembles`"));
    }

    #[test]
    fn sorts_by_metric_in_either_direction() {
        let mut rows = vec![
            row("a", 10.0, 100.0, 5.0),
            row("b", 30.0, 900.0, 2.0),
            row("c", 20.0, 200.0, 9.0),
        ];
        sort_rows(
            &mut rows,
            Metric::Impressions,
            default_order(Metric::Impressions),
        );
        assert_eq!(keys(&rows), ["b", "c", "a"]);

        sort_rows(&mut rows, Metric::Position, default_order(Metric::Position));
        assert_eq!(keys(&rows), ["b", "a", "c"], "best position first");

        sort_rows(&mut rows, Metric::Clicks, Order::Asc);
        assert_eq!(keys(&rows), ["a", "c", "b"]);

        sort_rows(&mut rows, Metric::Ctr, Order::Desc);
        assert_eq!(keys(&rows), ["a", "c", "b"]);
    }

    #[test]
    fn finds_incomplete_markers_in_metadata() {
        assert_eq!(first_incomplete(&None), None);
        assert_eq!(first_incomplete(&Some(json!({}))), None);
        assert_eq!(
            first_incomplete(&Some(json!({ "firstIncompleteDate": "2026-11-10" }))),
            Some("2026-11-10")
        );
        assert_eq!(
            first_incomplete(&Some(
                json!({ "firstIncompleteHour": "2026-11-12T08:00:00-08:00" })
            )),
            Some("2026-11-12T08:00:00-08:00")
        );
    }
}
