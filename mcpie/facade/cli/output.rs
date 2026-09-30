//! Output rendering: JSON, YAML and simple aligned tables.

use clap::ValueEnum;
use serde_json::Value;

use super::CliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Json,
    Yaml,
    /// Only for `sources`, `ops` and `search`.
    Table,
}

/// Render an arbitrary value. Tables are refused because there is no row shape to assume.
pub fn render_value(value: &Value, format: Format, pretty: bool) -> Result<String, CliError> {
    match format {
        Format::Json if pretty => {
            serde_json::to_string_pretty(value).map_err(|e| CliError::Failure(e.to_string()))
        }
        Format::Json => serde_json::to_string(value).map_err(|e| CliError::Failure(e.to_string())),
        Format::Yaml => {
            serde_yaml_ng::to_string(value).map_err(|e| CliError::Failure(e.to_string()))
        }
        Format::Table => Err(CliError::Usage(
            "--format table is only available for sources, ops and search".into(),
        )),
    }
}

/// Rows that know how to print themselves as a table.
pub trait Tabular {
    fn headers() -> Vec<&'static str>;
    fn row(&self) -> Vec<String>;
}

const MAX_CELL: usize = 80;

pub fn render_table<T: Tabular>(rows: &[T]) -> String {
    let headers = T::headers();
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.row().into_iter().map(clip).collect())
        .collect();
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            cells
                .iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .chain(std::iter::once(headers[i].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    push_row(
        &mut out,
        &headers.iter().map(|h| h.to_uppercase()).collect::<Vec<_>>(),
        &widths,
    );
    for row in &cells {
        push_row(&mut out, row, &widths);
    }
    out
}

fn push_row(out: &mut String, cells: &[String], widths: &[usize]) {
    let last = widths.len().saturating_sub(1);
    for (i, width) in widths.iter().enumerate() {
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        if i == last {
            out.push_str(cell);
        } else {
            let pad = width.saturating_sub(cell.chars().count());
            out.push_str(cell);
            out.extend(std::iter::repeat_n(' ', pad + 2));
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('\n');
}

fn clip(cell: String) -> String {
    let single: String = cell.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= MAX_CELL {
        return single;
    }
    let mut clipped: String = single.chars().take(MAX_CELL - 1).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row(&'static str, &'static str);

    impl Tabular for Row {
        fn headers() -> Vec<&'static str> {
            vec!["id", "note"]
        }

        fn row(&self) -> Vec<String> {
            vec![self.0.into(), self.1.into()]
        }
    }

    #[test]
    fn aligns_columns_and_clips_cells() {
        let table = render_table(&[Row("a", "short"), Row("longer-id", "multi\nline  text")]);
        assert_eq!(
            table,
            "ID         NOTE\na          short\nlonger-id  multi line text\n"
        );
        let long = "x".repeat(200);
        let clipped = clip(long);
        assert_eq!(clipped.chars().count(), MAX_CELL);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn refuses_tables_for_values() {
        let error = render_value(&Value::Null, Format::Table, false).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)));
        assert_eq!(
            render_value(&serde_json::json!({"a": 1}), Format::Json, false).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(
            render_value(&serde_json::json!({"a": 1}), Format::Yaml, false).unwrap(),
            "a: 1\n"
        );
    }
}
