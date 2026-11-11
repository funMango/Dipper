//! JSON shape for extracted ontology documents and human overlays.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::DipperError;

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldStatus {
    Confirmed,
    Estimated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OntologyColumn {
    pub name: String,
    pub data_type: String,
    pub data_length: Option<i64>,
    pub data_precision: Option<i64>,
    pub data_scale: Option<i64>,
    pub nullable: bool,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
    pub db_comment: Option<String>,
    pub meaning: Option<String>,
    pub meaning_status: FieldStatus,
    pub unit: Option<String>,
    pub unit_status: FieldStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OntologyDoc {
    pub version: u32,
    pub source: String,
    pub owner: String,
    pub table: String,
    pub extracted_at: String,
    pub object_type: String,
    pub table_comment: Option<String>,
    pub primary_key: Vec<String>,
    pub time_column: Option<String>,
    pub columns: Vec<OntologyColumn>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayDoc {
    pub version: u32,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub extracted_at: Option<String>,
    #[serde(default)]
    pub object_type: Option<String>,
    #[serde(default)]
    pub table_comment: Option<String>,
    #[serde(default)]
    pub primary_key: Option<Vec<String>>,
    #[serde(default)]
    pub time_column: Option<String>,
    #[serde(default)]
    pub columns: Vec<OverlayColumn>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayColumn {
    pub name: String,
    #[serde(default)]
    pub data_type: Option<String>,
    #[serde(default)]
    pub data_length: Option<i64>,
    #[serde(default)]
    pub data_precision: Option<i64>,
    #[serde(default)]
    pub data_scale: Option<i64>,
    #[serde(default)]
    pub nullable: Option<bool>,
    #[serde(default, rename = "virtual")]
    pub is_virtual: Option<bool>,
    #[serde(default)]
    pub db_comment: Option<String>,
    #[serde(default)]
    pub meaning: Option<String>,
    #[serde(default)]
    pub meaning_status: Option<FieldStatus>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub unit_status: Option<FieldStatus>,
}

impl OntologyDoc {
    pub fn check(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!("version must be {VERSION}"));
        }
        if self.object_type != "TABLE" && self.object_type != "VIEW" {
            return Err("object_type must be TABLE or VIEW".to_string());
        }
        let names: Vec<&str> = self
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        if names.iter().any(|name| name.is_empty()) {
            return Err("column name is empty".to_string());
        }
        for key in &self.primary_key {
            if !names.contains(&key.as_str()) {
                return Err(format!("primary key column {key} is not in columns"));
            }
        }
        if let Some(time) = &self.time_column {
            if !names.contains(&time.as_str()) {
                return Err(format!("time_column {time} is not in columns"));
            }
        }
        for column in &self.columns {
            if column.meaning.is_some() && column.meaning_status == FieldStatus::Unknown {
                return Err(format!(
                    "column {} has a meaning but meaning_status is unknown",
                    column.name
                ));
            }
            if column.unit.is_some() && column.unit_status == FieldStatus::Unknown {
                return Err(format!(
                    "column {} has a unit but unit_status is unknown",
                    column.name
                ));
            }
        }
        Ok(())
    }
}

pub fn parse_doc(text: &str) -> Result<OntologyDoc, String> {
    let doc: OntologyDoc = serde_json::from_str(text).map_err(|err| err.to_string())?;
    doc.check()?;
    Ok(doc)
}

pub fn parse_overlay(text: &str) -> Result<OverlayDoc, String> {
    let doc: OverlayDoc = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if doc.version != VERSION {
        return Err(format!("version must be {VERSION}"));
    }
    if doc
        .object_type
        .as_deref()
        .is_some_and(|kind| kind != "TABLE" && kind != "VIEW")
    {
        return Err("object_type must be TABLE or VIEW".to_string());
    }
    for column in &doc.columns {
        if column.name.is_empty() {
            return Err("column name is empty".to_string());
        }
        if column.meaning.is_some() && column.meaning_status.is_none() {
            return Err(format!(
                "column {} sets meaning without meaning_status",
                column.name
            ));
        }
        if column.unit.is_some() && column.unit_status.is_none() {
            return Err(format!(
                "column {} sets unit without unit_status",
                column.name
            ));
        }
    }
    Ok(doc)
}

pub fn validate(root: &Path) -> Result<(), DipperError> {
    let extracted = root.join("ontology/extracted");
    let overlay = root.join("ontology/overlay");
    let mut errors = Vec::new();
    if extracted.is_dir() {
        for path in json_files(&extracted) {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Err(err) = parse_doc(&text) {
                        errors.push(format!("{}: {err}", path.display()));
                    }
                }
                Err(err) => errors.push(format!("{}: {err}", path.display())),
            }
        }
    }
    if overlay.is_dir() {
        for path in json_files(&overlay) {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    errors.push(format!("{}: {err}", path.display()));
                    continue;
                }
            };
            match parse_overlay(&text) {
                Ok(overlay_doc) => {
                    let extracted_path = overlay_to_extracted(root, &path);
                    if extracted_path.is_file() {
                        match std::fs::read_to_string(&extracted_path).and_then(|text| {
                            parse_doc(&text).map_err(|err| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, err)
                            })
                        }) {
                            Ok(doc) => {
                                if let Err(err) = crate::ontology::merge::merge(&doc, &overlay_doc)
                                {
                                    errors.push(format!("{}: {err}", path.display()));
                                }
                            }
                            Err(err) => errors.push(format!("{}: {err}", extracted_path.display())),
                        }
                    }
                }
                Err(err) => errors.push(format!("{}: {err}", path.display())),
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ontology_error(errors.join("\n")))
    }
}

fn overlay_to_extracted(root: &Path, overlay: &Path) -> std::path::PathBuf {
    let relative = overlay
        .strip_prefix(root.join("ontology/overlay"))
        .unwrap_or(overlay);
    root.join("ontology/extracted").join(relative)
}

fn json_files(dir: &Path) -> Vec<std::path::PathBuf> {
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

pub fn ontology_error(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        source_name: "ontology".to_string(),
        message: message.into(),
    }
}
