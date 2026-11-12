//! Search Analytics: clicks, impressions, CTR and position.

use std::str::FromStr;

use clap::ValueEnum;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Client, encode_segment, lower_camel};
use crate::auth::Api;
use crate::dates::DateRange;
use crate::error::Result;
use crate::output::round_to;

/// The most rows Google returns for one request.
pub const PAGE_SIZE: u32 = 25_000;

/// A way to group search data. Each value is one column of the row key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum Dimension {
    /// The search query (keyword)
    Query,
    /// The URL that appeared in results
    Page,
    /// Country, as a lowercase ISO 3166-1 alpha-3 code
    Country,
    /// DESKTOP, MOBILE, or TABLET
    Device,
    /// Day, in Pacific time
    Date,
    /// Search result feature, e.g. rich results
    #[value(alias = "searchAppearance", alias = "appearance")]
    SearchAppearance,
    /// Hour; only recent days, and implies --data-state hourly-all
    Hour,
}

impl Dimension {
    /// The spelling used by the API, and as the key in `gsc` output rows.
    pub fn api_name(self) -> &'static str {
        match self {
            Dimension::Query => "query",
            Dimension::Page => "page",
            Dimension::Country => "country",
            Dimension::Device => "device",
            Dimension::Date => "date",
            Dimension::SearchAppearance => "searchAppearance",
            Dimension::Hour => "hour",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Dimension::Query => "Query",
            Dimension::Page => "Page",
            Dimension::Country => "Country",
            Dimension::Device => "Device",
            Dimension::Date => "Date",
            Dimension::SearchAppearance => "Appearance",
            Dimension::Hour => "Hour",
        }
    }

    /// Whether rows come back in chronological order rather than by clicks.
    pub fn is_time(self) -> bool {
        matches!(self, Dimension::Date | Dimension::Hour)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SearchType {
    #[default]
    Web,
    Image,
    Video,
    /// The News tab in Google Search
    News,
    Discover,
    /// news.google.com and the Google News app
    #[value(alias = "googleNews")]
    GoogleNews,
}

impl SearchType {
    pub fn api_name(self) -> &'static str {
        match self {
            SearchType::Web => "web",
            SearchType::Image => "image",
            SearchType::Video => "video",
            SearchType::News => "news",
            SearchType::Discover => "discover",
            SearchType::GoogleNews => "googleNews",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Aggregation {
    /// Let Google choose (by property unless grouping or filtering by page)
    Auto,
    /// Count each property once per query, as the Search Console totals do
    #[value(alias = "byProperty")]
    ByProperty,
    /// Count every URL separately
    #[value(alias = "byPage")]
    ByPage,
    #[value(alias = "byNewsShowcasePanel")]
    ByNewsShowcasePanel,
}

impl Aggregation {
    pub fn api_name(self) -> &'static str {
        match self {
            Aggregation::Auto => "auto",
            Aggregation::ByProperty => "byProperty",
            Aggregation::ByPage => "byPage",
            Aggregation::ByNewsShowcasePanel => "byNewsShowcasePanel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DataState {
    /// Only finalised data (the default)
    Final,
    /// Include fresh data from the last few days, which may still change
    All,
    /// Hourly data, including fresh data
    #[value(alias = "hourly_all", alias = "hourlyAll")]
    HourlyAll,
}

impl DataState {
    pub fn api_name(self) -> &'static str {
        match self {
            DataState::Final => "final",
            DataState::All => "all",
            DataState::HourlyAll => "hourly_all",
        }
    }

    pub fn includes_fresh(self) -> bool {
        !matches!(self, DataState::Final)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equals,
    NotEquals,
    Contains,
    NotContains,
    IncludingRegex,
    ExcludingRegex,
}

impl Operator {
    pub fn api_name(self) -> &'static str {
        match self {
            Operator::Equals => "equals",
            Operator::NotEquals => "notEquals",
            Operator::Contains => "contains",
            Operator::NotContains => "notContains",
            Operator::IncludingRegex => "includingRegex",
            Operator::ExcludingRegex => "excludingRegex",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let normalised: String = text
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match normalised.as_str() {
            "equals" | "is" | "=" | "==" => Operator::Equals,
            "notequals" | "isnot" | "!=" => Operator::NotEquals,
            "contains" => Operator::Contains,
            "notcontains" => Operator::NotContains,
            "regex" | "matches" | "includingregex" => Operator::IncludingRegex,
            "notregex" | "notmatches" | "excludingregex" => Operator::ExcludingRegex,
            _ => return None,
        })
    }
}

/// A condition on a dimension. All filters in a query must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub dimension: Dimension,
    pub operator: Operator,
    pub expression: String,
}

impl Filter {
    pub fn new(dimension: Dimension, operator: Operator, expression: &str) -> Self {
        // Google matches these two dimensions case-sensitively against fixed
        // vocabularies: lowercase country codes and uppercase device names.
        let expression = match dimension {
            Dimension::Country => expression.to_ascii_lowercase(),
            Dimension::Device => expression.to_ascii_uppercase(),
            _ => expression.to_owned(),
        };
        Self {
            dimension,
            operator,
            expression,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "dimension": self.dimension.api_name(),
            "operator": self.operator.api_name(),
            "expression": self.expression,
        })
    }
}

/// Parses `<dimension> <operator> <value>`, e.g. `query contains running shoes`.
impl FromStr for Filter {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const EXAMPLE: &str =
            "expected `<dimension> <operator> <value>`, e.g. `query contains shoes`";

        let mut parts = text.trim().splitn(3, char::is_whitespace);
        let (Some(dimension), Some(operator), Some(expression)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(EXAMPLE.to_owned());
        };
        let expression = expression.trim();
        if expression.is_empty() {
            return Err(EXAMPLE.to_owned());
        }

        let dimension = Dimension::from_str(dimension, true)
            .ok()
            .filter(|d| !d.is_time())
            .ok_or_else(|| {
                format!(
                    "cannot filter on `{dimension}`; use query, page, country, device, \
                     or search-appearance"
                )
            })?;
        let operator = Operator::parse(operator).ok_or_else(|| {
            format!(
                "unknown operator `{operator}`; use equals, not-equals, contains, \
                 not-contains, regex, or not-regex"
            )
        })?;
        Ok(Filter::new(dimension, operator, expression))
    }
}

/// Everything that defines a search analytics query except paging.
#[derive(Debug, Clone)]
pub struct Query {
    pub range: DateRange,
    pub dimensions: Vec<Dimension>,
    pub search_type: SearchType,
    pub filters: Vec<Filter>,
    pub aggregation: Option<Aggregation>,
    pub data_state: Option<DataState>,
}

impl Query {
    /// A query for `range` with no grouping or filters: the period's totals.
    pub fn totals(range: DateRange) -> Self {
        Self {
            range,
            dimensions: Vec::new(),
            search_type: SearchType::default(),
            filters: Vec::new(),
            aggregation: None,
            data_state: None,
        }
    }

    pub fn body(&self, start_row: u32, row_limit: u32) -> Value {
        let mut body = Map::new();
        body.insert("startDate".into(), json!(self.range.start.to_string()));
        body.insert("endDate".into(), json!(self.range.end.to_string()));
        body.insert("type".into(), json!(self.search_type.api_name()));
        if !self.dimensions.is_empty() {
            let names: Vec<&str> = self.dimensions.iter().map(|d| d.api_name()).collect();
            body.insert("dimensions".into(), json!(names));
        }
        if !self.filters.is_empty() {
            let filters: Vec<Value> = self.filters.iter().map(Filter::to_json).collect();
            body.insert(
                "dimensionFilterGroups".into(),
                json!([{ "groupType": "and", "filters": filters }]),
            );
        }
        if let Some(aggregation) = self.aggregation {
            body.insert("aggregationType".into(), json!(aggregation.api_name()));
        }
        if let Some(data_state) = self.data_state {
            body.insert("dataState".into(), json!(data_state.api_name()));
        }
        body.insert("rowLimit".into(), json!(row_limit));
        body.insert("startRow".into(), json!(start_row));
        Value::Object(body)
    }
}

/// One row of results: the dimension values and the four metrics.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Row {
    /// One value per requested dimension, in the same order.
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub clicks: f64,
    #[serde(default)]
    pub impressions: f64,
    /// Click-through rate as a fraction between 0 and 1.
    #[serde(default)]
    pub ctr: f64,
    /// Average position; 1 is the top result.
    #[serde(default)]
    pub position: f64,
}

impl Row {
    /// The row as a flat object keyed by dimension name, with metrics rounded
    /// to the precision that is actually meaningful.
    pub fn to_json(&self, dimensions: &[Dimension]) -> Value {
        let mut object = Map::new();
        for (dimension, key) in dimensions.iter().zip(&self.keys) {
            object.insert(dimension.api_name().into(), json!(key));
        }
        object.insert("clicks".into(), json!(self.clicks.round() as i64));
        object.insert("impressions".into(), json!(self.impressions.round() as i64));
        object.insert("ctr".into(), json!(round_to(self.ctr, 4)));
        object.insert("position".into(), json!(round_to(self.position, 2)));
        Value::Object(object)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    #[serde(default)]
    rows: Vec<Row>,
    #[serde(default)]
    response_aggregation_type: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

/// The rows fetched for a query, possibly across several requests.
#[derive(Debug, Clone, Default)]
pub struct Rows {
    pub rows: Vec<Row>,
    /// How Google aggregated the data: `byProperty` or `byPage`.
    pub aggregation: Option<String>,
    /// Present when the range includes days that are still being processed.
    pub metadata: Option<Value>,
    /// False when the limit was reached while Google still had rows to give.
    pub exhausted: bool,
}

impl Client {
    /// Fetch up to `limit` rows (`None` for all of them), skipping `start_row`.
    ///
    /// Google caps a single response at [`PAGE_SIZE`] rows, so larger requests
    /// are split into consecutive pages.
    pub async fn search_analytics(
        &self,
        site: &str,
        query: &Query,
        start_row: u32,
        limit: Option<u32>,
    ) -> Result<Rows> {
        let path = format!(
            "/webmasters/v3/sites/{}/searchAnalytics/query",
            encode_segment(site)
        );
        let mut result = Rows::default();
        loop {
            let fetched = result.rows.len() as u32;
            let page_limit = match limit {
                Some(limit) => (limit - fetched).min(PAGE_SIZE),
                None => PAGE_SIZE,
            };
            if page_limit == 0 {
                break;
            }
            let body = query.body(start_row + fetched, page_limit);
            let page: QueryResponse = self
                .call(Api::SearchConsole, Method::POST, &path, &[], Some(&body))
                .await?;

            let received = page.rows.len() as u32;
            result.rows.extend(page.rows);
            if result.aggregation.is_none() {
                result.aggregation = page.response_aggregation_type.as_deref().map(lower_camel);
            }
            if result.metadata.is_none() {
                result.metadata = page.metadata;
            }
            // A short page means Google has nothing more for this query.
            if received < page_limit {
                result.exhausted = true;
                break;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn range() -> DateRange {
        DateRange {
            start: NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        }
    }

    #[test]
    fn totals_query_sends_only_what_is_needed() {
        assert_eq!(
            Query::totals(range()).body(0, 1),
            json!({
                "startDate": "2026-10-13",
                "endDate": "2026-11-09",
                "type": "web",
                "rowLimit": 1,
                "startRow": 0,
            })
        );
    }

    #[test]
    fn full_query_uses_the_documented_api_spellings() {
        let query = Query {
            range: range(),
            dimensions: vec![Dimension::Query, Dimension::SearchAppearance],
            search_type: SearchType::GoogleNews,
            filters: vec![
                Filter::new(Dimension::Country, Operator::Equals, "USA"),
                Filter::new(
                    Dimension::Page,
                    Operator::IncludingRegex,
                    "^https://x/blog/",
                ),
            ],
            aggregation: Some(Aggregation::ByPage),
            data_state: Some(DataState::HourlyAll),
        };
        assert_eq!(
            query.body(25_000, 500),
            json!({
                "startDate": "2026-10-13",
                "endDate": "2026-11-09",
                "type": "googleNews",
                "dimensions": ["query", "searchAppearance"],
                "dimensionFilterGroups": [{ "groupType": "and", "filters": [
                    { "dimension": "country", "operator": "equals", "expression": "usa" },
                    { "dimension": "page", "operator": "includingRegex",
                      "expression": "^https://x/blog/" },
                ]}],
                "aggregationType": "byPage",
                "dataState": "hourly_all",
                "rowLimit": 500,
                "startRow": 25_000,
            })
        );
    }

    #[test]
    fn filters_parse_from_readable_text() {
        let parsed: Filter = "query contains running shoes".parse().unwrap();
        assert_eq!(
            parsed,
            Filter {
                dimension: Dimension::Query,
                operator: Operator::Contains,
                expression: "running shoes".into(),
            }
        );

        let cases = [
            ("page equals https://example.com/a b", Operator::Equals),
            ("page = https://example.com/", Operator::Equals),
            ("query != brand", Operator::NotEquals),
            ("query not-equals brand", Operator::NotEquals),
            ("query notContains brand", Operator::NotContains),
            ("query not_contains brand", Operator::NotContains),
            ("query regex ^how (to|do)", Operator::IncludingRegex),
            ("query includingRegex ^how", Operator::IncludingRegex),
            ("query not-regex brand|name", Operator::ExcludingRegex),
            ("QUERY EXCLUDINGREGEX x", Operator::ExcludingRegex),
        ];
        for (text, operator) in cases {
            assert_eq!(text.parse::<Filter>().unwrap().operator, operator, "{text}");
        }

        assert_eq!(
            "search-appearance equals VIDEO"
                .parse::<Filter>()
                .unwrap()
                .dimension,
            Dimension::SearchAppearance
        );
    }

    #[test]
    fn country_and_device_values_are_normalised() {
        assert_eq!(
            "country equals USA".parse::<Filter>().unwrap().expression,
            "usa"
        );
        assert_eq!(
            "device equals mobile".parse::<Filter>().unwrap().expression,
            "MOBILE"
        );
        assert_eq!(
            "query equals MiXed Case"
                .parse::<Filter>()
                .unwrap()
                .expression,
            "MiXed Case"
        );
    }

    #[test]
    fn bad_filters_explain_themselves() {
        assert!("query".parse::<Filter>().unwrap_err().contains("expected"));
        assert!(
            "query contains"
                .parse::<Filter>()
                .unwrap_err()
                .contains("expected")
        );
        assert!(
            "query contains   "
                .parse::<Filter>()
                .unwrap_err()
                .contains("expected")
        );
        assert!(
            "date equals 2026-01-01"
                .parse::<Filter>()
                .unwrap_err()
                .contains("cannot filter on `date`")
        );
        assert!(
            "colour equals red"
                .parse::<Filter>()
                .unwrap_err()
                .contains("cannot filter on `colour`")
        );
        assert!(
            "query resembles x"
                .parse::<Filter>()
                .unwrap_err()
                .contains("unknown operator `resembles`")
        );
    }

    #[test]
    fn rows_become_flat_objects_with_rounded_metrics() {
        let row = Row {
            keys: vec!["rust cli".into(), "usa".into()],
            clicks: 12.0,
            impressions: 340.0,
            ctr: 0.035_294_117_647,
            position: 8.238_461_538,
        };
        assert_eq!(
            row.to_json(&[Dimension::Query, Dimension::Country]),
            json!({
                "query": "rust cli",
                "country": "usa",
                "clicks": 12,
                "impressions": 340,
                "ctr": 0.0353,
                "position": 8.24,
            })
        );
        assert_eq!(
            row.to_json(&[]),
            json!({ "clicks": 12, "impressions": 340, "ctr": 0.0353, "position": 8.24 })
        );
    }
}
