use crate::config::{Filter, FilterOp};
use crate::error::DipperError;
use crate::source::Value;
use crate::sql::guard::SqlDialect;
use crate::sql::validate_ident;

use super::dialect::{placeholder, quote};

/// Build a parenthesized WHERE predicate. Values are returned as binds, never inlined.
pub fn where_clause(
    dialect: SqlDialect,
    filters: &[Filter],
    first_bind: usize,
) -> Result<(String, Vec<Value>), DipperError> {
    if filters.is_empty() {
        return Err(filter_error("at least one filter is required"));
    }
    let mut binds = Vec::new();
    let mut index = first_bind;
    let mut parts = Vec::with_capacity(filters.len());
    for filter in filters {
        validate_ident(&filter.column).map_err(filter_error)?;
        let column = quote(dialect, &filter.column);
        parts.push(render_filter(
            dialect,
            &column,
            filter.op,
            filter.value.as_ref(),
            &mut index,
            &mut binds,
        )?);
    }
    Ok((format!("({})", parts.join(" AND ")), binds))
}

/// Convert one TOML filter value into a bound [`Value`].
pub fn bind_value(value: &toml::Value) -> Result<Value, DipperError> {
    match value {
        toml::Value::String(text) => Ok(Value::Text(text.clone())),
        toml::Value::Integer(number) => Ok(Value::Int(*number)),
        toml::Value::Float(number) => Ok(Value::Float(*number)),
        toml::Value::Boolean(flag) => Ok(Value::Int(if *flag { 1 } else { 0 })),
        toml::Value::Datetime(datetime) => bind_datetime(datetime),
        toml::Value::Array(_) | toml::Value::Table(_) => {
            Err(filter_error("filter value must be a scalar"))
        }
    }
}

fn render_filter(
    dialect: SqlDialect,
    column: &str,
    op: FilterOp,
    value: Option<&toml::Value>,
    index: &mut usize,
    binds: &mut Vec<Value>,
) -> Result<String, DipperError> {
    Ok(match op {
        FilterOp::Eq => format!("{column} = {}", one_value(dialect, value, index, binds)?),
        FilterOp::Ne => format!("{column} <> {}", one_value(dialect, value, index, binds)?),
        FilterOp::Gt => format!("{column} > {}", one_value(dialect, value, index, binds)?),
        FilterOp::Gte => format!("{column} >= {}", one_value(dialect, value, index, binds)?),
        FilterOp::Lt => format!("{column} < {}", one_value(dialect, value, index, binds)?),
        FilterOp::Lte => format!("{column} <= {}", one_value(dialect, value, index, binds)?),
        FilterOp::Like => format!("{column} LIKE {}", one_value(dialect, value, index, binds)?),
        FilterOp::Between => {
            let (low, high) = two_values(dialect, value, index, binds)?;
            format!("{column} BETWEEN {low} AND {high}")
        }
        FilterOp::In => {
            let list = in_values(dialect, value, index, binds)?;
            format!("{column} IN ({list})")
        }
        FilterOp::IsNull => format!("{column} IS NULL"),
        FilterOp::NotNull => format!("{column} IS NOT NULL"),
    })
}

fn one_value(
    dialect: SqlDialect,
    value: Option<&toml::Value>,
    index: &mut usize,
    binds: &mut Vec<Value>,
) -> Result<String, DipperError> {
    let value = value.ok_or_else(|| filter_error("a single value is required"))?;
    push_bind(dialect, value, index, binds)
}

fn two_values(
    dialect: SqlDialect,
    value: Option<&toml::Value>,
    index: &mut usize,
    binds: &mut Vec<Value>,
) -> Result<(String, String), DipperError> {
    let items = array_values(value, 2, "between requires an array of two values")?;
    let low = push_bind(dialect, &items[0], index, binds)?;
    let high = push_bind(dialect, &items[1], index, binds)?;
    Ok((low, high))
}

fn in_values(
    dialect: SqlDialect,
    value: Option<&toml::Value>,
    index: &mut usize,
    binds: &mut Vec<Value>,
) -> Result<String, DipperError> {
    let items = match value {
        Some(toml::Value::Array(items)) if !items.is_empty() => items,
        _ => return Err(filter_error("in requires a non-empty array")),
    };
    let mut placeholders = Vec::with_capacity(items.len());
    for item in items {
        placeholders.push(push_bind(dialect, item, index, binds)?);
    }
    Ok(placeholders.join(", "))
}

fn array_values<'a>(
    value: Option<&'a toml::Value>,
    len: usize,
    message: &str,
) -> Result<&'a [toml::Value], DipperError> {
    match value {
        Some(toml::Value::Array(items)) if items.len() == len => Ok(items),
        _ => Err(filter_error(message)),
    }
}

fn push_bind(
    dialect: SqlDialect,
    value: &toml::Value,
    index: &mut usize,
    binds: &mut Vec<Value>,
) -> Result<String, DipperError> {
    binds.push(bind_value(value)?);
    let text = placeholder(dialect, *index);
    *index += 1;
    Ok(text)
}

fn bind_datetime(datetime: &toml::value::Datetime) -> Result<Value, DipperError> {
    let Some(date) = datetime.date else {
        return Err(filter_error("filter datetime requires a date"));
    };
    let naive = chrono::NaiveDate::from_ymd_opt(
        i32::from(date.year),
        u32::from(date.month),
        u32::from(date.day),
    )
    .ok_or_else(|| filter_error("invalid filter date"))?;
    let Some(time) = datetime.time else {
        return Ok(Value::Date(naive));
    };
    let timestamp = naive
        .and_hms_nano_opt(
            u32::from(time.hour),
            u32::from(time.minute),
            u32::from(time.second.unwrap_or(0)),
            time.nanosecond.unwrap_or(0),
        )
        .ok_or_else(|| filter_error("invalid filter time"))?;
    Ok(Value::Timestamp(timestamp))
}

fn filter_error(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        source_name: "filter".to_string(),
        message: message.into(),
    }
}
