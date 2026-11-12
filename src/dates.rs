//! Date ranges for search analytics.
//!
//! Search Console reports in Pacific time and needs a few days to finalise
//! its numbers, so "the last 28 days" is not simply the 28 days before now.

use chrono::{Days, Duration, Months, NaiveDate, Utc};
use clap::Args;

use crate::error::{Error, Result};

/// Days before today for which final (no longer changing) data exists.
pub const FINAL_DATA_LAG_DAYS: u64 = 3;
/// The period Search Console shows by default.
pub const DEFAULT_PERIOD_DAYS: u32 = 28;

/// An inclusive range of calendar days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    /// Number of days covered, counting both ends.
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }

    /// The period of the same length that ends the day before this one starts.
    pub fn previous(&self) -> DateRange {
        let end = self.start - Duration::days(1);
        DateRange {
            start: end - Duration::days(self.days() - 1),
            end,
        }
    }

    /// The same calendar dates one year earlier.
    pub fn year_earlier(&self) -> DateRange {
        let shift = |date: NaiveDate| date.checked_sub_months(Months::new(12)).unwrap_or(date);
        DateRange {
            start: shift(self.start),
            end: shift(self.end),
        }
    }
}

impl std::fmt::Display for DateRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let days = self.days();
        let unit = if days == 1 { "day" } else { "days" };
        write!(f, "{} to {} ({days} {unit})", self.start, self.end)
    }
}

/// The date options shared by every analytics command.
#[derive(Debug, Clone, Default, Args)]
pub struct DateArgs {
    /// First day: YYYY-MM-DD, `today`, or `yesterday`
    #[arg(long, value_name = "DATE", conflicts_with = "days")]
    pub start: Option<String>,

    /// Last day, inclusive [default: 3 days ago, the newest day with final data]
    #[arg(long, value_name = "DATE")]
    pub end: Option<String>,

    /// Length of the period in days, ending at --end [default: 28]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub days: Option<u32>,
}

impl DateArgs {
    /// Work out the period to query.
    ///
    /// `include_fresh` is set when the caller asked for data that is still
    /// being processed; the default end date then moves up to today.
    pub fn resolve(&self, today: NaiveDate, include_fresh: bool) -> Result<DateRange> {
        let end = match &self.end {
            Some(text) => parse_date(text, today)?,
            None if include_fresh => today,
            None => today - Days::new(FINAL_DATA_LAG_DAYS),
        };
        let start = match &self.start {
            Some(text) => parse_date(text, today)?,
            None => {
                let days = self.days.unwrap_or(DEFAULT_PERIOD_DAYS);
                end.checked_sub_days(Days::new(u64::from(days) - 1))
                    .ok_or_else(|| out_of_range(format!("--days {days}")))?
            }
        };
        if start > end {
            return Err(Error::usage(format!(
                "the start date {start} is after the end date {end}"
            )));
        }
        // Keeping both ends within a sane window also guarantees that the
        // date arithmetic in `previous` and `year_earlier` cannot overflow.
        for date in [start, end] {
            if date < earliest() || date > latest() {
                return Err(out_of_range(date.to_string()));
            }
        }
        Ok(DateRange { start, end })
    }
}

/// Dates outside this window cannot have Search Console data, which goes
/// back about 16 months.
fn earliest() -> NaiveDate {
    NaiveDate::from_ymd_opt(2000, 1, 1).expect("valid date")
}

fn latest() -> NaiveDate {
    NaiveDate::from_ymd_opt(2100, 12, 31).expect("valid date")
}

fn out_of_range(what: String) -> Error {
    Error::usage(format!("{what} is outside the supported range of dates")).with_hint(format!(
        "use dates between {} and {}; Search Console keeps about 16 months of data",
        earliest(),
        latest()
    ))
}

fn parse_date(text: &str, today: NaiveDate) -> Result<NaiveDate> {
    match text.trim().to_ascii_lowercase().as_str() {
        "today" => Ok(today),
        "yesterday" => Ok(today - Days::new(1)),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d").map_err(|_| {
            Error::usage(format!("`{text}` is not a valid date"))
                .with_hint("use YYYY-MM-DD, `today`, or `yesterday`")
        }),
    }
}

/// Today's date in the timezone Search Console reports in (UTC-8).
pub fn today() -> NaiveDate {
    (Utc::now() - Duration::hours(8)).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn args(start: Option<&str>, end: Option<&str>, days: Option<u32>) -> DateArgs {
        DateArgs {
            start: start.map(str::to_owned),
            end: end.map(str::to_owned),
            days,
        }
    }

    fn range(start: &str, end: &str) -> DateRange {
        DateRange {
            start: date(start),
            end: date(end),
        }
    }

    const TODAY: &str = "2026-11-12";

    #[test]
    fn default_is_28_days_of_final_data() {
        let resolved = args(None, None, None).resolve(date(TODAY), false).unwrap();
        assert_eq!(resolved, range("2026-10-13", "2026-11-09"));
        assert_eq!(resolved.days(), 28);
    }

    #[test]
    fn fresh_data_moves_the_default_end_to_today() {
        let resolved = args(None, None, Some(7))
            .resolve(date(TODAY), true)
            .unwrap();
        assert_eq!(resolved, range("2026-11-06", "2026-11-12"));
    }

    #[test]
    fn days_counts_back_from_the_end() {
        let resolved = args(None, None, Some(7))
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(resolved, range("2026-11-03", "2026-11-09"));

        let resolved = args(None, Some("2026-01-31"), Some(31))
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(resolved, range("2026-01-01", "2026-01-31"));

        let one_day = args(None, None, Some(1))
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(one_day.days(), 1);
    }

    #[test]
    fn explicit_dates_and_keywords() {
        let resolved = args(Some("2026-01-01"), Some("2026-03-31"), None)
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(resolved, range("2026-01-01", "2026-03-31"));

        let resolved = args(Some("Yesterday"), Some("today"), None)
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(resolved, range("2026-11-11", "2026-11-12"));

        let open_ended = args(Some("2026-11-01"), None, None)
            .resolve(date(TODAY), false)
            .unwrap();
        assert_eq!(open_ended, range("2026-11-01", "2026-11-09"));
    }

    #[test]
    fn invalid_input_is_a_usage_error() {
        let err = args(Some("11/01/2026"), None, None)
            .resolve(date(TODAY), false)
            .unwrap_err();
        assert_eq!(err.kind, crate::error::ErrorKind::Usage);
        assert!(err.hint.unwrap().contains("YYYY-MM-DD"));

        let err = args(Some("2026-11-10"), Some("2026-11-01"), None)
            .resolve(date(TODAY), false)
            .unwrap_err();
        assert!(err.message.contains("after the end date"));

        assert!(
            args(Some("2026-02-30"), None, None)
                .resolve(date(TODAY), false)
                .is_err()
        );
    }

    #[test]
    fn absurd_periods_are_usage_errors_not_panics() {
        let today = date(TODAY);
        for days in [100_000_000, u32::MAX] {
            let err = args(None, None, Some(days))
                .resolve(today, false)
                .unwrap_err();
            assert_eq!(err.kind, crate::error::ErrorKind::Usage, "--days {days}");
            assert!(err.message.contains("outside the supported range"));
        }
        // Far enough back to be representable, but not meaningful.
        let err = args(None, None, Some(60_000))
            .resolve(today, false)
            .unwrap_err();
        assert!(err.message.starts_with("1862-"), "{}", err.message);

        assert!(
            args(Some("0001-01-01"), None, None)
                .resolve(today, false)
                .is_err()
        );
        assert!(
            args(Some("2026-01-01"), Some("9999-12-31"), None)
                .resolve(today, false)
                .is_err()
        );

        // The widest range accepted still supports both comparisons.
        let widest = args(Some("2000-01-01"), Some("2100-12-31"), None)
            .resolve(today, false)
            .unwrap();
        assert_eq!(widest.previous().days(), widest.days());
        assert_eq!(widest.year_earlier().start, date("1999-01-01"));
    }

    #[test]
    fn previous_period_is_adjacent_and_the_same_length() {
        let current = range("2026-10-13", "2026-11-09");
        let previous = current.previous();
        assert_eq!(previous, range("2026-09-15", "2026-10-12"));
        assert_eq!(previous.days(), current.days());

        assert_eq!(
            range("2026-03-01", "2026-03-01").previous(),
            range("2026-02-28", "2026-02-28")
        );
    }

    #[test]
    fn year_earlier_keeps_calendar_dates_and_clamps_leap_days() {
        assert_eq!(
            range("2026-10-13", "2026-11-09").year_earlier(),
            range("2025-10-13", "2025-11-09")
        );
        assert_eq!(
            range("2024-02-01", "2024-02-29").year_earlier(),
            range("2023-02-01", "2023-02-28")
        );
    }

    #[test]
    fn display_is_human_readable() {
        assert_eq!(
            range("2026-10-13", "2026-11-09").to_string(),
            "2026-10-13 to 2026-11-09 (28 days)"
        );
        assert_eq!(
            range("2026-11-09", "2026-11-09").to_string(),
            "2026-11-09 to 2026-11-09 (1 day)"
        );
    }
}
