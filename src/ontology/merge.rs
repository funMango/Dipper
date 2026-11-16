//! Apply an overlay onto an extracted document without writing the overlay.

use crate::ontology::schema::{OntologyColumn, OntologyDoc, OverlayColumn, OverlayDoc};

pub fn merge(extracted: &OntologyDoc, overlay: &OverlayDoc) -> Result<OntologyDoc, String> {
    if let Some(source) = &overlay.source {
        if source != &extracted.source {
            return Err(format!(
                "overlay source {source} does not match extracted {}",
                extracted.source
            ));
        }
    }
    if let Some(owner) = &overlay.owner {
        if owner != &extracted.owner {
            return Err(format!(
                "overlay owner {owner} does not match extracted {}",
                extracted.owner
            ));
        }
    }
    if let Some(table) = &overlay.table {
        if table != &extracted.table {
            return Err(format!(
                "overlay table {table} does not match extracted {}",
                extracted.table
            ));
        }
    }
    if let Some(kind) = &overlay.object_type {
        if kind != &extracted.object_type {
            return Err("overlay object_type does not match extracted".to_string());
        }
    }
    let _ = &overlay.extracted_at;
    if let Some(comment) = &overlay.table_comment {
        if extracted.table_comment.as_ref() != Some(comment) {
            return Err("overlay table_comment does not match extracted".to_string());
        }
    }
    if let Some(key) = &overlay.primary_key {
        if key != &extracted.primary_key {
            return Err("overlay primary_key does not match extracted".to_string());
        }
    }
    if let Some(time) = &overlay.time_column {
        if extracted.time_column.as_ref() != Some(time) {
            return Err("overlay time_column does not match extracted".to_string());
        }
    }
    let mut doc = extracted.clone();
    for extra in &overlay.columns {
        let Some(column) = doc
            .columns
            .iter_mut()
            .find(|column| column.name.eq_ignore_ascii_case(&extra.name))
        else {
            return Err(format!(
                "overlay column {} is not in the extracted table",
                extra.name
            ));
        };
        same_structure(column, extra)?;
        if let Some(meaning) = &extra.meaning {
            column.meaning = Some(meaning.clone());
        }
        if let Some(status) = extra.meaning_status {
            column.meaning_status = status;
        }
        if let Some(unit) = &extra.unit {
            column.unit = Some(unit.clone());
        }
        if let Some(status) = extra.unit_status {
            column.unit_status = status;
        }
    }
    doc.relations = normalize_relations(&doc, &overlay.relations)?;
    doc.check()?;
    Ok(doc)
}

fn normalize_relations(
    doc: &OntologyDoc,
    relations: &[crate::ontology::schema::Relation],
) -> Result<Vec<crate::ontology::schema::Relation>, String> {
    use crate::ontology::schema::Relation;
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for relation in relations {
        let columns = relation
            .columns
            .iter()
            .map(|name| {
                doc.columns
                    .iter()
                    .find(|column| column.name.eq_ignore_ascii_case(name))
                    .map(|column| column.name.clone())
                    .ok_or_else(|| format!("relation column {name} is not in the extracted table"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let key = format!(
            "{}|{}|{}|{}",
            columns.join(",").to_ascii_lowercase(),
            relation.target_owner.to_ascii_lowercase(),
            relation.target_table.to_ascii_lowercase(),
            relation.target_columns.join(",").to_ascii_lowercase()
        );
        if seen.iter().any(|existing| existing == &key) {
            return Err("duplicate relation".to_string());
        }
        seen.push(key);
        out.push(Relation {
            columns,
            ..relation.clone()
        });
    }
    Ok(out)
}

fn same_structure(column: &OntologyColumn, extra: &OverlayColumn) -> Result<(), String> {
    if extra
        .data_type
        .as_ref()
        .is_some_and(|value| value != &column.data_type)
    {
        return Err(format!(
            "overlay must not change data_type of {}",
            column.name
        ));
    }
    if extra
        .data_length
        .is_some_and(|value| Some(value) != column.data_length)
    {
        return Err(format!(
            "overlay must not change data_length of {}",
            column.name
        ));
    }
    if extra
        .data_precision
        .is_some_and(|value| Some(value) != column.data_precision)
    {
        return Err(format!(
            "overlay must not change data_precision of {}",
            column.name
        ));
    }
    if extra
        .data_scale
        .is_some_and(|value| Some(value) != column.data_scale)
    {
        return Err(format!(
            "overlay must not change data_scale of {}",
            column.name
        ));
    }
    if extra
        .db_comment
        .as_ref()
        .is_some_and(|value| Some(value) != column.db_comment.as_ref())
    {
        return Err(format!(
            "overlay must not change db_comment of {}",
            column.name
        ));
    }
    if extra.nullable.is_some_and(|value| value != column.nullable) {
        return Err(format!(
            "overlay must not change nullable of {}",
            column.name
        ));
    }
    if extra
        .is_virtual
        .is_some_and(|value| value != column.is_virtual)
    {
        return Err(format!(
            "overlay must not change virtual of {}",
            column.name
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::schema::{FieldStatus, OntologyColumn, OntologyDoc, VERSION};

    #[test]
    fn overlay_column_match_ignores_ascii_case() {
        let extracted = OntologyDoc {
            version: VERSION,
            source: "sample".to_string(),
            owner: "main".to_string(),
            table: "EVENTS".to_string(),
            extracted_at: "2026-11-11T00:00:00Z".to_string(),
            object_type: "TABLE".to_string(),
            table_comment: None,
            primary_key: vec!["QTY".to_string()],
            time_column: None,
            foreign_keys: Vec::new(),
            relations: Vec::new(),
            columns: vec![OntologyColumn {
                name: "QTY".to_string(),
                data_type: "INTEGER".to_string(),
                data_length: None,
                data_precision: None,
                data_scale: None,
                nullable: true,
                is_virtual: false,
                db_comment: None,
                meaning: None,
                meaning_status: FieldStatus::Unknown,
                unit: None,
                unit_status: FieldStatus::Unknown,
            }],
        };
        let overlay = OverlayDoc {
            version: VERSION,
            source: None,
            owner: None,
            table: None,
            extracted_at: None,
            object_type: None,
            table_comment: None,
            primary_key: None,
            time_column: None,
            relations: Vec::new(),
            columns: vec![OverlayColumn {
                name: "qty".to_string(),
                data_type: None,
                data_length: None,
                data_precision: None,
                data_scale: None,
                nullable: None,
                is_virtual: None,
                db_comment: None,
                meaning: Some("how many".to_string()),
                meaning_status: Some(FieldStatus::Confirmed),
                unit: None,
                unit_status: None,
            }],
        };
        let merged = merge(&extracted, &overlay).unwrap();
        assert_eq!(merged.columns[0].name, "QTY");
        assert_eq!(merged.columns[0].meaning.as_deref(), Some("how many"));
        assert_eq!(merged.columns[0].meaning_status, FieldStatus::Confirmed);
    }
}
