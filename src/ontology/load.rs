//! Load extracted ontology JSON and merge any overlay. No database access.

use std::path::{Path, PathBuf};

use crate::error::DipperError;
use crate::ontology::merge::merge;
use crate::ontology::schema::{ontology_error, parse_doc, parse_overlay, OntologyDoc};

pub struct LoadedDoc {
    pub doc: OntologyDoc,
    #[allow(dead_code)]
    pub extracted_path: PathBuf,
}

pub fn load_source(root: &Path, source: &str) -> Result<Vec<LoadedDoc>, DipperError> {
    let dir = root.join("ontology/extracted").join(source);
    if !dir.is_dir() {
        return Err(not_found(format!("ontology/extracted/{source} not found")));
    }
    let files = json_files(&dir);
    if files.is_empty() {
        return Err(not_found(format!(
            "ontology/extracted/{source} has no JSON files"
        )));
    }
    let mut loaded = Vec::new();
    for path in files {
        let doc = read_merged(root, &path)?;
        loaded.push(LoadedDoc {
            doc,
            extracted_path: path,
        });
    }
    Ok(loaded)
}

pub(crate) fn read_merged(root: &Path, extracted: &Path) -> Result<OntologyDoc, DipperError> {
    let text = std::fs::read_to_string(extracted)
        .map_err(|err| ontology_error(format!("{}: {err}", display_path(extracted))))?;
    let doc = parse_doc(&text)
        .map_err(|err| ontology_error(format!("{}: {err}", display_path(extracted))))?;
    let overlay_path = extracted_to_overlay(root, extracted);
    if !overlay_path.is_file() {
        return Ok(doc);
    }
    let overlay_text = std::fs::read_to_string(&overlay_path)
        .map_err(|err| ontology_error(format!("{}: {err}", display_path(&overlay_path))))?;
    let overlay = parse_overlay(&overlay_text)
        .map_err(|err| ontology_error(format!("{}: {err}", display_path(&overlay_path))))?;
    merge(&doc, &overlay)
        .map_err(|err| ontology_error(format!("{}: {err}", display_path(&overlay_path))))
}

pub(crate) fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for path in children.into_iter().rev() {
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

pub(crate) fn extracted_to_overlay(root: &Path, extracted: &Path) -> PathBuf {
    let relative = extracted
        .strip_prefix(root.join("ontology/extracted"))
        .unwrap_or(extracted);
    root.join("ontology/overlay").join(relative)
}

pub(crate) fn overlay_to_extracted(root: &Path, overlay: &Path) -> PathBuf {
    let relative = overlay
        .strip_prefix(root.join("ontology/overlay"))
        .unwrap_or(overlay);
    root.join("ontology/extracted").join(relative)
}

fn not_found(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::ObjectNotFound,
        source_name: "ontology".to_string(),
        message: message.into(),
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
