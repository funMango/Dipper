//! Markdown rendered from merged ontology JSON.

use std::path::{Path, PathBuf};

use crate::error::DipperError;
use crate::ontology::merge::merge;
use crate::ontology::schema::{ontology_error, parse_doc, parse_overlay, OntologyDoc};

pub fn render(root: &Path) -> Result<Vec<PathBuf>, DipperError> {
    let extracted = root.join("ontology/extracted");
    if !extracted.is_dir() {
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    let mut hubs: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for path in json_files(&extracted) {
        let text = std::fs::read_to_string(&path).map_err(|err| ontology_error(err.to_string()))?;
        let doc = parse_doc(&text).map_err(ontology_error)?;
        let overlay_path = extracted_to_overlay(root, &path);
        let merged = if overlay_path.is_file() {
            let overlay_text = std::fs::read_to_string(&overlay_path)
                .map_err(|err| ontology_error(err.to_string()))?;
            let overlay = parse_overlay(&overlay_text).map_err(ontology_error)?;
            merge(&doc, &overlay).map_err(ontology_error)?
        } else {
            doc
        };
        let md_path = extracted_to_md(root, &path);
        if let Some(parent) = md_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| ontology_error(err.to_string()))?;
        }
        std::fs::write(&md_path, markdown(&merged))
            .map_err(|err| ontology_error(err.to_string()))?;
        hubs.entry(merged.source.clone())
            .or_default()
            .push(format!("[[{}.{}]]", merged.owner, merged.table));
        written.push(md_path);
    }
    for (source, mut links) in hubs {
        links.sort();
        links.dedup();
        let index = root.join("ontology/md").join(&source).join("_index.md");
        if let Some(parent) = index.parent() {
            std::fs::create_dir_all(parent).map_err(|err| ontology_error(err.to_string()))?;
        }
        let mut body = format!("# {source}\n\n");
        for link in &links {
            body.push_str(&format!("- {link}\n"));
        }
        std::fs::write(&index, body).map_err(|err| ontology_error(err.to_string()))?;
        written.push(index);
    }
    written.sort();
    Ok(written)
}

fn markdown(doc: &OntologyDoc) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}.{}\n\n", doc.owner, doc.table));
    out.push_str(&format!("- source: {}\n", doc.source));
    out.push_str(&format!("- object_type: {}\n", doc.object_type));
    out.push_str(&format!(
        "- table_comment: {}\n",
        doc.table_comment.as_deref().unwrap_or("")
    ));
    out.push_str(&format!("- primary_key: {}\n", doc.primary_key.join(", ")));
    out.push_str(&format!(
        "- time_column: {}\n\n",
        doc.time_column.as_deref().unwrap_or("")
    ));
    out.push_str("| name | data_type | nullable | virtual | meaning | meaning_status | unit | unit_status |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for column in &doc.columns {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&column.name),
            cell(&column.data_type),
            column.nullable,
            column.is_virtual,
            cell(column.meaning.as_deref().unwrap_or("")),
            status(column.meaning_status),
            cell(column.unit.as_deref().unwrap_or("")),
            status(column.unit_status),
        ));
    }
    out.push('\n');
    out.push_str("## Foreign Keys\n");
    if doc.foreign_keys.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for key in &doc.foreign_keys {
            let mut line = format!(
                "- `{}` -> [[{}.{}]] (`{}`)",
                key.columns.join(", "),
                key.target_owner,
                key.target_table,
                key.target_columns.join(", ")
            );
            if let Some(name) = &key.name {
                line.push_str(&format!(" ({name})"));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str("## Relations\n");
    if doc.relations.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for relation in &doc.relations {
            let label = match relation.status {
                crate::ontology::schema::RelationStatus::Draft => "draft",
                crate::ontology::schema::RelationStatus::Confirmed => "confirmed",
            };
            let mut line = format!(
                "- [{label}] `{}` -> [[{}.{}]] (`{}`)",
                relation.columns.join(", "),
                relation.target_owner,
                relation.target_table,
                relation.target_columns.join(", ")
            );
            if let Some(evidence) = relation.evidence.as_deref().filter(|text| !text.is_empty()) {
                line.push_str(" — ");
                line.push_str(evidence);
            }
            if let Some(overlap) = &relation.overlap {
                let percent = overlap.match_ratio * 100.0;
                line.push_str(&format!(
                    " (overlap: {}/{}, {percent:.1}%)",
                    overlap.matched_rows, overlap.source_non_null_rows
                ));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn status(status: crate::ontology::schema::FieldStatus) -> &'static str {
    match status {
        crate::ontology::schema::FieldStatus::Confirmed => "confirmed",
        crate::ontology::schema::FieldStatus::Draft => "draft",
        crate::ontology::schema::FieldStatus::Estimated => "estimated",
        crate::ontology::schema::FieldStatus::Unknown => "unknown",
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn extracted_to_overlay(root: &Path, extracted: &Path) -> PathBuf {
    let relative = extracted
        .strip_prefix(root.join("ontology/extracted"))
        .unwrap_or(extracted);
    root.join("ontology/overlay").join(relative)
}

fn extracted_to_md(root: &Path, extracted: &Path) -> PathBuf {
    let relative = extracted
        .strip_prefix(root.join("ontology/extracted"))
        .unwrap_or(extracted);
    let mut path = root.join("ontology/md").join(relative);
    path.set_extension("md");
    path
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
