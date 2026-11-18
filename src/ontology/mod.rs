//! Ontology documents extracted from a read-only source dictionary.

mod diagram;
mod extract;
mod graph;
mod layout;
mod load;
mod merge;
mod mermaid;
mod schema;
mod svg;

pub(crate) use diagram::{
    default_name, diagram, parse_patterns, DiagramReport, DiagramRequest, EdgeCounts,
};
pub use extract::pull;
pub(crate) use extract::split_table;
pub(crate) use graph::Selection;
pub(crate) use load::load_source;
pub use schema::{validate, OntologyDoc, RelationStatus};
