//! Build an extracted document from dictionary metadata. Meanings stay unknown.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{self, SourceConfig};
use crate::error::DipperError;
use crate::ontology::schema::{ontology_error, FieldStatus, OntologyColumn, OntologyDoc, VERSION};
use crate::source::{open, CatalogColumn, CatalogTable, OpenOptions, SqlLogSink};

pub fn pull(
    config_path: &Path,
    source_name: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
) -> Result<Vec<PathBuf>, DipperError> {
    let config = config::load(config_path)?;
    let source_cfg = config
        .sources
        .get(source_name)
        .ok_or_else(|| ontology_error(format!("unknown source: {source_name}")))?;
    let targets = tables_for(&config, source_name, source_cfg, table)?;
    let opened = open(
        source_name,
        source_cfg,
        &OpenOptions {
            strict,
            log: if log_sql {
                SqlLogSink::Stderr
            } else {
                SqlLogSink::Off
            },
            action: format!("ontology:{source_name}"),
        },
    )?;
    let mut written = Vec::with_capacity(targets.len());
    for (owner, table) in targets {
        let catalog = opened.source.catalog(&owner, &table)?;
        let doc = document(source_name, &owner, &table, &catalog);
        let path = extracted_path(source_name, &owner, &table)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| ontology_error(err.to_string()))?;
        }
        let text =
            serde_json::to_string_pretty(&doc).map_err(|err| ontology_error(err.to_string()))?;
        std::fs::write(&path, format!("{text}\n"))
            .map_err(|err| ontology_error(err.to_string()))?;
        written.push(path);
    }
    Ok(written)
}

fn tables_for(
    config: &crate::config::Config,
    source_name: &str,
    source_cfg: &SourceConfig,
    table: Option<&str>,
) -> Result<Vec<(String, String)>, DipperError> {
    if let Some(table) = table {
        return Ok(vec![split_table(table, source_cfg)?]);
    }
    let mut targets = Vec::new();
    for dip in config.dips.iter().filter(|dip| dip.source == source_name) {
        let parsed = split_table(&dip.table, source_cfg)?;
        if !targets.contains(&parsed) {
            targets.push(parsed);
        }
    }
    if targets.is_empty() {
        return Err(ontology_error(
            "pass --table OWNER.TABLE or configure a dip for this source",
        ));
    }
    Ok(targets)
}

fn split_table(table: &str, source_cfg: &SourceConfig) -> Result<(String, String), DipperError> {
    let mut parts = table.split('.');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    if parts.next().is_some() || first.is_empty() {
        return Err(ontology_error("--table must be OWNER.TABLE with one dot"));
    }
    match second {
        Some(name) if !name.is_empty() => Ok((first.to_string(), name.to_string())),
        Some(_) => Err(ontology_error("--table must be OWNER.TABLE with one dot")),
        None => match source_cfg {
            SourceConfig::Sqlite { .. } => Ok(("main".to_string(), first.to_string())),
            SourceConfig::Oracle { .. } => {
                Err(ontology_error("--table must be OWNER.TABLE with one dot"))
            }
        },
    }
}

pub(crate) fn document(
    source: &str,
    owner: &str,
    table: &str,
    catalog: &CatalogTable,
) -> OntologyDoc {
    OntologyDoc {
        version: VERSION,
        source: source.to_string(),
        owner: owner.to_string(),
        table: table.to_string(),
        extracted_at: extracted_at_now(),
        object_type: catalog.object_type.clone(),
        table_comment: catalog.table_comment.clone(),
        primary_key: catalog.primary_key.clone(),
        time_column: time_column(catalog),
        columns: catalog.columns.iter().map(column_doc).collect(),
    }
}

fn column_doc(column: &CatalogColumn) -> OntologyColumn {
    OntologyColumn {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        data_length: column.data_length,
        data_precision: column.data_precision,
        data_scale: column.data_scale,
        nullable: column.nullable,
        is_virtual: column.virtual_column,
        db_comment: column.db_comment.clone(),
        meaning: None,
        meaning_status: FieldStatus::Unknown,
        unit: None,
        unit_status: FieldStatus::Unknown,
    }
}

/// DATE/TIMESTAMP column that is in the primary key or whose name contains DT, DATE, or TIME.
/// Several candidates stay unset instead of picking one.
pub(crate) fn time_column(catalog: &CatalogTable) -> Option<String> {
    let mut found = Vec::new();
    for column in &catalog.columns {
        if !is_time_type(&column.data_type) {
            continue;
        }
        let in_key = catalog
            .primary_key
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&column.name));
        if in_key || name_hints_time(&column.name) {
            found.push(column.name.clone());
        }
    }
    if found.len() == 1 {
        found.pop()
    } else {
        None
    }
}

fn is_time_type(data_type: &str) -> bool {
    let data_type = data_type.to_ascii_uppercase();
    data_type.contains("DATE") || data_type.contains("TIMESTAMP") || data_type == "TIME"
}

fn name_hints_time(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.contains("DATE") || name.contains("TIME") || name.contains("DT")
}

fn extracted_path(source: &str, owner: &str, table: &str) -> Result<PathBuf, DipperError> {
    for part in [source, owner, table] {
        if part.is_empty() || part.contains('/') || part.contains('\\') || part.contains("..") {
            return Err(ontology_error(format!(
                "invalid ontology path part: {part}"
            )));
        }
    }
    Ok(PathBuf::from("ontology")
        .join("extracted")
        .join(source)
        .join(format!("{owner}.{table}.json")))
}

fn extracted_at_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::CatalogColumn;

    fn column(name: &str, data_type: &str) -> CatalogColumn {
        CatalogColumn {
            name: name.to_string(),
            data_type: data_type.to_string(),
            data_length: None,
            data_precision: None,
            data_scale: None,
            nullable: true,
            virtual_column: false,
            db_comment: None,
        }
    }

    #[test]
    fn extractor_leaves_unknown_codes_unknown() {
        let catalog = CatalogTable {
            object_type: "TABLE".to_string(),
            table_comment: None,
            primary_key: vec!["ID".to_string()],
            columns: vec![
                column("ID", "NUMBER"),
                column("JYWK_CNT", "NUMBER"),
                column("VS", "NUMBER"),
                column("XC", "NUMBER"),
            ],
        };
        let doc = document("sample", "MAIN", "EVENTS", &catalog);
        assert!(doc.time_column.is_none());
        for column in &doc.columns {
            assert!(column.meaning.is_none(), "{}", column.name);
            assert!(column.unit.is_none(), "{}", column.name);
            assert_eq!(column.meaning_status, FieldStatus::Unknown);
            assert_eq!(column.unit_status, FieldStatus::Unknown);
        }
    }

    #[test]
    fn one_date_key_becomes_the_time_column() {
        let catalog = CatalogTable {
            object_type: "TABLE".to_string(),
            table_comment: None,
            primary_key: vec!["EVENT_DT".to_string(), "ID".to_string()],
            columns: vec![
                column("ID", "INTEGER"),
                column("EVENT_DT", "DATE"),
                column("QTY", "INTEGER"),
            ],
        };
        assert_eq!(time_column(&catalog).as_deref(), Some("EVENT_DT"));
    }
}
