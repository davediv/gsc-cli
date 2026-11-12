//! Analyses built on search analytics rows.
//!
//! Everything here is a pure function over rows already fetched, so the
//! logic can be tested without a network.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::RangeInclusive;

use serde_json::{Map, Value, json};

use crate::api::analytics::{Dimension, Metric, Row};
use crate::output::round_to;

/// One item's metrics in two periods. Either side is missing when the item
/// had no data (or was outside the rows fetched) in that period.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub keys: Vec<String>,
    pub current: Option<Row>,
    pub previous: Option<Row>,
}

impl Change {
    /// How much better the current period is on `metric`.
    ///
    /// Positive always means improvement: more clicks, impressions or CTR,
    /// or a position closer to 1. Position needs data in both periods, so
    /// the gain is `None` for items that are new or gone.
    pub fn gain(&self, metric: Metric) -> Option<f64> {
        let value = |row: &Option<Row>| row.as_ref().map(|row| metric.of(row));
        match metric {
            Metric::Position => Some(value(&self.previous)? - value(&self.current)?),
            _ => Some(value(&self.current).unwrap_or(0.0) - value(&self.previous).unwrap_or(0.0)),
        }
    }

    /// A flat object: the keys, then each metric as current, previous, change.
    ///
    /// `*Change` is current minus previous, so a negative `positionChange`
    /// means the item moved up the results.
    pub fn to_json(&self, dimensions: &[Dimension]) -> Value {
        let mut object = Map::new();
        for (dimension, key) in dimensions.iter().zip(&self.keys) {
            object.insert(dimension.api_name().into(), json!(key));
        }
        let get = |row: &Option<Row>, metric: Metric| row.as_ref().map(|row| metric.of(row));

        for (name, metric, places) in [
            ("clicks", Metric::Clicks, 0),
            ("impressions", Metric::Impressions, 0),
            ("ctr", Metric::Ctr, 4),
        ] {
            let current = get(&self.current, metric).unwrap_or(0.0);
            let previous = get(&self.previous, metric).unwrap_or(0.0);
            object.insert(name.into(), number(current, places));
            object.insert(format!("{name}Previous"), number(previous, places));
            object.insert(format!("{name}Change"), number(current - previous, places));
        }

        // An item with no data has no position; zero would read as "ranked first".
        let current = get(&self.current, Metric::Position);
        let previous = get(&self.previous, Metric::Position);
        let optional = |value: Option<f64>| value.map_or(Value::Null, |v| number(v, 2));
        object.insert("position".into(), optional(current));
        object.insert("positionPrevious".into(), optional(previous));
        object.insert(
            "positionChange".into(),
            optional(current.zip(previous).map(|(now, before)| now - before)),
        );
        Value::Object(object)
    }
}

/// Counts are whole numbers; rates and positions keep a few decimals.
fn number(value: f64, places: u32) -> Value {
    if places == 0 {
        json!(value.round() as i64)
    } else {
        json!(round_to(value, places))
    }
}

/// Pair up the rows of two periods by their keys.
///
/// Items keep the order of the current period (by clicks, as Google returns
/// them), followed by items that only existed in the previous one.
pub fn join(current: Vec<Row>, previous: Vec<Row>) -> Vec<Change> {
    let mut before: HashMap<Vec<String>, Row> = previous
        .into_iter()
        .map(|row| (row.keys.clone(), row))
        .collect();
    let mut changes: Vec<Change> = current
        .into_iter()
        .map(|row| Change {
            keys: row.keys.clone(),
            previous: before.remove(&row.keys),
            current: Some(row),
        })
        .collect();

    let mut gone: Vec<Row> = before.into_values().collect();
    gone.sort_by(|a, b| {
        b.clicks
            .total_cmp(&a.clicks)
            .then_with(|| a.keys.cmp(&b.keys))
    });
    changes.extend(gone.into_iter().map(|row| Change {
        keys: row.keys.clone(),
        current: None,
        previous: Some(row),
    }));
    changes
}

/// Which changes to keep when ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movers {
    /// Everything, biggest change in either direction first.
    Any,
    /// Only items that got worse, worst first.
    Declining,
    /// Only items that improved, best first.
    Rising,
}

/// Select and order changes by how much `metric` moved.
pub fn rank(mut changes: Vec<Change>, metric: Metric, movers: Movers) -> Vec<Change> {
    match movers {
        Movers::Any => {}
        Movers::Declining => changes.retain(|c| c.gain(metric).is_some_and(|gain| gain < 0.0)),
        Movers::Rising => changes.retain(|c| c.gain(metric).is_some_and(|gain| gain > 0.0)),
    }
    // Items whose gain is unknown sort last. The sort is stable, so ties keep
    // their original (by clicks) order.
    changes.sort_by(|a, b| match (a.gain(metric), b.gain(metric)) {
        (Some(a), Some(b)) => match movers {
            Movers::Any => b.abs().total_cmp(&a.abs()),
            Movers::Declining => a.total_cmp(&b),
            Movers::Rising => b.total_cmp(&a),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
    changes
}

/// Rows ranking within `positions` with enough impressions to matter,
/// most-seen first. These are the keywords a small push could move onto
/// the first page.
pub fn striking_distance(
    rows: Vec<Row>,
    positions: RangeInclusive<f64>,
    min_impressions: f64,
) -> Vec<Row> {
    let mut selected: Vec<Row> = rows
        .into_iter()
        .filter(|row| positions.contains(&row.position) && row.impressions >= min_impressions)
        .collect();
    selected.sort_by(|a, b| b.impressions.total_cmp(&a.impressions));
    selected
}

/// A row whose CTR is below what its position usually earns.
#[derive(Debug, Clone, PartialEq)]
pub struct Underperformer {
    pub row: Row,
    /// The property's average CTR at this row's position.
    pub expected_ctr: f64,
    /// Clicks the row would have gained at the expected CTR.
    pub missed_clicks: f64,
}

impl Underperformer {
    pub fn to_json(&self, dimensions: &[Dimension]) -> Value {
        let mut object = self.row.to_json(dimensions);
        object["expectedCtr"] = json!(round_to(self.expected_ctr, 4));
        object["missedClicks"] = json!(self.missed_clicks.round() as i64);
        object
    }
}

/// Find rows that earn fewer clicks than their ranking should bring.
///
/// The benchmark is the property's own data: the impression-weighted CTR of
/// all rows at the same rounded position. That makes it self-calibrating
/// across industries and search types, at the cost of being noisy for
/// positions where the property has little data.
pub fn low_ctr(rows: &[Row], max_position: f64, min_impressions: f64) -> Vec<Underperformer> {
    let bucket = |position: f64| position.round().max(1.0) as u32;

    // Position → (clicks, impressions) across every row at that position.
    let mut totals: HashMap<u32, (f64, f64)> = HashMap::new();
    for row in rows {
        let entry = totals.entry(bucket(row.position)).or_default();
        entry.0 += row.clicks;
        entry.1 += row.impressions;
    }

    let mut found: Vec<Underperformer> = rows
        .iter()
        .filter(|row| row.position <= max_position && row.impressions >= min_impressions)
        .filter_map(|row| {
            let (clicks, impressions) = totals[&bucket(row.position)];
            let expected_ctr = clicks / impressions;
            let missed_clicks = row.impressions * (expected_ctr - row.ctr);
            // Less than one click short is noise, not an opportunity.
            (missed_clicks >= 1.0).then(|| Underperformer {
                row: row.clone(),
                expected_ctr,
                missed_clicks,
            })
        })
        .collect();
    found.sort_by(|a, b| b.missed_clicks.total_cmp(&a.missed_clicks));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn keys(changes: &[Change]) -> Vec<&str> {
        changes.iter().map(|c| c.keys[0].as_str()).collect()
    }

    fn sample() -> Vec<Change> {
        join(
            vec![
                row("steady", 100.0, 1000.0, 3.0),
                row("grew", 80.0, 900.0, 4.0),
                row("shrank", 20.0, 800.0, 9.0),
                row("new", 15.0, 200.0, 6.0),
            ],
            vec![
                row("steady", 100.0, 1000.0, 3.0),
                row("grew", 30.0, 500.0, 7.0),
                row("shrank", 90.0, 1500.0, 5.0),
                row("lost-small", 5.0, 50.0, 12.0),
                row("lost-big", 40.0, 400.0, 8.0),
            ],
        )
    }

    #[test]
    fn join_keeps_current_order_then_appends_what_disappeared() {
        let changes = sample();
        assert_eq!(
            keys(&changes),
            ["steady", "grew", "shrank", "new", "lost-big", "lost-small"]
        );
        assert!(changes[3].previous.is_none());
        assert!(changes[4].current.is_none());
    }

    #[test]
    fn gain_is_positive_for_improvement_on_every_metric() {
        let changes = sample();
        let grew = &changes[1];
        assert_eq!(grew.gain(Metric::Clicks), Some(50.0));
        assert_eq!(grew.gain(Metric::Impressions), Some(400.0));
        assert_eq!(
            grew.gain(Metric::Position),
            Some(3.0),
            "moved up from 7 to 4"
        );

        let shrank = &changes[2];
        assert_eq!(shrank.gain(Metric::Clicks), Some(-70.0));
        assert_eq!(
            shrank.gain(Metric::Position),
            Some(-4.0),
            "dropped from 5 to 9"
        );

        let new = &changes[3];
        assert_eq!(
            new.gain(Metric::Clicks),
            Some(15.0),
            "absent counts as zero"
        );
        assert_eq!(
            new.gain(Metric::Position),
            None,
            "no earlier position to compare"
        );
    }

    #[test]
    fn rank_any_orders_by_size_of_change_in_either_direction() {
        let ranked = rank(sample(), Metric::Clicks, Movers::Any);
        assert_eq!(
            keys(&ranked),
            ["shrank", "grew", "lost-big", "new", "lost-small", "steady"]
        );
    }

    #[test]
    fn rank_declining_and_rising_keep_only_their_direction() {
        let declining = rank(sample(), Metric::Clicks, Movers::Declining);
        assert_eq!(keys(&declining), ["shrank", "lost-big", "lost-small"]);

        let rising = rank(sample(), Metric::Clicks, Movers::Rising);
        assert_eq!(keys(&rising), ["grew", "new"]);
    }

    #[test]
    fn ranking_by_position_ignores_items_without_both_positions() {
        let declining = rank(sample(), Metric::Position, Movers::Declining);
        assert_eq!(keys(&declining), ["shrank"]);
        let rising = rank(sample(), Metric::Position, Movers::Rising);
        assert_eq!(keys(&rising), ["grew"]);

        let any = rank(sample(), Metric::Position, Movers::Any);
        assert_eq!(
            keys(&any),
            ["shrank", "grew", "steady", "new", "lost-big", "lost-small"],
            "unknown gains go last"
        );
    }

    #[test]
    fn change_json_has_current_previous_and_difference() {
        let changes = sample();
        assert_eq!(
            changes[1].to_json(&[Dimension::Query]),
            json!({
                "query": "grew",
                "clicks": 80, "clicksPrevious": 30, "clicksChange": 50,
                "impressions": 900, "impressionsPrevious": 500, "impressionsChange": 400,
                "ctr": 0.0889, "ctrPrevious": 0.06, "ctrChange": 0.0289,
                "position": 4.0, "positionPrevious": 7.0, "positionChange": -3.0,
            })
        );
    }

    #[test]
    fn missing_periods_are_zero_counts_and_null_positions() {
        let changes = sample();
        let new = changes[3].to_json(&[Dimension::Page]);
        assert_eq!(new["page"], "new");
        assert_eq!(new["clicksPrevious"], 0);
        assert_eq!(new["clicksChange"], 15);
        assert_eq!(new["position"], 6.0);
        assert_eq!(new["positionPrevious"], Value::Null);
        assert_eq!(new["positionChange"], Value::Null);

        let lost = changes[4].to_json(&[Dimension::Page]);
        assert_eq!(lost["clicks"], 0);
        assert_eq!(lost["clicksChange"], -40);
        assert_eq!(lost["position"], Value::Null);
    }

    #[test]
    fn totals_join_on_the_empty_key() {
        let total = |clicks| Row {
            keys: vec![],
            clicks,
            impressions: 1000.0,
            ctr: clicks / 1000.0,
            position: 10.0,
        };
        let changes = join(vec![total(120.0)], vec![total(100.0)]);
        assert_eq!(changes.len(), 1);
        let json = changes[0].to_json(&[]);
        assert_eq!(json["clicksChange"], 20);
        assert_eq!(json["ctrChange"], 0.02);
    }

    #[test]
    fn striking_distance_filters_by_position_and_impressions() {
        let rows = vec![
            row("already top", 500.0, 5000.0, 2.1),
            row("page two, popular", 12.0, 3000.0, 11.4),
            row("edge of page one", 30.0, 900.0, 8.0),
            row("page two, rare", 0.0, 4.0, 14.0),
            row("too deep", 1.0, 2000.0, 35.0),
            row("upper bound", 2.0, 100.0, 20.0),
        ];
        let found = striking_distance(rows, 8.0..=20.0, 10.0);
        let names: Vec<&str> = found.iter().map(|r| r.keys[0].as_str()).collect();
        assert_eq!(
            names,
            ["page two, popular", "edge of page one", "upper bound"],
            "bounds are inclusive; most impressions first"
        );
    }

    #[test]
    fn low_ctr_compares_each_row_with_its_position_peers() {
        let rows = vec![
            // Position 3: two strong rows and one weak one.
            row("strong a", 100.0, 1000.0, 3.2),
            row("strong b", 90.0, 1000.0, 2.8),
            row("weak at three", 10.0, 1000.0, 3.0),
            // Position 9: everyone converts poorly, so nobody stands out.
            row("nine a", 10.0, 1000.0, 9.1),
            row("nine b", 10.0, 1000.0, 8.9),
        ];
        let found = low_ctr(&rows, 10.0, 100.0);
        assert_eq!(found.len(), 1);
        let weak = &found[0];
        assert_eq!(weak.row.keys[0], "weak at three");
        // Position-3 average: 200 clicks / 3000 impressions.
        assert!((weak.expected_ctr - 200.0 / 3000.0).abs() < 1e-9);
        assert!((weak.missed_clicks - (1000.0 * (200.0 / 3000.0) - 10.0)).abs() < 1e-9);

        assert_eq!(
            weak.to_json(&[Dimension::Page]),
            json!({
                "page": "weak at three",
                "clicks": 10, "impressions": 1000, "ctr": 0.01, "position": 3.0,
                "expectedCtr": 0.0667, "missedClicks": 57,
            })
        );
    }

    #[test]
    fn low_ctr_respects_thresholds_and_orders_by_missed_clicks() {
        let rows = vec![
            row("benchmark", 300.0, 1000.0, 1.0),
            row("big miss", 100.0, 5000.0, 1.2),
            row("small miss", 5.0, 200.0, 0.9),
            row("too few impressions", 0.0, 50.0, 1.0),
            row("benchmark deep", 50.0, 1000.0, 15.0),
            row("too deep to matter", 1.0, 1000.0, 15.2),
        ];
        let found = low_ctr(&rows, 10.0, 100.0);
        let names: Vec<&str> = found.iter().map(|u| u.row.keys[0].as_str()).collect();
        assert_eq!(names, ["big miss", "small miss"]);
        assert!(found[0].missed_clicks > found[1].missed_clicks);
    }

    #[test]
    fn low_ctr_on_no_rows_is_empty() {
        assert!(low_ctr(&[], 10.0, 0.0).is_empty());
    }
}
