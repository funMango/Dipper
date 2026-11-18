//! Build an ontology diagram from local JSON. This command never opens a database.

use std::path::Path;

use serde::Serialize;

use crate::error::DipperError;
use crate::ontology::graph::{select, SelectError, Selection, TablePattern};
use crate::ontology::layout::layout;
use crate::ontology::load::load_source;
use crate::ontology::mermaid::to_mermaid;
use crate::ontology::svg::to_svg;

pub struct DiagramRequest {
    pub source: String,
    pub selection: Selection,
    pub name: String,
    pub mmd: bool,
}

#[derive(Serialize)]
pub struct EdgeCounts {
    pub confirmed: usize,
    pub draft: usize,
    pub foreign_key: usize,
}

pub struct DiagramReport {
    pub files: Vec<String>,
    pub tables: usize,
    pub missing_tables: usize,
    pub edges: EdgeCounts,
    pub warnings: Vec<String>,
}

pub fn diagram(root: &Path, request: &DiagramRequest) -> Result<DiagramReport, DipperError> {
    validate_name(&request.name)?;
    if request.selection.depth > 5 {
        return Err(usage("depth must be from 0 to 5"));
    }
    let loaded = load_source(root, &request.source)?;
    let docs: Vec<_> = loaded.into_iter().map(|item| item.doc).collect();
    let graph = crate::ontology::graph::build(&docs);
    let selected = select(&graph, &request.selection).map_err(select_error)?;
    let placed = layout(&selected);
    let mut confirmed = 0;
    let mut draft = 0;
    let mut foreign_key = 0;
    for edge in &selected.edges {
        match &edge.source {
            crate::ontology::graph::EdgeSource::ForeignKey { .. } => foreign_key += 1,
            crate::ontology::graph::EdgeSource::Relation {
                status: crate::ontology::schema::RelationStatus::Draft,
            } => draft += 1,
            crate::ontology::graph::EdgeSource::Relation {
                status: crate::ontology::schema::RelationStatus::Confirmed,
            } => confirmed += 1,
        }
    }
    let missing_tables = selected
        .nodes
        .values()
        .filter(|node| matches!(node.kind, crate::ontology::graph::NodeKind::Missing))
        .count();
    let tables = selected.nodes.len() - missing_tables;
    let title = format!(
        "{} · {} · tables {tables} · relations {}",
        request.source,
        root_title(&request.selection),
        selected.edges.len()
    );
    let svg = to_svg(&selected, &placed, &title);
    let mut files = Vec::new();
    let svg_path = root
        .join("ontology/diagrams")
        .join(&request.source)
        .join(format!("{}.svg", request.name));
    write_if_changed(&svg_path, svg.as_bytes())?;
    files.push(relative_path(root, &svg_path));
    if request.mmd {
        let mmd_path = svg_path.with_extension("mmd");
        write_if_changed(&mmd_path, to_mermaid(&selected).as_bytes())?;
        files.push(relative_path(root, &mmd_path));
    }
    let mut warnings = Vec::new();
    if selected.nodes.len() > 80 || selected.edges.len() > 200 {
        warnings.push("large_diagram".to_string());
    }
    Ok(DiagramReport {
        files,
        tables,
        missing_tables,
        edges: EdgeCounts {
            confirmed,
            draft,
            foreign_key,
        },
        warnings,
    })
}

pub fn default_name(roots: &[TablePattern]) -> String {
    let Some(first) = roots.first() else {
        return "all".to_string();
    };
    first
        .raw
        .chars()
        .map(|ch| {
            let lower = ch.to_ascii_lowercase();
            if lower.is_ascii_alphanumeric() || matches!(lower, '.' | '_' | '-') {
                lower
            } else {
                '_'
            }
        })
        .collect()
}

pub fn parse_patterns(values: &[String]) -> Result<Vec<TablePattern>, DipperError> {
    values
        .iter()
        .map(|value| TablePattern::parse(value).map_err(usage))
        .collect()
}

fn validate_name(name: &str) -> Result<(), DipperError> {
    let chars = name.chars().count();
    let safe = !name.is_empty()
        && chars <= 80
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if safe {
        Ok(())
    } else {
        Err(usage(format!("invalid diagram name {name}")))
    }
}

fn root_title(selection: &Selection) -> String {
    if selection.roots.is_empty() {
        "all".to_string()
    } else {
        selection
            .roots
            .iter()
            .map(|pattern| pattern.raw.clone())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn select_error(err: SelectError) -> DipperError {
    match err {
        SelectError::NoRootMatch(pattern) => not_found(format!("no table matches {pattern}")),
        SelectError::BadPattern(message) => usage(message),
    }
}

fn write_if_changed(path: &Path, body: &[u8]) -> Result<(), DipperError> {
    if std::fs::read(path).ok().as_deref() == Some(body) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| other(err.to_string()))?;
    }
    std::fs::write(path, body).map_err(|err| other(err.to_string()))
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn usage(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::Usage,
        source_name: "ontology".to_string(),
        message: message.into(),
    }
}

fn not_found(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::ObjectNotFound,
        source_name: "ontology".to_string(),
        message: message.into(),
    }
}

fn other(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        kind: crate::error::ErrorKind::Other,
        source_name: "ontology".to_string(),
        message: message.into(),
    }
}
