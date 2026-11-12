//! Rendering of command results as a table, JSON, or CSV.
//!
//! A command builds one [`Report`]: a JSON document plus a description of how
//! to show it to a person. The same rows back all three formats, so the table
//! a human sees and the JSON an agent parses can never disagree.

use std::io::{self, IsTerminal, Write};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Aligned columns for reading in a terminal
    Table,
    /// One JSON document with every field
    Json,
    /// Comma-separated rows with raw values, the most compact tabular form
    Csv,
}

impl Format {
    /// The default when nothing was requested: a table for people, JSON for pipes.
    pub fn detect() -> Self {
        if io::stdout().is_terminal() {
            Format::Table
        } else {
            Format::Json
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Table => "table",
            Format::Json => "json",
            Format::Csv => "csv",
        }
    }
}

/// How a column's raw JSON value is shown in table output.
///
/// JSON and CSV always carry the raw value; these only affect the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Shown as-is. Arrays are joined with commas.
    Text,
    /// An RFC 3339 timestamp shown as its date: `2026-11-10T03:12:45Z` → `2026-11-10`.
    Date,
    /// Whole number with thousands separators: `12,345`.
    Int,
    /// One decimal place: `8.2`.
    Decimal,
    /// A fraction shown as a percentage: `0.0353` → `3.53%`.
    Percent,
    /// Like [`Kind::Int`] with an explicit sign: `+120`.
    IntDelta,
    /// Like [`Kind::Decimal`] with an explicit sign: `-1.4`.
    DecimalDelta,
    /// Like [`Kind::Percent`] with an explicit sign: `+12.50%`.
    PercentDelta,
}

impl Kind {
    fn is_numeric(self) -> bool {
        !matches!(self, Kind::Text | Kind::Date)
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    /// Key looked up in each row object; also the CSV header.
    pub key: String,
    /// Header shown in table output.
    pub title: String,
    pub kind: Kind,
}

impl Column {
    pub fn new(key: impl Into<String>, title: impl Into<String>, kind: Kind) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            kind,
        }
    }

    pub fn text(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self::new(key, title, Kind::Text)
    }
}

/// A tabular view over an array inside the report's JSON document.
#[derive(Debug, Clone)]
pub struct Table {
    pub columns: Vec<Column>,
    /// JSON pointer (RFC 6901) to the array of row objects, e.g. `/rows`.
    pub rows_at: &'static str,
}

/// The result of a command, ready to be rendered in any format.
#[derive(Debug, Clone)]
pub struct Report {
    json: Value,
    fields: Vec<(String, String)>,
    table: Option<Table>,
    notes: Vec<String>,
}

impl Report {
    pub fn new(json: Value) -> Self {
        Self {
            json,
            fields: Vec::new(),
            table: None,
            notes: Vec::new(),
        }
    }

    /// Add a `Label: value` line shown above the table.
    pub fn field(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((label.into(), value.into()));
        self
    }

    pub fn table(mut self, columns: Vec<Column>, rows_at: &'static str) -> Self {
        self.table = Some(Table { columns, rows_at });
        self
    }

    /// Add a caveat the reader needs to interpret the data correctly.
    ///
    /// Notes are printed under the table, added to JSON output as a `notes`
    /// array, and sent to stderr for CSV so stdout stays parseable.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn json(&self) -> &Value {
        &self.json
    }

    fn rows(&self, table: &Table) -> &[Value] {
        self.json
            .pointer(table.rows_at)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// Where rendered output goes. Split out so tests can capture both streams.
pub struct Streams<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Pretty-print JSON. Enabled when stdout is a terminal.
    pub pretty: bool,
}

/// Print a report to stdout/stderr in the requested format.
///
/// A closed pipe (`gsc ... | head`) is treated as success, not an error.
pub fn print(report: &Report, format: Format) -> io::Result<()> {
    let stdout = io::stdout();
    let pretty = stdout.is_terminal();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut err = io::stderr().lock();
    let result = render(
        report,
        format,
        Streams {
            out: &mut out,
            err: &mut err,
            pretty,
        },
    )
    .and_then(|()| out.flush());
    match result {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

pub fn render(report: &Report, format: Format, streams: Streams<'_>) -> io::Result<()> {
    match format {
        Format::Json => render_json(report, streams),
        Format::Csv => render_csv(report, streams),
        Format::Table => render_table(report, streams),
    }
}

fn render_json(report: &Report, streams: Streams<'_>) -> io::Result<()> {
    let mut doc = report.json.clone();
    if !report.notes.is_empty()
        && let Some(object) = doc.as_object_mut()
    {
        object.insert("notes".into(), Value::from(report.notes.clone()));
    }
    let text = if streams.pretty {
        serde_json::to_string_pretty(&doc)
    } else {
        serde_json::to_string(&doc)
    }
    .map_err(io::Error::other)?;
    writeln!(streams.out, "{text}")
}

fn render_csv(report: &Report, streams: Streams<'_>) -> io::Result<()> {
    let mut writer = csv::Writer::from_writer(streams.out);
    match &report.table {
        Some(table) => {
            writer.write_record(table.columns.iter().map(|c| c.key.as_str()))?;
            for row in report.rows(table) {
                writer.write_record(table.columns.iter().map(|c| raw_cell(row.get(&c.key))))?;
            }
        }
        None => {
            writer.write_record(["field", "value"])?;
            for (label, value) in &report.fields {
                writer.write_record([label, value])?;
            }
        }
    }
    writer.flush()?;
    for note in &report.notes {
        writeln!(streams.err, "note: {note}")?;
    }
    Ok(())
}

fn render_table(report: &Report, streams: Streams<'_>) -> io::Result<()> {
    let out = streams.out;
    let mut wrote_block = false;

    if !report.fields.is_empty() {
        let width = report
            .fields
            .iter()
            .map(|(label, _)| label.width())
            .max()
            .unwrap_or(0);
        for (label, value) in &report.fields {
            // +1 for the colon, +2 for the gap between label and value.
            let pad = " ".repeat(width - label.width() + 2);
            writeln!(out, "{label}:{pad}{value}")?;
        }
        wrote_block = true;
    }

    if let Some(table) = &report.table {
        if wrote_block {
            writeln!(out)?;
        }
        let rows = report.rows(table);
        if rows.is_empty() {
            writeln!(out, "(no rows)")?;
        } else {
            write_grid(out, table, rows)?;
        }
        wrote_block = true;
    }

    if !report.notes.is_empty() {
        if wrote_block {
            writeln!(out)?;
        }
        for note in &report.notes {
            writeln!(out, "{note}")?;
        }
    }
    Ok(())
}

fn write_grid(out: &mut dyn Write, table: &Table, rows: &[Value]) -> io::Result<()> {
    let header: Vec<String> = table
        .columns
        .iter()
        .map(|c| c.title.to_uppercase())
        .collect();
    let body: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            table
                .columns
                .iter()
                .map(|c| display_cell(row.get(&c.key), c.kind))
                .collect()
        })
        .collect();

    let widths: Vec<usize> = (0..table.columns.len())
        .map(|i| {
            body.iter()
                .map(|row| row[i].width())
                .chain([header[i].width()])
                .max()
                .unwrap_or(0)
        })
        .collect();

    for line in std::iter::once(&header).chain(&body) {
        let mut text = String::new();
        for (i, cell) in line.iter().enumerate() {
            if i > 0 {
                text.push_str("  ");
            }
            let pad = " ".repeat(widths[i] - cell.width());
            if table.columns[i].kind.is_numeric() {
                text.push_str(&pad);
                text.push_str(cell);
            } else {
                text.push_str(cell);
                text.push_str(&pad);
            }
        }
        writeln!(out, "{}", text.trim_end())?;
    }
    Ok(())
}

/// Machine-readable cell for CSV: the raw value, never a formatted one.
fn raw_cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => join_scalars(items, "; "),
        Some(other) => other.to_string(),
    }
}

/// Human-readable cell for table output.
fn display_cell(value: Option<&Value>, kind: Kind) -> String {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return "-".into();
    };
    let Some(number) = value.as_f64().filter(|_| kind.is_numeric()) else {
        return match value {
            Value::String(s) if kind == Kind::Date => date_part(s).to_owned(),
            Value::String(s) => s.clone(),
            Value::Bool(true) => "yes".into(),
            Value::Bool(false) => "no".into(),
            Value::Array(items) if items.is_empty() => "-".into(),
            Value::Array(items) => join_scalars(items, ", "),
            other => other.to_string(),
        };
    };
    match kind {
        Kind::Text | Kind::Date => unreachable!("numeric kinds only"),
        Kind::Int => group_thousands(number.round() as i64),
        Kind::Decimal => format!("{number:.1}"),
        Kind::Percent => format!("{:.2}%", number * 100.0),
        Kind::IntDelta => signed(number, group_thousands(number.round() as i64)),
        Kind::DecimalDelta => signed(number, format!("{number:.1}")),
        Kind::PercentDelta => signed(number, format!("{:.2}%", number * 100.0)),
    }
}

/// The `YYYY-MM-DD` prefix of a timestamp, or the whole text if it has none.
fn date_part(timestamp: &str) -> &str {
    match timestamp.split_once('T') {
        Some((date, _)) if date.len() == 10 => date,
        _ => timestamp,
    }
}

/// Prefix positive numbers with `+`; negatives already carry their sign.
fn signed(number: f64, text: String) -> String {
    if number > 0.0 {
        format!("+{text}")
    } else {
        text
    }
}

fn join_scalars(items: &[Value], separator: &str) -> String {
    items
        .iter()
        .map(|item| match item {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(separator)
}

fn group_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Round to a fixed number of decimal places.
///
/// Google returns CTR and position as long floats (`8.238461538461538`).
/// Rounding keeps JSON output compact without losing meaningful precision.
pub fn round_to(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn render_to_strings(report: &Report, format: Format, pretty: bool) -> (String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        render(
            report,
            format,
            Streams {
                out: &mut out,
                err: &mut err,
                pretty,
            },
        )
        .unwrap();
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn sample() -> Report {
        Report::new(json!({
            "site": "sc-domain:example.com",
            "rows": [
                { "query": "rust cli", "clicks": 1200, "ctr": 0.0353, "position": 8.24 },
                { "query": "日本語", "clicks": 7, "ctr": 0.5, "position": 1.0 },
            ],
        }))
        .field("Site", "sc-domain:example.com")
        .field("Days", "28")
        .table(
            vec![
                Column::text("query", "Query"),
                Column::new("clicks", "Clicks", Kind::Int),
                Column::new("ctr", "CTR", Kind::Percent),
                Column::new("position", "Position", Kind::Decimal),
            ],
            "/rows",
        )
    }

    #[test]
    fn table_aligns_columns_and_formats_numbers() {
        let (out, err) = render_to_strings(&sample(), Format::Table, false);
        let expected = "\
Site:  sc-domain:example.com
Days:  28

QUERY     CLICKS     CTR  POSITION
rust cli   1,200   3.53%       8.2
日本語         7  50.00%       1.0
";
        assert_eq!(out, expected);
        assert!(err.is_empty());
    }

    #[test]
    fn csv_uses_keys_and_raw_values() {
        let (out, _) = render_to_strings(&sample(), Format::Csv, false);
        assert_eq!(
            out,
            "query,clicks,ctr,position\nrust cli,1200,0.0353,8.24\n日本語,7,0.5,1.0\n"
        );
    }

    #[test]
    fn json_is_compact_when_piped_and_pretty_on_a_terminal() {
        let report = Report::new(json!({ "a": 1 }));
        assert_eq!(
            render_to_strings(&report, Format::Json, false).0,
            "{\"a\":1}\n"
        );
        assert_eq!(
            render_to_strings(&report, Format::Json, true).0,
            "{\n  \"a\": 1\n}\n"
        );
    }

    #[test]
    fn json_keeps_fields_in_the_order_they_were_written() {
        // Dimension keys before metrics, metadata before rows: the order is
        // chosen for readers, so it must survive serialisation.
        let report = Report::new(json!({
            "site": "x",
            "rows": [{ "query": "q", "clicks": 1, "impressions": 2 }],
        }));
        assert_eq!(
            render_to_strings(&report, Format::Json, false).0,
            "{\"site\":\"x\",\"rows\":[{\"query\":\"q\",\"clicks\":1,\"impressions\":2}]}\n"
        );
    }

    #[test]
    fn notes_reach_every_format_without_corrupting_stdout() {
        let report = sample().note("Only the top 2 rows are shown.");

        let (table, _) = render_to_strings(&report, Format::Table, false);
        assert!(table.ends_with("\nOnly the top 2 rows are shown.\n"));

        let (json_out, _) = render_to_strings(&report, Format::Json, false);
        let doc: Value = serde_json::from_str(&json_out).unwrap();
        assert_eq!(doc["notes"], json!(["Only the top 2 rows are shown."]));

        let (csv_out, csv_err) = render_to_strings(&report, Format::Csv, false);
        assert!(!csv_out.contains("Only the top"));
        assert_eq!(csv_err, "note: Only the top 2 rows are shown.\n");
    }

    #[test]
    fn empty_table_says_so() {
        let report =
            Report::new(json!({ "rows": [] })).table(vec![Column::text("query", "Query")], "/rows");
        assert_eq!(
            render_to_strings(&report, Format::Table, false).0,
            "(no rows)\n"
        );
        assert_eq!(render_to_strings(&report, Format::Csv, false).0, "query\n");
    }

    #[test]
    fn fields_only_report_falls_back_to_key_value_csv() {
        let report = Report::new(json!({ "status": "ok" })).field("Status", "ok");
        assert_eq!(
            render_to_strings(&report, Format::Csv, false).0,
            "field,value\nStatus,ok\n"
        );
        assert_eq!(
            render_to_strings(&report, Format::Table, false).0,
            "Status:  ok\n"
        );
    }

    #[test]
    fn display_cells_cover_every_kind() {
        let n = json!(1234.56);
        assert_eq!(display_cell(Some(&n), Kind::Int), "1,235");
        assert_eq!(display_cell(Some(&n), Kind::Decimal), "1234.6");
        assert_eq!(display_cell(Some(&json!(0.1234)), Kind::Percent), "12.34%");
        assert_eq!(display_cell(Some(&json!(120)), Kind::IntDelta), "+120");
        assert_eq!(display_cell(Some(&json!(-1500)), Kind::IntDelta), "-1,500");
        assert_eq!(display_cell(Some(&json!(0)), Kind::IntDelta), "0");
        assert_eq!(
            display_cell(Some(&json!(-1.44)), Kind::DecimalDelta),
            "-1.4"
        );
        assert_eq!(
            display_cell(Some(&json!(0.125)), Kind::PercentDelta),
            "+12.50%"
        );
        assert_eq!(display_cell(None, Kind::Int), "-");
        assert_eq!(display_cell(Some(&Value::Null), Kind::Text), "-");
        assert_eq!(display_cell(Some(&json!(true)), Kind::Text), "yes");
        assert_eq!(display_cell(Some(&json!(["a", "b"])), Kind::Text), "a, b");
        assert_eq!(display_cell(Some(&json!([])), Kind::Text), "-");
        assert_eq!(
            display_cell(Some(&json!("2026-11-10T03:12:45.000Z")), Kind::Date),
            "2026-11-10"
        );
        assert_eq!(
            display_cell(Some(&json!("not a timestamp")), Kind::Date),
            "not a timestamp"
        );
        assert_eq!(display_cell(Some(&Value::Null), Kind::Date), "-");
    }

    #[test]
    fn raw_cells_keep_machine_values() {
        assert_eq!(raw_cell(None), "");
        assert_eq!(raw_cell(Some(&Value::Null)), "");
        assert_eq!(raw_cell(Some(&json!(false))), "false");
        assert_eq!(raw_cell(Some(&json!(["a", "b"]))), "a; b");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1234567), "1,234,567");
        assert_eq!(group_thousands(-1234), "-1,234");
    }

    #[test]
    fn rounds_to_places() {
        assert_eq!(round_to(8.238461538461538, 2), 8.24);
        assert_eq!(round_to(0.035294117, 4), 0.0353);
        assert_eq!(round_to(12.0, 2), 12.0);
    }
}
